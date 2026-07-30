use super::*;
use super::{
    frame_backstop::{
        run_with_backstop, run_with_backstop_and_admission, InboundDisposition, StanzaBackstop,
        StanzaTimeout,
    },
    parse_errors::{is_sasl_parse_failure, parse_error_responses},
    resource_binding::handle_resource_binding,
    sasl::{
        handle_sasl_oauthbearer, handle_sasl_scram_client_first, handle_sasl_scram_response,
        record_scram_failure,
    },
    state::{InboundFrameTerminal, WsConnState},
    stream_management::{handle_sm_stanza, SmCtx},
    transport_xml::{
        build_stream_features_for_phase, sasl_failure_xml, websocket_stream_close_xml,
        websocket_stream_open_xml,
    },
};
use crate::server::routes::auth_telemetry::AuthFailure;

/// Handle an XMPP frame per RFC 7395
#[cfg(test)]
pub(super) async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> Vec<String> {
    handle_xmpp_frame_impl(frame, domain, state, conn, None).await
}

pub(super) async fn handle_xmpp_frame_with_admission(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Vec<String> {
    handle_xmpp_frame_impl(frame, domain, state, conn, Some((permit, shutdown))).await
}

async fn await_control_stage<T>(
    admission: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
    work: impl std::future::Future<Output = T>,
) -> Result<T, InboundFrameTerminal> {
    // Claim-owning control futures must reach their typed completion boundary;
    // dropping them mid-commit could strand durable ownership. The process
    // boundary is independently hard-bounded by the HTTP drain deadline.
    let output = work.await;
    if let Some((permit, shutdown)) = admission {
        if shutdown.is_cancelled() || permit.revalidate().is_err() {
            return Err(InboundFrameTerminal::AuthorityRevoked);
        }
    }
    Ok(output)
}

async fn handle_xmpp_frame_impl(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
    admission: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
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
        inbound_frame_terminal,
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
        pending_sm_enable_commit,
        state_machine,
        registry_owner,
        ..
    } = conn;
    if let Some((permit, shutdown)) = admission {
        if shutdown.is_cancelled() || permit.revalidate().is_err() {
            *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
            return Vec::new();
        }
    }
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
                pending_sm_enable_commit,
            };
            return match await_control_stage(admission, handle_sm_stanza(sm, state, ctx)).await {
                Ok(responses) => responses,
                Err(terminal) => {
                    *inbound_frame_terminal = Some(terminal);
                    Vec::new()
                }
            };
        }
    }

    let inbound = match parse_frame(frame) {
        Ok(f) => f,
        Err(ParseError::Empty) => return vec![],
        Err(err) => {
            if let Some(responses) = parse_error_responses(frame, &err) {
                if phase.scram_pending_username().is_some() && is_sasl_parse_failure(frame, &err) {
                    record_scram_failure(AuthFailure::ScramMalformed, None);
                    let _ = phase.take_scram_pending();
                }
                warn!(
                    error = %err,
                    len = frame.len(),
                    responses = responses.len(),
                    "Handled XMPP parse error with protocol response"
                );
                return responses;
            }
            warn!(error = %err, len = frame.len(), "Unhandled XMPP frame");
            return vec![];
        }
    };

    match inbound {
        InboundFrame::Open => {
            info!("XMPP stream open requested");
            let open_element = websocket_stream_open_xml(domain);
            let features_element = build_stream_features_for_phase(phase);
            conn.begin_server_stream_open_response();
            vec![open_element, features_element]
        }

        InboundFrame::Close => {
            info!("XMPP stream close requested");
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            // The stream is over: no response header remains to hang a
            // graceful-shutdown <stream:error> on.
            conn.reset_stream_open_for_xmpp_lifecycle();
            vec![websocket_stream_close_xml()]
        }

        InboundFrame::Auth { mechanism, data } => {
            if !phase.allows_sasl_auth() {
                let reset_scram_phase = phase.scram_pending_username().is_some();
                warn!(phase = ?phase, mechanism = %mechanism, "SASL auth received in invalid phase");
                if reset_scram_phase || mechanism == "SCRAM-SHA-256" {
                    record_scram_failure(AuthFailure::ScramOther, None);
                }
                if reset_scram_phase {
                    let _ = phase.take_scram_pending();
                }
                return vec![sasl_failure_xml("not-authorized")];
            }
            let responses = match await_control_stage(admission, async {
                match mechanism.as_str() {
                    "SCRAM-SHA-256" => {
                        handle_sasl_scram_client_first(&data, domain, state, phase).await
                    }
                    "OAUTHBEARER" => {
                        handle_sasl_oauthbearer(&data, state, authenticated_session, phase).await
                    }
                    other => {
                        warn!(mechanism = %other, "Unsupported SASL mechanism");
                        vec![sasl_failure_xml("invalid-mechanism")]
                    }
                }
            })
            .await
            {
                Ok(responses) => responses,
                Err(terminal) => {
                    *inbound_frame_terminal = Some(terminal);
                    return Vec::new();
                }
            };
            // RFC 6120 §6.4.6: SASL success restarts the stream. Until
            // the client's next <open/> is answered, no response header
            // exists for the new stream, so the graceful-shutdown arm
            // must not send a <stream:error> (§4.9.1.2).
            if phase.is_authenticated() {
                conn.reset_stream_open_for_xmpp_lifecycle();
            }
            responses
        }

        InboundFrame::SaslResponse(data) => {
            if !phase.allows_sasl_response() {
                warn!(phase = ?phase, "SASL response received in invalid phase");
                record_scram_failure(AuthFailure::ScramOther, None);
                return vec![sasl_failure_xml("not-authorized")];
            }
            let scram = phase
                .take_scram_pending()
                .expect("SASL response must have pending SCRAM state");
            let responses =
                handle_sasl_scram_response(&data, domain, scram, authenticated_session, phase);
            // Same stream-restart rule as the <auth/> arm above: after
            // SCRAM success no response header exists for the new
            // stream until the next <open/> is answered.
            if phase.is_authenticated() {
                conn.reset_stream_open_for_xmpp_lifecycle();
            }
            responses
        }

        InboundFrame::Stanza(stanza) => {
            // Resource binding is stream setup, not a countable request. Keep
            // it before SM reservation and the ordered-relay lookup so a
            // lifecycle cancellation cannot leave an unsettled inbound slot.
            if let Stanza::Iq(iq) = &*stanza {
                let is_bind = matches!(
                    &**iq,
                    xmpp_parsers::iq::Iq::Set { payload: e, .. }
                        | xmpp_parsers::iq::Iq::Get { payload: e, .. }
                        if e.ns() == waddle_xmpp::ns::BIND
                );
                if is_bind {
                    return handle_resource_binding(iq, domain, phase);
                }
            }

            let reserved_inbound_for_sm = sm_state
                .enabled
                .then(|| sm_inbound_completion.reserve(sm_state));
            let ordered_relay_origin = match await_control_stage(
                admission,
                ordered_relay_origin_for_inbound_stanza(
                    state,
                    sm_state,
                    phase.bound_jid(),
                    registry_owner.as_ref(),
                    reserved_inbound_for_sm,
                    ordered_relay_handoff_tx.as_ref(),
                ),
            )
            .await
            {
                Ok(origin) => origin,
                Err(terminal) => {
                    if let Some(inbound_sequence) = reserved_inbound_for_sm {
                        sm_inbound_completion.abandon(inbound_sequence);
                    }
                    *inbound_frame_terminal = Some(terminal);
                    return Vec::new();
                }
            };

            // #808: capture the conformant-reply metadata before the stanza is
            // moved into the dispatch future, then run dispatch under the
            // per-connection wedge backstop. A single slow/wedged handler can no
            // longer freeze the connection's frame loop indefinitely; on elapse
            // an IQ get/set gets a conformant resource-constraint/wait error and
            // message/presence are dropped (logged + metered).
            let backstop = StanzaBackstop::capture(&stanza, phase.bound_jid());
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
            let (dispatch_result, authority_revoked_after_start) = match admission {
                Some((permit, shutdown)) => {
                    let result =
                        run_with_backstop_and_admission(backstop, dispatch, permit, shutdown).await;
                    (result.result, result.authority_revoked_after_start)
                }
                None => (run_with_backstop(backstop, dispatch).await, false),
            };
            if authority_revoked_after_start {
                *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
            }
            let (responses, disposition) = match dispatch_result {
                Ok(responses) => (responses, InboundDisposition::Handled),
                Err(StanzaTimeout::HandledIq(reply)) => {
                    (vec![element_to_xml(reply)], InboundDisposition::Handled)
                }
                Err(StanzaTimeout::Unhandled) => (Vec::new(), InboundDisposition::Unhandled),
                Err(StanzaTimeout::AdmissionRevoked) => {
                    *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
                    (Vec::new(), InboundDisposition::Unhandled)
                }
            };
            settle_inbound_dispatch(
                disposition,
                ordered_relay_origin_was_deferred(&ordered_relay_origin),
                reserved_inbound_for_sm,
                sm_inbound_completion,
                sm_state,
            );
            responses
        }
    }
}

