use super::*;
use super::{
    frame_backstop::{run_with_backstop, StanzaBackstop},
    isr_resume::handle_isr_resume_authenticate,
    parse_errors::{is_sasl_auth_parse_failure, is_sasl_parse_failure, parse_error_responses},
    resource_binding::handle_resource_binding,
    sasl::{
        handle_sasl_oauthbearer_error_response, handle_sasl_oauthbearer_initial,
        handle_sasl_oauthbearer_response, handle_sasl_scram_client_first,
        handle_sasl_scram_response,
    },
    state::WsConnState,
    stream_management::{handle_sm_stanza, SmCtx},
    telemetry_privacy::{connection_phase_category, parse_error_category, sasl_mechanism_category},
    transport_xml::{
        build_stream_features_for_phase, sasl_failure_xml, websocket_stream_close_xml,
        websocket_stream_open_xml, SaslFailureCondition,
    },
};
use waddle_xmpp::auth::SaslMechanism;
use waddle_xmpp::prometheus::{
    increment_auth_terminal_attempt, AuthMechanism, AuthTerminalOutcome,
};

fn pending_auth_mechanism(phase: &ConnectionPhase) -> Option<AuthMechanism> {
    if phase.has_pending_oauthbearer_exchange() {
        Some(AuthMechanism::OAuthBearer)
    } else if phase.scram_pending_username().is_some() {
        Some(AuthMechanism::ScramSha256)
    } else {
        None
    }
}

fn metric_auth_mechanism(mechanism: &SaslMechanism) -> Option<AuthMechanism> {
    match mechanism {
        SaslMechanism::OAuthBearer => Some(AuthMechanism::OAuthBearer),
        SaslMechanism::ScramSha256 => Some(AuthMechanism::ScramSha256),
        SaslMechanism::Plain | SaslMechanism::Unsupported => None,
    }
}

fn record_terminal(state: &WebSocketState, mechanism: AuthMechanism, outcome: AuthTerminalOutcome) {
    if mechanism == AuthMechanism::OAuthBearer {
        state
            .deps
            .oauth_terminal_recorder
            .record(mechanism, outcome);
    } else {
        increment_auth_terminal_attempt(mechanism, outcome);
    }
}

/// Handle an XMPP frame per RFC 7395
pub(super) async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> Vec<String> {
    handle_xmpp_frame_impl(frame, domain, state, conn).await
}

