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
        build_handled_count_too_high_stream_error, build_stream_features_for_phase_element,
        element_to_xml, sasl_failure_element, stanza_to_xml, websocket_stream_close_element,
        websocket_stream_open_element,
    },
};
use crate::room_effect_outbox::drain::RoomEffectCompletion;
use crate::server::routes::auth_telemetry::AuthFailure;
use crate::server::routes::interpret::ParkedIngressShadowSubmission;
#[cfg(any(feature = "clustering", test))]
use jid::BareJid;
use std::str::FromStr;
use xmpp_parsers::message::Lang;
use xmpp_parsers::minidom::Element;

#[derive(Debug, Clone)]
pub(crate) enum StreamErrorFrame {
    HandledCountTooHigh { acknowledged: u32, send_count: u32 },
}

impl StreamErrorFrame {
    fn into_serialized_xml(self) -> String {
        match self {
            Self::HandledCountTooHigh {
                acknowledged,
                send_count,
            } => build_handled_count_too_high_stream_error(acknowledged, send_count),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResponseFrame {
    /// Typed stanza written at the websocket transport boundary.
    Stanza(Box<Stanza>),
    /// Typed non-stanza XML written at the websocket transport boundary.
    Element(Element),
    /// Typed transport-only stream error emitted by the websocket adapter.
    StreamError(StreamErrorFrame),
}

impl ResponseFrame {
    pub(crate) fn into_serialized_xml(self) -> String {
        match self {
            Self::Stanza(stanza) => stanza_to_xml(&stanza),
            Self::Element(element) => element_to_xml(element),
            Self::StreamError(error) => error.into_serialized_xml(),
        }
    }

    pub(crate) fn is_websocket_stream_close(&self) -> bool {
        match self {
            Self::Stanza(_) => false,
            Self::Element(element) => {
                element.name() == "close" && element.ns() == "urn:ietf:params:xml:ns:xmpp-framing"
            }
            Self::StreamError(_) => false,
        }
    }

    pub(crate) fn from_serialized_xml(xml: String) -> Self {
        let element = Element::from_str(xml.trim_start())
            .expect("server-authored response batch frame must be valid XML");
        match element.name() {
            "message" => xmpp_parsers::message::Message::try_from(element.clone())
                .map(Stanza::Message)
                .map(Into::into)
                .unwrap_or_else(|_| Self::Element(element)),
            "iq" => xmpp_parsers::iq::Iq::try_from(element.clone())
                .map(|iq| Stanza::Iq(Box::new(iq)))
                .map(Into::into)
                .unwrap_or_else(|_| Self::Element(element)),
            "presence" => xmpp_parsers::presence::Presence::try_from(element.clone())
                .map(Stanza::Presence)
                .map(Into::into)
                .unwrap_or_else(|_| Self::Element(element)),
            _ => Self::Element(element),
        }
    }
}

impl From<String> for ResponseFrame {
    fn from(xml: String) -> Self {
        Self::from_serialized_xml(xml)
    }
}

impl From<Stanza> for ResponseFrame {
    fn from(stanza: Stanza) -> Self {
        Self::Stanza(Box::new(stanza))
    }
}

impl From<Box<Stanza>> for ResponseFrame {
    fn from(stanza: Box<Stanza>) -> Self {
        Self::Stanza(stanza)
    }
}

impl From<Element> for ResponseFrame {
    fn from(element: Element) -> Self {
        Self::Element(element)
    }
}

impl From<StreamErrorFrame> for ResponseFrame {
    fn from(error: StreamErrorFrame) -> Self {
        Self::StreamError(error)
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ResponseBatch {
    pub(super) frames: Vec<ResponseFrame>,
    pub(super) completions: Vec<RoomEffectCompletion>,
    pub(super) completion_frame_indices: Vec<usize>,
}

impl ResponseBatch {
    pub(super) fn from_frames<I, F>(frames: I) -> Self
    where
        I: IntoIterator<Item = F>,
        F: Into<ResponseFrame>,
    {
        Self {
            frames: frames.into_iter().map(Into::into).collect(),
            completions: Vec::new(),
            completion_frame_indices: Vec::new(),
        }
    }

    pub(super) fn push_completion_frame(
        &mut self,
        frame: impl Into<ResponseFrame>,
        completion: RoomEffectCompletion,
    ) {
        self.completion_frame_indices.push(self.frames.len());
        self.frames.push(frame.into());
        self.completions.push(completion);
    }

    pub(super) fn prepend_frames<I, F>(&mut self, prefix: I)
    where
        I: IntoIterator<Item = F>,
        F: Into<ResponseFrame>,
    {
        let mut prefix: Vec<ResponseFrame> = prefix.into_iter().map(Into::into).collect();
        if prefix.is_empty() {
            return;
        }
        let offset = prefix.len();
        prefix.append(&mut self.frames);
        self.frames = prefix;
        for frame_index in &mut self.completion_frame_indices {
            *frame_index += offset;
        }
    }

    pub(super) fn append_batch(&mut self, mut other: Self) {
        let offset = self.frames.len();
        self.frames.append(&mut other.frames);
        self.completions.append(&mut other.completions);
        self.completion_frame_indices.extend(
            other
                .completion_frame_indices
                .into_iter()
                .map(|frame_index| frame_index + offset),
        );
    }

    #[cfg(test)]
    pub(super) fn from_completion_frames(
        completion_frames: Vec<(ResponseFrame, RoomEffectCompletion)>,
    ) -> Self {
        let mut batch = Self::default();
        for (frame, completion) in completion_frames {
            batch.push_completion_frame(frame, completion);
        }
        batch
    }

    #[cfg(test)]
    pub(crate) fn into_serialized_frames(self) -> Vec<String> {
        self.frames
            .into_iter()
            .map(ResponseFrame::into_serialized_xml)
            .collect()
    }
}

impl From<Vec<String>> for ResponseBatch {
    fn from(frames: Vec<String>) -> Self {
        Self::from_frames(frames.into_iter().map(ResponseFrame::from_serialized_xml))
    }
}

impl From<Vec<Element>> for ResponseBatch {
    fn from(frames: Vec<Element>) -> Self {
        Self::from_frames(frames)
    }
}

impl From<Vec<Stanza>> for ResponseBatch {
    fn from(frames: Vec<Stanza>) -> Self {
        Self::from_frames(frames)
    }
}

impl From<Vec<ResponseFrame>> for ResponseBatch {
    fn from(frames: Vec<ResponseFrame>) -> Self {
        Self::from_frames(frames)
    }
}

/// Handle an XMPP frame per RFC 7395
#[cfg(test)]
pub(super) async fn handle_xmpp_frame(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> Vec<String> {
    handle_xmpp_frame_impl(frame, domain, state, conn, None)
        .await
        .into_serialized_frames()
}

pub(super) async fn handle_xmpp_frame_with_admission(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> ResponseBatch {
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

fn ingress_effect_capture_for_stanza(
    state: &WebSocketState,
    stanza_lang: Option<Lang>,
    stanza: &Stanza,
) -> Option<crate::ingress_shadow::IngressEffectCapture> {
    if !state.deps.protocol.ingress_shadow.is_enabled() {
        return None;
    }
    let Stanza::Message(message) = stanza else {
        return None;
    };
    let capture = crate::ingress_shadow::IngressEffectCapture::new(stanza_lang);
    if let Some(room_fence) = shadow_room_fence_for_message(state, message) {
        capture.record_room_fence(room_fence);
    }
    Some(capture)
}

#[cfg(not(feature = "clustering"))]
fn shadow_room_fence_for_message(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
) -> Option<crate::ingress_shadow::IngressShadowRoomFence> {
    let _ = (state, message);
    None
}

#[cfg(feature = "clustering")]
fn shadow_room_fence_for_message(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
) -> Option<crate::ingress_shadow::IngressShadowRoomFence> {
    let room = shadow_room_scope(message, &state.deps.service_domains.muc)?;
    let store = state
        .deps
        .app_state
        .clustering_claims
        .muc_durable_store
        .as_ref()?;
    let fence = store.current_claim_fence(&room)?;
    Some(crate::ingress_shadow::IngressShadowRoomFence::from_context(
        &room, &fence,
    ))
}

#[cfg(any(feature = "clustering", test))]
fn shadow_room_scope(
    message: &xmpp_parsers::message::Message,
    muc_domain: &str,
) -> Option<BareJid> {
    let to = message.to.as_ref()?;
    let room = to.to_bare();
    if room.domain().as_str() != muc_domain {
        return None;
    }
    if to.resource().is_some() || message.type_ == xmpp_parsers::message::MessageType::Groupchat {
        return Some(room);
    }
    message
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
        .and_then(|payload| {
            (payload
                .get_child("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                .is_some()
                || payload
                    .get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .is_some())
            .then_some(room)
        })
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
) -> ResponseBatch {
    if frame.len() > MAX_FRAME_SIZE {
        warn!(len = frame.len(), "Dropping oversized XMPP frame");
        return ResponseBatch::default();
    }

    let WsConnState {
        phase,
        authenticated_session,
        occupancy_session,
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
        pending_resume_claim,
        pending_finalized_resume_outcome,
        #[cfg(test)]
        pre_final_principal_recheck_test_hook,
        suppress_sm_record_next_batch,
        pending_sm_enable_commit,
        state_machine,
        registry_owner,
        ..
    } = conn;
    if let Some((permit, shutdown)) = admission {
        if shutdown.is_cancelled() || permit.revalidate().is_err() {
            *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
            return ResponseBatch::default();
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
                occupancy_session,
                carbons_enabled,
                presence_available,
                presence_show,
                presence_status,
                presence_priority,
                presence_payloads,
                pending_subscribes_flushed,
                pending_resume_stream_id,
                pending_resume_h,
                pending_resume_claim,
                #[cfg(test)]
                pre_final_principal_recheck_test_hook,
                suppress_sm_record_next_batch,
                roster_interested,
                blocklist_interested,
                pending_sm_enable_commit,
            };
            return match await_control_stage(
                admission,
                handle_sm_stanza(sm, state, ctx, pending_finalized_resume_outcome),
            )
            .await
            {
                Ok(responses) => responses.into(),
                Err(terminal) => {
                    *inbound_frame_terminal = Some(terminal);
                    ResponseBatch::default()
                }
            };
        }
    }

    let parsed = match waddle_xmpp::protocol::frame::parse_frame_with_metadata(frame) {
        Ok(parsed) => parsed,
        Err(ParseError::Empty) => return ResponseBatch::default(),
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
                return responses.into();
            }
            warn!(error = %err, len = frame.len(), "Unhandled XMPP frame");
            return ResponseBatch::default();
        }
    };
    let stanza_lang = parsed.message_stanza_lang;

    match parsed.frame {
        InboundFrame::Open => {
            info!("XMPP stream open requested");
            let open_element = websocket_stream_open_element(domain);
            let features_element = build_stream_features_for_phase_element(phase);
            conn.begin_server_stream_open_response();
            vec![open_element, features_element].into()
        }

        InboundFrame::Close => {
            info!("XMPP stream close requested");
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            // The stream is over: no response header remains to hang a
            // graceful-shutdown <stream:error> on.
            conn.reset_stream_open_for_xmpp_lifecycle();
            vec![websocket_stream_close_element()].into()
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
                return vec![sasl_failure_element("not-authorized")].into();
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
                        vec![sasl_failure_element("invalid-mechanism")]
                    }
                }
            })
            .await
            {
                Ok(responses) => responses,
                Err(terminal) => {
                    *inbound_frame_terminal = Some(terminal);
                    return ResponseBatch::default();
                }
            };
            // RFC 6120 §6.4.6: SASL success restarts the stream. Until
            // the client's next <open/> is answered, no response header
            // exists for the new stream, so the graceful-shutdown arm
            // must not send a <stream:error> (§4.9.1.2).
            if phase.is_authenticated() {
                conn.reset_stream_open_for_xmpp_lifecycle();
            }
            responses.into()
        }

        InboundFrame::SaslResponse(data) => {
            if !phase.allows_sasl_response() {
                warn!(phase = ?phase, "SASL response received in invalid phase");
                record_scram_failure(AuthFailure::ScramOther, None);
                return vec![sasl_failure_element("not-authorized")].into();
            }
            let scram = phase
                .take_scram_pending()
                .expect("SASL response must have pending SCRAM state");
            let responses = handle_sasl_scram_response(
                &data,
                domain,
                state,
                scram,
                authenticated_session,
                phase,
            )
            .await;
            // Same stream-restart rule as the <auth/> arm above: after
            // SCRAM success no response header exists for the new
            // stream until the next <open/> is answered.
            if phase.is_authenticated() {
                conn.reset_stream_open_for_xmpp_lifecycle();
            }
            responses.into()
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
                    return handle_resource_binding(iq, domain, phase).into();
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
                    return ResponseBatch::default();
                }
            };

            // #808: capture the conformant-reply metadata before the stanza is
            // moved into the dispatch future, then run dispatch under the
            // per-connection wedge backstop. A single slow/wedged handler can no
            // longer freeze the connection's frame loop indefinitely; on elapse
            // an IQ get/set gets a conformant resource-constraint/wait error and
            // message/presence are dropped (logged + metered).
            let backstop = StanzaBackstop::capture(&stanza, phase.bound_jid());
            let ingress_effect_capture =
                ingress_effect_capture_for_stanza(state, stanza_lang, stanza.as_ref());
            if let (Some(inbound_sequence), Some(shadow_submission)) = (
                reserved_inbound_for_sm,
                parked_shadow_submission(
                    state,
                    sm_state,
                    authenticated_session.as_ref(),
                    stanza.as_ref(),
                    ingress_effect_capture.clone(),
                ),
            ) {
                sm_inbound_completion.park_shadow_submission(inbound_sequence, shadow_submission);
            }
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
                        .into()
                    }

                    Stanza::Message(message) => handlers::message::handle_message(
                        message,
                        state,
                        phase,
                        state_machine.as_mut(),
                        authenticated_session.as_ref(),
                        ordered_relay_origin.clone(),
                        ingress_effect_capture.clone(),
                    )
                    .await
                    .into(),
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
                Err(StanzaTimeout::HandledIq(reply)) => (
                    ResponseBatch::from_frames(vec![reply]),
                    InboundDisposition::Handled,
                ),
                Err(StanzaTimeout::Unhandled) => {
                    (ResponseBatch::default(), InboundDisposition::Unhandled)
                }
                Err(StanzaTimeout::AdmissionRevoked) => {
                    *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
                    (ResponseBatch::default(), InboundDisposition::Unhandled)
                }
            };
            settle_inbound_dispatch(
                &state.deps.protocol.ingress_shadow,
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
    ingress_shadow: &crate::ingress_shadow::IngressShadowHandle,
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
                completion.complete(inbound_sequence, sm_state, |submission| {
                    let _ = ingress_shadow.try_submit(submission);
                });
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

fn parked_shadow_submission(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    authenticated_session: Option<&crate::auth::Session>,
    stanza: &Stanza,
    capture: Option<crate::ingress_shadow::IngressEffectCapture>,
) -> Option<ParkedIngressShadowSubmission> {
    let Stanza::Message(message) = stanza else {
        return None;
    };
    if !state.deps.protocol.ingress_shadow.is_enabled() {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::Disabled,
        );
        return None;
    }
    let Some(stream_id) = sm_state.stream_id.as_deref() else {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::NoStream,
        );
        return None;
    };
    if !sm_state.is_resumable() {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::NonResumable,
        );
        debug!(
            stream_id,
            "ingress shadow explicitly excludes non-resumable SM traffic until a connection-scoped fence exists"
        );
        return None;
    }
    let Some(fence) = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream_id)
    else {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::NoClaimFence,
        );
        debug!(
            stream_id,
            "ingress shadow skipped SM traffic because no current claim fence was present"
        );
        return None;
    };
    let Some(principal) =
        authenticated_session.and_then(|session| session.authenticated_principal_ref().ok())
    else {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::NoPrincipal,
        );
        return None;
    };
    let Some(capture) = capture else {
        waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
            waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::NoCapture,
        );
        return None;
    };
    waddle_xmpp::telemetry::reliability::increment_ingress_shadow_candidate(
        waddle_xmpp::telemetry::attributes::IngressCandidateOutcome::Parked,
    );
    Some(ParkedIngressShadowSubmission {
        stream_id: waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
        owner: fence.owner().clone(),
        claim_epoch: fence.epoch(),
        principal,
        target: match message.to.as_ref() {
            None => waddle_xmpp::ingress::NormalizedTarget::Absent,
            Some(jid) => match jid.try_as_full() {
                Ok(full) => waddle_xmpp::ingress::NormalizedTarget::Full(full.clone()),
                Err(bare) => waddle_xmpp::ingress::NormalizedTarget::Bare(bare.clone()),
            },
        },
        message: message.clone(),
        capture,
    })
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
    #[cfg(feature = "clustering")]
    use crate::ingress_shadow::IngressShadowHandle;
    #[cfg(feature = "clustering")]
    use std::sync::Arc;
    #[cfg(feature = "clustering")]
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry as _};
    use xmpp_parsers::message::MessageType;
    use xmpp_parsers::minidom::Element;

    #[tokio::test]
    async fn handled_dispatch_advances_h_unless_ordered_relay_owns_completion() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut state = waddle_xmpp::stream_management::StreamManagementState::new();
        state.enable("dispatch-test".to_string(), true, Some(300));
        let mut completion =
            crate::server::routes::interpret::SmInboundCompletionTracker::default();
        let sequence = completion.reserve(&state);

        settle_inbound_dispatch(
            &websocket_state.deps.protocol.ingress_shadow,
            InboundDisposition::Handled,
            false,
            Some(sequence),
            &mut completion,
            &mut state,
        );

        assert_eq!(state.get_inbound_count(), 1);

        let deferred = completion.reserve(&state);
        settle_inbound_dispatch(
            &websocket_state.deps.protocol.ingress_shadow,
            InboundDisposition::Handled,
            true,
            Some(deferred),
            &mut completion,
            &mut state,
        );
        assert_eq!(state.get_inbound_count(), 1);
        assert!(completion.has_pending());
    }

    #[tokio::test]
    async fn disabled_shadow_skips_capture_and_preserves_shadow_ordinal() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("shadow-disabled".to_string(), true, Some(300));
        let sequence = {
            let mut completion =
                crate::server::routes::interpret::SmInboundCompletionTracker::default();
            let sequence = completion.reserve(&sm_state);
            let message = xmpp_parsers::message::Message::new(Some(
                "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
            ));
            let stanza = Stanza::Message(message);
            assert!(
                ingress_effect_capture_for_stanza(websocket_state.as_ref(), None, &stanza,)
                    .is_none()
            );
            assert!(parked_shadow_submission(
                websocket_state.as_ref(),
                &sm_state,
                None,
                &stanza,
                None,
            )
            .is_none());
            settle_inbound_dispatch(
                &websocket_state.deps.protocol.ingress_shadow,
                InboundDisposition::Handled,
                false,
                Some(sequence),
                &mut completion,
                &mut sm_state,
            );
            sequence
        };

        assert_eq!(sequence.0, 1);
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
        assert_eq!(sm_state.get_inbound_count(), 1);
        assert_eq!(
            metrics.counter_sum("ingress.shadow.candidates", &[("outcome", "disabled")]),
            Some(1)
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_counts_message_without_stream_id_as_no_stream_candidate() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                Arc::new(InMemorySmSessionRegistry::new()),
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));

        assert!(
            parked_shadow_submission(websocket_state.as_ref(), &sm_state, None, &stanza, None,)
                .is_none()
        );
        assert_eq!(
            metrics.counter_sum("ingress.shadow.candidates", &[("outcome", "no_stream")]),
            Some(1)
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_counts_missing_claim_fence_before_later_gates() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                Arc::new(InMemorySmSessionRegistry::new()),
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("missing-fence".to_string(), true, Some(300));
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));

        assert!(
            parked_shadow_submission(websocket_state.as_ref(), &sm_state, None, &stanza, None,)
                .is_none()
        );
        assert_eq!(
            metrics.counter_sum(
                "ingress.shadow.candidates",
                &[("outcome", "no_claim_fence")]
            ),
            Some(1)
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn non_resumable_sm_explicitly_skips_shadow_parking() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                Arc::new(InMemorySmSessionRegistry::new()),
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("shadow-non-resumable".to_string(), false, Some(300));
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
        let message = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        ));
        let stanza = Stanza::Message(message);
        let capture = ingress_effect_capture_for_stanza(websocket_state.as_ref(), None, &stanza)
            .expect("enabled shadow should allocate capture state");

        assert!(parked_shadow_submission(
            websocket_state.as_ref(),
            &sm_state,
            Some(&session),
            &stanza,
            Some(capture),
        )
        .is_none());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
        assert_eq!(
            metrics.counter_sum("ingress.shadow.candidates", &[("outcome", "non_resumable")]),
            Some(1)
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_parks_fenced_principal_message_with_capture() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let _claim_guard = registry
            .ensure_session_claim("shadow-parked")
            .await
            .expect("test SM stream should acquire its claim fence");
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                registry,
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("shadow-parked".to_string(), true, Some(300));
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));
        let capture = ingress_effect_capture_for_stanza(websocket_state.as_ref(), None, &stanza)
            .expect("enabled shadow must allocate message capture");
        let parked_before = metrics
            .counter_sum("ingress.shadow.candidates", &[("outcome", "parked")])
            .unwrap_or(0);

        assert!(parked_shadow_submission(
            websocket_state.as_ref(),
            &sm_state,
            Some(&session),
            &stanza,
            Some(capture),
        )
        .is_some());
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.candidates", &[("outcome", "parked")])
                .unwrap_or(0),
            parked_before + 1
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn resumed_shadow_message_parks_with_the_retained_claim_fence() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let stream_id = "shadow-after-resume";
        let publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("enable claim");
        drop(publication);

        let jid = "alice@example.com/phone"
            .parse::<jid::FullJid>()
            .expect("jid");
        let mut detached_state = waddle_xmpp::stream_management::StreamManagementState::new();
        detached_state.enable(stream_id.to_string(), true, Some(300));
        let detached = detached_state
            .to_detached_session(waddle_xmpp::stream_management::DetachedSessionSnapshot {
                user_id: "alice@example.com".to_string(),
                jid,
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .expect("resumable snapshot");
        registry
            .store_session(detached)
            .await
            .expect("detach session");
        registry
            .claim_session(stream_id)
            .await
            .expect("claim session")
            .expect("detached session exists");
        let completion = registry
            .complete_claim_if_resumable(stream_id, 0)
            .await
            .expect("complete resume")
            .expect("claimed session exists");
        let waddle_xmpp::stream_management::SmClaimCompletion::Resumed(resumed) = completion else {
            panic!("resume must succeed")
        };
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.restore_from_session(&resumed);

        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                registry,
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));
        let capture = ingress_effect_capture_for_stanza(websocket_state.as_ref(), None, &stanza)
            .expect("message capture");
        let parked_before = metrics
            .counter_sum("ingress.shadow.candidates", &[("outcome", "parked")])
            .unwrap_or(0);
        let no_fence_before = metrics
            .counter_sum(
                "ingress.shadow.candidates",
                &[("outcome", "no_claim_fence")],
            )
            .unwrap_or(0);

        assert!(parked_shadow_submission(
            websocket_state.as_ref(),
            &sm_state,
            Some(&session),
            &stanza,
            Some(capture),
        )
        .is_some());
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.candidates", &[("outcome", "parked")])
                .unwrap_or(0),
            parked_before + 1
        );
        assert_eq!(
            metrics
                .counter_sum(
                    "ingress.shadow.candidates",
                    &[("outcome", "no_claim_fence")],
                )
                .unwrap_or(0),
            no_fence_before
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_counts_fenced_message_without_principal() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let _claim_guard = registry
            .ensure_session_claim("shadow-no-principal")
            .await
            .expect("test SM stream should acquire its claim fence");
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                registry,
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("shadow-no-principal".to_string(), true, Some(300));
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));
        let capture = ingress_effect_capture_for_stanza(websocket_state.as_ref(), None, &stanza)
            .expect("enabled shadow must allocate message capture");
        let no_principal_before = metrics
            .counter_sum("ingress.shadow.candidates", &[("outcome", "no_principal")])
            .unwrap_or(0);

        assert!(parked_shadow_submission(
            websocket_state.as_ref(),
            &sm_state,
            None,
            &stanza,
            Some(capture),
        )
        .is_none());
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.candidates", &[("outcome", "no_principal")],)
                .unwrap_or(0),
            no_principal_before + 1
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_counts_fenced_principal_message_without_capture() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let _claim_guard = registry
            .ensure_session_claim("shadow-no-capture")
            .await
            .expect("test SM stream should acquire its claim fence");
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                registry,
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let mut sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        sm_state.enable("shadow-no-capture".to_string(), true, Some(300));
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
        let stanza = Stanza::Message(xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        )));
        let no_capture_before = metrics
            .counter_sum("ingress.shadow.candidates", &[("outcome", "no_capture")])
            .unwrap_or(0);

        assert!(parked_shadow_submission(
            websocket_state.as_ref(),
            &sm_state,
            Some(&session),
            &stanza,
            None,
        )
        .is_none());
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.candidates", &[("outcome", "no_capture")])
                .unwrap_or(0),
            no_capture_before + 1
        );
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn enabled_shadow_does_not_count_non_message_stanzas_as_candidates() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_ingress_shadow(
                Arc::new(InMemorySmSessionRegistry::new()),
                IngressShadowHandle::spawn_test_worker(8, 1, |_kind, _stream_id| async move {}),
            )
            .await;
        let sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
        let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::None,
        ));
        let candidates_before = metrics
            .counter_sum("ingress.shadow.candidates", &[])
            .unwrap_or(0);

        assert!(
            parked_shadow_submission(websocket_state.as_ref(), &sm_state, None, &stanza, None)
                .is_none()
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.candidates", &[])
                .unwrap_or(0),
            candidates_before,
            "only message stanzas may be counted as ingress-shadow candidates"
        );
    }

    #[test]
    fn ingress_effect_capture_uses_stanza_lang_from_initial_parse_metadata() {
        // The stanza language captured during the initial typed parse is the
        // capture's single source — no raw-frame re-parse exists to fall
        // back on, so the constructor must carry it verbatim.
        let capture = crate::ingress_shadow::IngressEffectCapture::new(Some(Lang::from("fr")));
        assert_eq!(capture.snapshot().stanza_lang, Some(Lang::from("fr")));

        // With the shadow disabled (the default, and this fixture's state),
        // the seam allocates nothing at all — the default-off gate.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let websocket_state = runtime
            .block_on(crate::server::routes::websocket::tests::create_test_websocket_state());
        let mut message = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        ));
        message.type_ = MessageType::Groupchat;
        let stanza = Stanza::Message(message);
        assert!(ingress_effect_capture_for_stanza(
            websocket_state.as_ref(),
            Some(Lang::from("fr")),
            &stanza,
        )
        .is_none());
    }

    #[test]
    fn shadow_room_scope_recognizes_groupchat_pm_and_mediated_invite_decline_forms() {
        let groupchat = {
            let mut message = xmpp_parsers::message::Message::new(Some(
                "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
            ));
            message.type_ = MessageType::Groupchat;
            message
        };
        assert_eq!(
            shadow_room_scope(&groupchat, "muc.example.com")
                .expect("groupchat room scope")
                .to_string(),
            "room@muc.example.com"
        );

        let occupant_pm = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com/alice"
                .parse::<jid::Jid>()
                .expect("jid"),
        ));
        assert_eq!(
            shadow_room_scope(&occupant_pm, "muc.example.com")
                .expect("occupant pm room scope")
                .to_string(),
            "room@muc.example.com"
        );

        let mut invite = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        ));
        invite.type_ = MessageType::Normal;
        invite.payloads.push(
            Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER).build())
                .build(),
        );
        assert_eq!(
            shadow_room_scope(&invite, "muc.example.com")
                .expect("invite room scope")
                .to_string(),
            "room@muc.example.com"
        );

        let mut decline = xmpp_parsers::message::Message::new(Some(
            "room@muc.example.com".parse::<jid::Jid>().expect("jid"),
        ));
        decline.type_ = MessageType::Normal;
        decline.payloads.push(
            Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER).build(),
                )
                .build(),
        );
        assert_eq!(
            shadow_room_scope(&decline, "muc.example.com")
                .expect("decline room scope")
                .to_string(),
            "room@muc.example.com"
        );
    }
}
