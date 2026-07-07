use super::*;
use super::{
    frame_backstop::{run_with_backstop, StanzaBackstop},
    isr_resume::handle_isr_resume_authenticate,
    parse_errors::{is_sasl_parse_failure, parse_error_responses},
    resource_binding::handle_resource_binding,
    sasl::{handle_sasl_oauthbearer, handle_sasl_scram_client_first, handle_sasl_scram_response},
    state::WsConnState,
    stream_management::{handle_sm_stanza, SmCtx},
    transport_xml::{
        build_stream_features_for_phase, sasl_failure_xml, websocket_stream_close_xml,
        websocket_stream_open_xml,
    },
};

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
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        roster_interested,
        blocklist_interested,
        pending_resume_stream_id,
        pending_resume_h,
        suppress_sm_record_next_batch,
        state_machine,
        stream_open_sent,
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
                if phase.scram_pending_username().is_some() && is_sasl_parse_failure(frame, &err) {
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
            let isr_available = state
                .deps
                .app_state
                .clustering_claims
                .isr_token_store()
                .is_some();
            let features_element = build_stream_features_for_phase(phase, isr_available);
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

        InboundFrame::Auth { mechanism, data } => {
            if !phase.allows_sasl_auth() {
                let reset_scram_phase = phase.scram_pending_username().is_some();
                warn!(phase = ?phase, mechanism = %mechanism, "SASL auth received in invalid phase");
                if reset_scram_phase {
                    let _ = phase.take_scram_pending();
                }
                return vec![sasl_failure_xml("not-authorized")];
            }
            let responses = match mechanism.as_str() {
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
                warn!(phase = ?phase, "ISR resume authenticate received in invalid phase");
                return vec![sasl_failure_xml("not-authorized")];
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
                warn!(phase = ?phase, "SASL response received in invalid phase");
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
                *stream_open_sent = false;
            }
            responses
        }

        InboundFrame::Stanza(stanza) => {
            // Count inbound stanzas for XEP-0198: iq/message/presence always
            // count; SM control nonzas and framing elements are excluded above.
            if sm_state.enabled {
                sm_state.increment_inbound();
            }

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
                    return handle_resource_binding(iq, domain, phase);
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
                            state_machine: state_machine.as_mut(),
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
                        handlers::presence::handle_presence(
                            presence,
                            domain,
                            &muc_domain,
                            state,
                            phase,
                            authenticated_session,
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
                        )
                        .await
                    }
                }
            };
            run_with_backstop(backstop, dispatch).await
        }
    }
}