async fn handle_xmpp_frame_impl(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> Vec<String> {
    if frame.len() > MAX_FRAME_SIZE {
        warn!(len = frame.len(), "Dropping oversized XMPP frame");
        return vec![];
    }

    let WsConnState {
        phase,
        authenticated_session,
        sm_state,
        sm_inbound_completion,
        ordered_relay_handoff_tx,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        presence_payloads,
        pending_subscribes_flushed,
        roster_interested,
        blocklist_interested,
        pending_resume_stream_id,
        pending_resume_h,
        suppress_sm_record_next_batch,
        state_machine,
        stream_open_sent,
        registry_owner,
        ..
    } = conn;
    let muc_domain = state.deps.service_domains.muc.clone();

    // SM nonzas (enable/resume/r/a) are not part of the parse_frame typed
    // vocabulary — keep the direct SmStanza check before parse_frame.
    if SmStanza::is_client_nonza_candidate(frame) {
        if let Some(sm) = SmStanza::parse(frame) {
            let ctx = SmCtx {
                phase,
                sm_state,
                authenticated_session,
                carbons_enabled,
                presence_available,
                presence_show,
                presence_status,
                presence_priority,
                presence_payloads,
                pending_subscribes_flushed,
                pending_resume_stream_id,
                pending_resume_h,
                suppress_sm_record_next_batch,
                roster_interested,
                blocklist_interested,
            };
            return handle_sm_stanza(sm, state, ctx).await;
        }
    }

    let inbound = match parse_frame(frame) {
        Ok(f) => f,
        Err(ParseError::Empty) => return vec![],
        Err(err) => {
            if let Some(responses) = parse_error_responses(frame, &err) {
                let initial_response_mechanism = match &err {
                    ParseError::InvalidSaslInitialResponseEncoding { mechanism } => {
                        metric_auth_mechanism(mechanism)
                    }
                    _ => None,
                };
                if phase.has_pending_sasl_exchange() && is_sasl_auth_parse_failure(frame, &err) {
                    if let Some(mechanism) = pending_auth_mechanism(phase) {
                        record_terminal(state, mechanism, AuthTerminalOutcome::Cancelled);
                    }
                    phase.reset_pending_sasl_exchange();
                    if let Some(mechanism) = initial_response_mechanism {
                        record_terminal(state, mechanism, AuthTerminalOutcome::Malformed);
                    }
                } else if phase.has_pending_sasl_exchange() && is_sasl_parse_failure(frame, &err) {
                    if let Some(mechanism) = pending_auth_mechanism(phase) {
                        record_terminal(state, mechanism, AuthTerminalOutcome::Malformed);
                    }
                    phase.reset_pending_sasl_exchange();
                } else if let Some(mechanism) = initial_response_mechanism {
                    record_terminal(state, mechanism, AuthTerminalOutcome::Malformed);
                }
                warn!(
                    category = parse_error_category(&err),
                    len = frame.len(),
                    responses = responses.len(),
                    "Handled XMPP parse error with protocol response"
                );
                return responses;
            }
            warn!(
                category = parse_error_category(&err),
                len = frame.len(),
                "Unhandled XMPP frame"
            );
            return vec![];
        }
    };

    match inbound {
        InboundFrame::Open => {
            info!("XMPP stream open requested");
            let open_element = websocket_stream_open_xml(domain);
            let isr_available = state
                .deps
                .app_state
                .clustering_claims
                .isr_token_store()
                .is_some();
            let features_element = build_stream_features_for_phase(
                phase,
                isr_available,
                state.deps.oauthbearer_available,
            );
            *stream_open_sent = true;
            vec![open_element, features_element]
        }

        InboundFrame::Close => {
            info!("XMPP stream close requested");
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            // The stream is over: no response header remains to hang a
            // graceful-shutdown <stream:error> on.
            *stream_open_sent = false;
            vec![websocket_stream_close_xml()]
        }

        InboundFrame::Auth {
            mechanism,
            initial_response,
        } => {
            // RFC 6120 section 6.4.2: a subsequent <auth/> replaces an
            // unfinished SASL handshake. Discard the old exchange first, then
            // process this frame normally instead of rejecting it as an
            // out-of-phase request.
            if phase.has_pending_sasl_exchange() {
                if let Some(pending_mechanism) = pending_auth_mechanism(phase) {
                    record_terminal(state, pending_mechanism, AuthTerminalOutcome::Cancelled);
                }
                phase.reset_pending_sasl_exchange();
            }
            if !phase.allows_sasl_auth() {
                warn!(
                    phase = connection_phase_category(phase),
                    mechanism = sasl_mechanism_category(&mechanism),
                    "SASL auth received in invalid phase"
                );
                return vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)];
            }
            let responses = match mechanism {
                SaslMechanism::ScramSha256 => {
                    handle_sasl_scram_client_first(&initial_response, domain, state, phase).await
                }
                SaslMechanism::OAuthBearer if state.deps.oauthbearer_available => {
                    handle_sasl_oauthbearer_initial(
                        &initial_response,
                        state,
                        authenticated_session,
                        phase,
                    )
                    .await
                }
                other => {
                    warn!(
                        mechanism = sasl_mechanism_category(&other),
                        "Unsupported or unavailable SASL mechanism"
                    );
                    vec![sasl_failure_xml(SaslFailureCondition::InvalidMechanism)]
                }
            };
            // RFC 6120 §6.4.6: SASL success restarts the stream. Until
            // the client's next <open/> is answered, no response header
            // exists for the new stream, so the graceful-shutdown arm
            // must not send a <stream:error> (§4.9.1.2).
            if phase.is_authenticated() {
                *stream_open_sent = false;
            }
            responses
        }

        InboundFrame::IsrResumeAuthenticate {
            mechanism,
            initial_response,
            resume,
        } => {
            // ADR-0017 Phase 3 Slice 8: XEP-0397 ISR resume, like SASL
            // <auth>/<response> above, is only legal before this transport
            // has its own SASL/bind lifecycle already established — it
            // performs authentication itself, inline.
            if !phase.allows_sasl_auth() {
                warn!(
                    phase = connection_phase_category(phase),
                    "ISR resume authenticate received in invalid phase"
                );
                phase.reset_pending_sasl_exchange();
                return vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)];
            }
            let ctx = SmCtx {
                phase,
                sm_state,
                authenticated_session,
                carbons_enabled,
                presence_available,
                presence_show,
                presence_status,
                presence_priority,
                presence_payloads,
                pending_subscribes_flushed,
                pending_resume_stream_id,
                pending_resume_h,
                suppress_sm_record_next_batch,
                roster_interested,
                blocklist_interested,
            };
            let responses =
                handle_isr_resume_authenticate(mechanism, initial_response, resume, state, ctx)
                    .await;
            // RFC 6120 §6.4.6 / XEP-0388: successful authentication restarts
            // the stream — same rule the SASL1 <auth>/<response> arms apply.
            if phase.is_authenticated() {
                *stream_open_sent = false;
            }
            responses
        }

        InboundFrame::SaslResponse(data) => {
            if !phase.allows_sasl_response() {
                warn!(
                    phase = connection_phase_category(phase),
                    "SASL response received in invalid phase"
                );
                return vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)];
            }
            let responses = if phase.take_oauthbearer_error_pending() {
                handle_sasl_oauthbearer_error_response(&data, state)
            } else if phase.take_oauthbearer_initial_response_pending() {
                handle_sasl_oauthbearer_response(&data, state, authenticated_session, phase).await
            } else {
                let scram = phase
                    .take_scram_pending()
                    .expect("SASL response must have a typed pending exchange");
                handle_sasl_scram_response(&data, domain, scram, authenticated_session, phase)
            };
            // Same stream-restart rule as the <auth/> arm above: after
            // SCRAM success no response header exists for the new
            // stream until the next <open/> is answered.
            if phase.is_authenticated() {
                *stream_open_sent = false;
            }
            responses
        }

        InboundFrame::SaslAbort => {
            if !phase.has_pending_sasl_exchange() {
                warn!(
                    phase = connection_phase_category(phase),
                    "SASL abort received without a pending exchange"
                );
                return vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)];
            }
            let cancelled_mechanism = pending_auth_mechanism(phase);
            phase.reset_pending_sasl_exchange();
            if let Some(mechanism) = cancelled_mechanism {
                record_terminal(state, mechanism, AuthTerminalOutcome::Cancelled);
            }
            vec![sasl_failure_xml(SaslFailureCondition::Aborted)]
        }

        InboundFrame::Stanza(stanza) => {
            let reserved_inbound_for_sm = sm_state
                .enabled
                .then(|| sm_inbound_completion.reserve(sm_state));
            let ordered_relay_origin = ordered_relay_origin_for_inbound_stanza(
                state,
                sm_state,
                phase.bound_jid(),
                registry_owner.as_ref(),
                reserved_inbound_for_sm,
                ordered_relay_handoff_tx.as_ref(),
            )
            .await;

            // Resource binding is stream setup, not request processing: handle
            // it inline and return BEFORE the wedge backstop (#808 ADR-008 scope
            // guard). It must never be subject to, or delayed by, the timeout.
            if let Stanza::Iq(iq) = &*stanza {
                let is_bind = matches!(
                    &**iq,
                    xmpp_parsers::iq::Iq::Set { payload: e, .. }
                        | xmpp_parsers::iq::Iq::Get { payload: e, .. }
                        if e.ns() == waddle_xmpp::ns::BIND
                );
                if is_bind {
                    let responses = handle_resource_binding(iq, domain, phase);
                    if let Some(inbound_sequence) = reserved_inbound_for_sm {
                        sm_inbound_completion.complete(inbound_sequence, sm_state);
                    }
                    return responses;
                }
            }

            // #808: capture the conformant-reply metadata before the stanza is
            // moved into the dispatch future, then run dispatch under the
            // per-connection wedge backstop. A single slow/wedged handler can no
            // longer freeze the connection's frame loop indefinitely; on elapse
            // an IQ get/set gets a conformant resource-constraint/wait error and
            // message/presence are dropped (logged + metered).
            let backstop = StanzaBackstop::capture(&stanza);
            let dispatch = async {
                match *stanza {
                    Stanza::Iq(iq) => {
                        let mut iq_conn_state = handlers::iq::IqConnState {
                            carbons_enabled,
                            roster_interested,
                            blocklist_interested,
                            registry_owner: registry_owner.as_ref(),
                            state_machine: state_machine.as_mut(),
                            ordered_relay_origin: ordered_relay_origin.clone(),
                        };
                        handlers::iq::handle_iq_with_conn_state(
                            *iq,
                            domain,
                            &muc_domain,
                            state,
                            authenticated_session,
                            phase,
                            &mut iq_conn_state,
                        )
                        .await
                    }

                    Stanza::Presence(presence) => {
                        handlers::presence::handle_presence_with_ordered_relay(
                            presence,
                            handlers::presence::PresenceHandlerContext {
                                domain,
                                muc_domain: &muc_domain,
                                state,
                                phase,
                                authenticated_session,
                                registry_owner: registry_owner.as_ref(),
                                ordered_relay_origin: ordered_relay_origin.clone(),
                            },
                        )
                        .await
                    }

                    Stanza::Message(message) => {
                        handlers::message::handle_message(
                            message,
                            state,
                            phase,
                            state_machine.as_mut(),
                            authenticated_session.as_ref(),
                            ordered_relay_origin.clone(),
                        )
                        .await
                    }
                }
            };
            let responses = run_with_backstop(backstop, dispatch).await;
            if let Some(inbound_sequence) = reserved_inbound_for_sm {
                if !ordered_relay_origin_was_deferred(&ordered_relay_origin) {
                    sm_inbound_completion.complete(inbound_sequence, sm_state);
                }
            }
            responses
        }
    }
}