pub(super) fn settle_inbound_dispatch(
    disposition: InboundDisposition,
    ordered_relay_deferred: bool,
    inbound_sequence: Option<crate::server::routes::interpret::OrderedRelayInboundSequence>,
    completion: &mut crate::server::routes::interpret::SmInboundCompletionTracker,
    sm_state: &mut waddle_xmpp::stream_management::StreamManagementState,
) {
    let Some(inbound_sequence) = inbound_sequence else {
        return;
    };
    match disposition {
        InboundDisposition::Handled => {
            if !ordered_relay_deferred {
                completion.complete(inbound_sequence, sm_state);
            }
        }
        InboundDisposition::Unhandled => {
            // A timeout cancels dispatch. Remove the slot from the set cleanup
            // waits for, but preserve a permanent `h` hole so responsibility
            // stays with the sender. This overrides ordered-relay deferral: a
            // late handoff must not convert a cancelled dispatch into an ack.
            completion.abandon(inbound_sequence);
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

#[cfg(test)]
mod inbound_dispatch_tests {
    use super::*;

    fn enabled_sm() -> waddle_xmpp::stream_management::StreamManagementState {
        let mut state = waddle_xmpp::stream_management::StreamManagementState::new();
        state.enable("dispatch-test".to_string(), true, Some(300));
        state
    }

    #[test]
    fn handled_dispatch_advances_h_unless_ordered_relay_owns_completion() {
        let mut state = enabled_sm();
        let mut completion =
            crate::server::routes::interpret::SmInboundCompletionTracker::default();
        let sequence = completion.reserve(&state);

        settle_inbound_dispatch(
            InboundDisposition::Handled,
            false,
            Some(sequence),
            &mut completion,
            &mut state,
        );

        assert_eq!(state.get_inbound_count(), 1);

        let deferred = completion.reserve(&state);
        settle_inbound_dispatch(
            InboundDisposition::Handled,
            true,
            Some(deferred),
            &mut completion,
            &mut state,
        );
        assert_eq!(state.get_inbound_count(), 1);
        assert!(completion.has_pending());
    }
}
