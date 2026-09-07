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
use std::str::FromStr;
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

#[derive(Debug, Default)]
pub(crate) struct ResponseBatch {
    pub(super) frames: Vec<ResponseFrame>,
    pub(super) completions: Vec<RoomEffectCompletion>,
    pub(super) completion_frame_indices: Vec<usize>,
    pub(super) ingress_reports: Vec<crate::ingress::ExecutionReport>,
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
            ingress_reports: Vec::new(),
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
        self.ingress_reports.append(&mut other.ingress_reports);
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
    Box::pin(handle_xmpp_frame_impl(frame, domain, state, conn, None))
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
    Box::pin(handle_xmpp_frame_impl(
        frame,
        domain,
        state,
        conn,
        Some((permit, shutdown)),
    ))
    .await
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
        sm_ingress_fence,
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
                sm_ingress_fence,
                sm_inbound_completion,
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
            // A connection that lost its registry slot to a same-FullJID
            // replacement is superseded (#1703): none of its further stanzas
            // may perform FullJID-keyed writes (MUC joins, Muji updates,
            // call initiates), so its inbound stream ends here exactly like a
            // revoked authority. Its shutdown cleanup stays generation-scoped.
            if let (Some(owner), Some(bound)) = (registry_owner.as_ref(), phase.bound_jid()) {
                if !state
                    .deps
                    .protocol
                    .connection_registry
                    .is_owned_by(bound, owner)
                {
                    debug!(jid = %bound, "dropping stanza from a superseded connection");
                    if matches!(stanza.as_ref(), Stanza::Message(_)) && !sm_state.is_resumable() {
                        *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
                        return ResponseBatch::from_frames([
                            ingress_failure_stream_error(
                                crate::ingress::IngressDecisionClass::ClaimFenceMissing,
                            ),
                            websocket_stream_close_element(),
                        ]);
                    }
                    *inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
                    return ResponseBatch::default();
                }
            }
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
            if let Stanza::Message(message) = stanza.as_ref() {
                if let Some(principal) = authenticated_session
                    .as_ref()
                    .and_then(|session| session.authenticated_principal_ref().ok())
                {
                    return Box::pin(dispatch_authoritative_message(
                        MessageIngressDispatch {
                            message: message.clone(),
                            stanza_lang,
                            principal,
                            sequence: reserved_inbound_for_sm,
                            origin: ordered_relay_origin,
                            backstop,
                        },
                        state,
                        conn,
                        admission,
                    ))
                    .await;
                }
            }
            let dispatch = Box::pin(async {
                match *stanza {
                    Stanza::Iq(iq) => {
                        let mut iq_conn_state = handlers::iq::IqConnState {
                            carbons_enabled,
                            roster_interested,
                            blocklist_interested,
                            occupancy_session,
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
                                occupancy_session,
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
                        None,
                    )
                    .await
                    .into(),
                }
            });
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

struct MessageIngressDispatch {
    message: xmpp_parsers::message::Message,
    stanza_lang: Option<xmpp_parsers::message::Lang>,
    principal: waddle_xmpp::auth::AuthenticatedPrincipalRef,
    sequence: Option<crate::server::routes::interpret::OrderedRelayInboundSequence>,
    origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
    backstop: StanzaBackstop,
}

async fn dispatch_authoritative_message(
    request: MessageIngressDispatch,
    state: &WebSocketState,
    conn: &mut WsConnState,
    admission: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> ResponseBatch {
    use crate::ingress::{IngressDecisionClass, IngressStreamIdentity, IngressSubmission};
    use waddle_xmpp::ingress::{DigestContext, NormalizedTarget, WireHandledCount};
    let MessageIngressDispatch {
        message,
        stanza_lang,
        principal,
        sequence,
        origin,
        backstop,
    } = request;
    let checkpoint = sequence.map(|seq| conn.sm_inbound_completion.checkpoint_for(seq));
    let stream_id = conn
        .sm_state
        .stream_id
        .as_deref()
        .map(waddle_xmpp::pending_delivery::SmSessionId::new);
    let fence = conn.sm_ingress_fence.clone();
    let resumable = conn.sm_state.is_resumable();
    let deps = super::interpret_loop::build_interpret_deps(
        state,
        conn.authenticated_session
            .as_ref()
            .map(ResolvedPrincipal::from_authenticated_session),
    )
    .with_ordered_relay_origin(origin);
    let work = Box::pin(async {
        let identity = if resumable {
            let fence = fence
                .as_ref()
                .ok_or(IngressDecisionClass::ClaimFenceMissing)?;
            #[cfg(not(feature = "clustering"))]
            let _ = fence;
            let sequence = sequence.ok_or(IngressDecisionClass::Storage)?;
            let stream_id = stream_id.as_ref().ok_or(IngressDecisionClass::Storage)?;
            let sm_ingress_id = state
                .deps
                .protocol
                .ingress
                .lookup_stream(stream_id)
                .await
                .map_err(|_| IngressDecisionClass::Storage)?
                .ok_or(IngressDecisionClass::Storage)?;
            IngressStreamIdentity::Resumable {
                stream_id: stream_id.clone(),
                sm_ingress_id,
                #[cfg(feature = "clustering")]
                owner: fence.owner().clone(),
                #[cfg(feature = "clustering")]
                claim_epoch: fence.epoch(),
                reserved_wire_position: WireHandledCount::from_storage(sequence.0),
                checkpoint_h: WireHandledCount::from_storage(
                    checkpoint.expect("reserved checkpoint"),
                ),
            }
        } else {
            IngressStreamIdentity::Ephemeral {
                principal: principal.clone(),
            }
        };
        let target = match message.to.as_ref() {
            None => NormalizedTarget::Absent,
            Some(jid) => match jid.try_as_full() {
                Ok(full) => NormalizedTarget::Full(full.clone()),
                Err(bare) => NormalizedTarget::Bare(bare.clone()),
            },
        };
        let muc: jid::BareJid = state
            .deps
            .service_domains
            .muc
            .parse()
            .map_err(|_| IngressDecisionClass::Storage)?;
        let digest_context = DigestContext {
            target: target.clone(),
            server_authorities: crate::ingress::submission::digest_authorities(
                &message,
                principal.bare_jid(),
                muc.domain(),
            ),
            stanza_lang,
        };
        let (digest_input, malformed) =
            match crate::ingress::submission::digest_input(&message, &digest_context) {
                Ok(input) => (input, false),
                Err(
                    waddle_xmpp::ingress::DigestInputError::DuplicateOriginId
                    | waddle_xmpp::ingress::DigestInputError::MalformedOriginId
                    | waddle_xmpp::ingress::DigestInputError::DuplicateThread
                    | waddle_xmpp::ingress::DigestInputError::DuplicateReply
                    | waddle_xmpp::ingress::DigestInputError::ReplyMalformed,
                ) => (
                    waddle_xmpp::ingress::DigestInput::from_rejected_parsed(
                        &message,
                        &digest_context,
                    )
                    .map_err(|_| IngressDecisionClass::Storage)?,
                    true,
                ),
                Err(_) => return Err(IngressDecisionClass::Storage),
            };
        let sender = conn
            .phase
            .bound_jid()
            .cloned()
            .ok_or(IngressDecisionClass::PrincipalMissing)?;
        let machine = conn
            .state_machine
            .as_mut()
            .ok_or(IngressDecisionClass::Storage)?;
        let plan = if malformed {
            crate::server::routes::interpret::reject_malformed_message(message, &sender)
        } else {
            crate::server::routes::interpret::plan_message_dispatch(machine, message, &deps).await
        };
        let submission = IngressSubmission {
            sender,
            identity,
            principal,
            target,
            plan,
            digest_input,
            connection_generation: conn.ingress_generation,
        };
        Ok::<_, IngressDecisionClass>(state.deps.protocol.ingress.commit(&submission).await)
    });
    let (result, revoked) = match admission {
        Some((permit, shutdown)) => {
            let result = super::frame_backstop::run_commit_with_backstop_and_admission(
                backstop, work, permit, shutdown,
            )
            .await;
            (result.result, result.authority_revoked_after_start)
        }
        None => (run_with_backstop(backstop, work).await, false),
    };
    if revoked {
        conn.inbound_frame_terminal = Some(InboundFrameTerminal::AuthorityRevoked);
    }
    let decision = match result {
        Ok(Ok(decision)) if decision.class.advances() => decision,
        failure => {
            let class = match failure {
                Ok(Ok(decision)) => decision.class,
                Ok(Err(class)) => class,
                Err(_) => IngressDecisionClass::Timeout,
            };
            settle_inbound_dispatch(
                InboundDisposition::Unhandled,
                false,
                sequence,
                &mut conn.sm_inbound_completion,
                &mut conn.sm_state,
            );
            if resumable {
                return ResponseBatch::default();
            }
            conn.phase = ConnectionPhase::closing(conn.phase.bound_jid().cloned());
            return ResponseBatch::from_frames([
                ResponseFrame::Element(ingress_failure_stream_error(class)),
                ResponseFrame::Element(websocket_stream_close_element()),
            ]);
        }
    };
    if resumable {
        if let (Some(seq), Some(checkpoint)) = (sequence, checkpoint) {
            conn.sm_inbound_completion.mark_committed(seq, checkpoint);
        }
    }
    settle_inbound_dispatch(
        InboundDisposition::Handled,
        false,
        sequence,
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );
    if revoked {
        return ResponseBatch::default();
    }
    if let (Some(owner), Some(bound)) = (conn.registry_owner.as_ref(), conn.phase.bound_jid()) {
        if !state
            .deps
            .protocol
            .connection_registry
            .is_owned_by(bound, owner)
        {
            conn.phase = ConnectionPhase::closing(conn.phase.bound_jid().cloned());
            return ResponseBatch::from_frames([
                ingress_failure_stream_error(IngressDecisionClass::ClaimFenceMissing),
                websocket_stream_close_element(),
            ]);
        }
    }
    // Phase C owns its budget, including admission waits; cancellation cannot
    // revoke the disposition that was settled immediately after commit.
    execute_committed_message(
        state
            .deps
            .protocol
            .ingress
            .execute(&decision, &crate::ingress::ImmediateSink, &deps),
        std::time::Duration::from_secs(5),
    )
    .await
}

pub(super) async fn execute_committed_message(
    execution: impl std::future::Future<Output = crate::ingress::ExecutionReport>,
    budget: std::time::Duration,
) -> ResponseBatch {
    match tokio::time::timeout(budget, execution).await {
        Ok(report) => {
            let mut batch = ResponseBatch::from_frames(
                report
                    .frame_obligations
                    .iter()
                    .flat_map(|obligation| obligation.frames.iter().cloned()),
            );
            batch.ingress_reports.push(report);
            batch
        }
        Err(_) => ResponseBatch::default(),
    }
}

fn ingress_failure_stream_error(class: crate::ingress::IngressDecisionClass) -> Element {
    use crate::ingress::IngressDecisionClass;
    use xmpp_parsers::stream_error::{DefinedCondition, StreamError};
    let condition = match class {
        IngressDecisionClass::PrincipalMissing => DefinedCondition::NotAuthorized,
        IngressDecisionClass::ClaimFenceMissing | IngressDecisionClass::RoomGenerationStale => {
            DefinedCondition::Conflict
        }
        _ => DefinedCondition::InternalServerError,
    };
    StreamError {
        condition,
        texts: Default::default(),
        application_specific: Vec::new(),
    }
    .into()
}