#[cfg(feature = "clustering")]
async fn ordered_relay_origin_for_inbound_stanza(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    inbound_sequence: Option<crate::server::routes::interpret::OrderedRelayInboundSequence>,
    handoff_tx: Option<
        &tokio::sync::mpsc::UnboundedSender<
            crate::server::routes::interpret::OrderedRelayHandoffCompletion,
        >,
    >,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let sender_entity = bound_jid.map(|jid| {
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            jid.to_bare().to_string(),
        )
    })?;
    if let (Some(jid), Some(owner), Some(bridge)) = (
        bound_jid,
        registry_owner,
        state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref(),
    ) {
        if let Some(remote) = bridge.remote_resource_origin_if_owner(jid, owner).await {
            return Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
                kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::RemoteResource(
                    remote,
                ),
                sender_entity,
                inbound_sequence: inbound_sequence.map(|sequence| sequence.0).unwrap_or(0),
                handoff: if sm_state.enabled {
                    inbound_sequence.and_then(|sequence| {
                        handoff_tx.map(|tx| {
                            crate::server::routes::interpret::OrderedRelayHandoffHandle::new(
                                sequence,
                                tx.clone(),
                            )
                        })
                    })
                } else {
                    None
                },
            });
        }
    }
    if sm_state.enabled {
        let stream_id = sm_state.stream_id.as_ref()?;
        let inbound_sequence = inbound_sequence?;
        return Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
            kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::SmSession(
                waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            ),
            sender_entity,
            inbound_sequence: inbound_sequence.0,
            handoff: handoff_tx.map(|tx| {
                crate::server::routes::interpret::OrderedRelayHandoffHandle::new(
                    inbound_sequence,
                    tx.clone(),
                )
            }),
        });
    }
    Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
        kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
            sender_entity.clone(),
        ),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    })
}

#[cfg(not(feature = "clustering"))]
async fn ordered_relay_origin_for_inbound_stanza(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    inbound_sequence: Option<crate::server::routes::interpret::OrderedRelayInboundSequence>,
    handoff_tx: Option<
        &tokio::sync::mpsc::UnboundedSender<
            crate::server::routes::interpret::OrderedRelayHandoffCompletion,
        >,
    >,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let _ = (
        state,
        sm_state,
        bound_jid,
        registry_owner,
        inbound_sequence,
        handoff_tx,
    );
    None
}

#[cfg(feature = "clustering")]
fn ordered_relay_origin_was_deferred(
    origin: &Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    origin
        .as_ref()
        .and_then(|origin| origin.handoff.as_ref())
        .is_some_and(|handoff| handoff.was_deferred())
}

#[cfg(not(feature = "clustering"))]
fn ordered_relay_origin_was_deferred(
    origin: &Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    let _ = origin;
    false
}

#[cfg(feature = "clustering")]
pub(super) fn ordered_relay_origin_from_sm(
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let sender_entity = bound_jid.map(|jid| {
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            jid.to_bare().to_string(),
        )
    })?;
    if sm_state.enabled {
        let stream_id = sm_state.stream_id.as_ref()?;
        return Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
            kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::SmSession(
                waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            ),
            sender_entity,
            inbound_sequence: sm_state.get_inbound_count(),
            handoff: None,
        });
    }
    Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
        kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
            sender_entity.clone(),
        ),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    })
}

#[cfg(not(feature = "clustering"))]
pub(super) fn ordered_relay_origin_from_sm(
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let _ = (sm_state, bound_jid);
    None
}

#[cfg(test)]
mod sasl_abort_metric_tests {
    use super::*;

    #[test]
    fn pending_sasl_exchanges_map_to_their_cancelled_mechanism() {
        let scram = ConnectionPhase::scram_pending(ScramPendingState::new(
            ScramServer::new(),
            vec![1],
            vec![2],
            "alice",
        ));
        assert_eq!(
            pending_auth_mechanism(&scram),
            Some(AuthMechanism::ScramSha256)
        );
        for oauth in [
            ConnectionPhase::oauthbearer_initial_response_pending(),
            ConnectionPhase::oauthbearer_error_pending(),
        ] {
            assert_eq!(
                pending_auth_mechanism(&oauth),
                Some(AuthMechanism::OAuthBearer)
            );
        }
        assert_eq!(pending_auth_mechanism(&ConnectionPhase::new()), None);
    }
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;

    #[test]
    fn non_sm_peer_side_route_uses_user_actor_origin() {
        let sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        let bound: jid::FullJid = "romeo.test/phone".parse().expect("full jid");

        let origin = ordered_relay_origin_from_sm(&sm_state, Some(&bound)).expect("non-SM origin");

        assert_eq!(origin.inbound_sequence, 0);
        assert!(origin.handoff.is_none());
        assert_eq!(
            origin.kind,
            crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
                waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::UserActor,
                    "romeo.test"
                )
            )
        );
    }
}
