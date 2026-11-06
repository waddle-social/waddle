use super::frame_backstop::{
    run_with_backstop, run_with_backstop_and_admission, InboundDisposition, StanzaBackstop,
    StanzaTimeout,
};
use super::parse_errors::{
    is_sasl2_parse_failure, is_sasl2_response_parse_failure, is_sasl_parse_failure,
    parse_error_responses,
};
use super::resource_binding::handle_resource_binding;
use super::sasl::{
    handle_sasl_oauthbearer, handle_sasl_scram_client_first, handle_sasl_scram_response,
    record_scram_failure,
};
use super::state::{authenticated_client_room_origin, InboundFrameTerminal, WsConnState};
use super::stream_management::{handle_sm_stanza, SmCtx};
use super::transport_xml::{
    build_handled_count_too_high_stream_error, build_stream_features_element,
    build_stream_features_for_phase_element, element_to_xml, sasl2_failure_element,
    sasl_failure_element, stanza_to_xml, websocket_stream_close_element,
    websocket_stream_open_element,
};
use super::{
    debug, handlers, info, warn, ConnectionPhase, InboundFrame, ParseError, ResolvedPrincipal,
    SmStanza, Stanza, WebSocketState, MAX_FRAME_SIZE,
};
use crate::clustering::NodeAdmissionPermit;
use crate::ingress::ExecutionReport;
use crate::ingress::ImmediateSink;
use crate::ingress::IngressDecisionClass;
use crate::ingress::IngressPrincipal;
use crate::room_effect_outbox::drain::RoomEffectCompletion;
use crate::server::routes::auth_telemetry::AuthFailure;
use crate::server::routes::interpret::effects::{ClientRoomOrigin, IngressPlan};
use crate::server::routes::interpret::OrderedRelayHandoffCompletion;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::OrderedRelayHandoffHandle;
use crate::server::routes::interpret::OrderedRelayInboundSequence;
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::OrderedRelayRouteOriginKind;
use crate::server::routes::interpret::SmInboundCompletionTracker;
use std::str::FromStr;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stream_error::{DefinedCondition, StreamError};

#[cfg(test)]
tokio::task_local! {
    pub(super) static TEST_LOCAL_ROOM_PLAN_ORIGINS:
        std::sync::Arc<std::sync::Mutex<Vec<Option<ClientRoomOrigin>>>>;
}

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
    /// Replay keeps the durable ingress obligation with the retained frame.
    Replay {
        frame: Box<ResponseFrame>,
        ingress_receipts: Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt>,
    },
}

impl ResponseFrame {
    pub(crate) fn into_serialized_xml(self) -> String {
        match self {
            Self::Stanza(stanza) => stanza_to_xml(&stanza),
            Self::Element(element) => element_to_xml(element),
            Self::StreamError(error) => error.into_serialized_xml(),
            Self::Replay { frame, .. } => frame.into_serialized_xml(),
        }
    }

    pub(crate) fn is_websocket_stream_close(&self) -> bool {
        match self {
            Self::Stanza(_) => false,
            Self::Element(element) => {
                element.name() == "close" && element.ns() == "urn:ietf:params:xml:ns:xmpp-framing"
            }
            Self::StreamError(_) => false,
            Self::Replay { frame, .. } => frame.is_websocket_stream_close(),
        }
    }

    pub(crate) fn with_ingress_receipts(
        self,
        ingress_receipts: Vec<waddle_xmpp::stream_management::SmIngressFrameReceipt>,
    ) -> Self {
        if ingress_receipts.is_empty() {
            self
        } else {
            Self::Replay {
                frame: Box::new(self),
                ingress_receipts,
            }
        }
    }

    /// The stanza this frame carries, through any replay obligation wrapper.
    #[cfg(all(test, feature = "clustering"))]
    pub(crate) fn stanza(&self) -> Option<&Stanza> {
        match self {
            Self::Stanza(stanza) => Some(stanza),
            Self::Replay { frame, .. } => frame.stanza(),
            Self::Element(_) | Self::StreamError(_) => None,
        }
    }

    pub(crate) fn ingress_receipts(
        &self,
    ) -> &[waddle_xmpp::stream_management::SmIngressFrameReceipt] {
        match self {
            Self::Replay {
                ingress_receipts, ..
            } => ingress_receipts,
            _ => &[],
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
    pub(super) ingress_reports: Vec<ExecutionReport>,
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

fn stream_open_error_response(
    domain: &str,
    to: Option<&jid::BareJid>,
    condition: DefinedCondition,
) -> ResponseBatch {
    vec![
        websocket_stream_open_element(domain, to),
        Element::from(StreamError {
            condition,
            texts: Default::default(),
            application_specific: Vec::new(),
        }),
        websocket_stream_close_element(),
    ]
    .into()
}

fn sasl2_authentication_pending(phase: &ConnectionPhase) -> bool {
    phase.is_sasl2_oauth_error_pending() || phase.scram_pending_is_sasl2()
}

fn clear_sasl2_authentication_pending(phase: &mut ConnectionPhase, scram_failure: AuthFailure<'_>) {
    if phase.take_sasl2_oauth_error_pending() {
        return;
    }
    if phase.scram_pending_is_sasl2() {
        let _ = phase.take_scram_pending();
        record_scram_failure(scram_failure, None);
    }
}

fn sasl2_stream_error_response() -> ResponseBatch {
    vec![
        Element::from(StreamError {
            condition: DefinedCondition::NotAuthorized,
            texts: Default::default(),
            application_specific: Vec::new(),
        }),
        websocket_stream_close_element(),
    ]
    .into()
}

fn close_pending_sasl2_exchange(conn: &mut WsConnState) -> ResponseBatch {
    clear_sasl2_authentication_pending(&mut conn.phase, AuthFailure::ScramOther);
    conn.authenticated_session = None;
    conn.phase = ConnectionPhase::closing(None);
    vec![websocket_stream_close_element()].into()
}

fn sasl2_failure_from_sasl1(responses: &[Element]) -> Element {
    use waddle_xmpp_core::xep0388::Sasl2FailureCondition;

    let condition = responses
        .first()
        .and_then(|response| {
            response
                .children()
                .find(|condition| condition.ns() == waddle_xmpp_core::xep0388::NS_SASL)
        })
        .map(|condition| match condition.name() {
            "aborted" => Sasl2FailureCondition::Aborted,
            "account-disabled" => Sasl2FailureCondition::AccountDisabled,
            "credentials-expired" => Sasl2FailureCondition::CredentialsExpired,
            "encryption-required" => Sasl2FailureCondition::EncryptionRequired,
            "incorrect-encoding" => Sasl2FailureCondition::IncorrectEncoding,
            "invalid-authzid" => Sasl2FailureCondition::InvalidAuthzid,
            "invalid-mechanism" => Sasl2FailureCondition::InvalidMechanism,
            "malformed-request" => Sasl2FailureCondition::MalformedRequest,
            "mechanism-too-weak" => Sasl2FailureCondition::MechanismTooWeak,
            "temporary-auth-failure" => Sasl2FailureCondition::TemporaryAuthFailure,
            _ => Sasl2FailureCondition::NotAuthorized,
        })
        .unwrap_or(Sasl2FailureCondition::NotAuthorized);
    sasl2_failure_element(condition)
}

/// One bounded input classified at the transport boundary. No raw XML survives
/// admission into the dormant owner task; size and error-routing metadata do.
#[derive(Debug, Clone)]
pub(super) struct ClassifiedInboundFrame {
    wire_bytes: usize,
    input: ClassifiedInboundInput,
}

/// Proof that classification accepted exactly an SM acknowledgement request.
/// The original byte count is retained for admission accounting.
#[derive(Debug, Clone, Copy)]
pub(super) struct ClassifiedSmRequest {
    wire_bytes: usize,
}

impl ClassifiedSmRequest {
    pub(super) fn wire_bytes(self) -> usize {
        self.wire_bytes
    }
}

#[derive(Debug, Clone)]
enum ClassifiedInboundInput {
    Parsed(Box<waddle_xmpp::protocol::frame::ParsedFrame>),
    StreamManagement(SmStanza),
    Empty,
    Oversized,
    Malformed(MalformedInboundInput),
}

#[derive(Debug, Clone)]
enum MalformedInboundInput {
    Open(DefinedCondition),
    Protocol {
        sasl: MalformedSaslInput,
        responses: Option<Vec<ResponseFrame>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MalformedSaslInput {
    None,
    Sasl,
    Sasl2,
    Sasl2Response,
}

impl ClassifiedInboundFrame {
    pub(super) fn classify(frame: &str) -> Self {
        let input = if frame.len() > MAX_FRAME_SIZE {
            ClassifiedInboundInput::Oversized
        } else if let Some(sm) = SmStanza::is_client_nonza_candidate(frame)
            .then(|| SmStanza::parse(frame))
            .flatten()
        {
            ClassifiedInboundInput::StreamManagement(sm)
        } else {
            match waddle_xmpp::protocol::frame::parse_frame_with_metadata(frame) {
                Ok(parsed) => ClassifiedInboundInput::Parsed(Box::new(parsed)),
                Err(ParseError::Empty) => ClassifiedInboundInput::Empty,
                Err(error) => {
                    let malformed = match error {
                        ParseError::InvalidOpenFrom => {
                            MalformedInboundInput::Open(DefinedCondition::InvalidFrom)
                        }
                        ParseError::InvalidOpenXml => {
                            MalformedInboundInput::Open(DefinedCondition::NotWellFormed)
                        }
                        ParseError::InvalidOpenNamespace => {
                            MalformedInboundInput::Open(DefinedCondition::InvalidNamespace)
                        }
                        ParseError::InvalidOpenContent => {
                            MalformedInboundInput::Open(DefinedCondition::InvalidXml)
                        }
                        _ => {
                            let sasl = if is_sasl2_response_parse_failure(frame, &error) {
                                MalformedSaslInput::Sasl2Response
                            } else if is_sasl2_parse_failure(frame, &error) {
                                MalformedSaslInput::Sasl2
                            } else if is_sasl_parse_failure(frame, &error) {
                                MalformedSaslInput::Sasl
                            } else {
                                MalformedSaslInput::None
                            };
                            // Reuse the existing error mapping and retain only its typed
                            // output template, never the rejected client XML or parse text.
                            let responses = parse_error_responses(frame, &error).map(|responses| {
                                responses.into_iter().map(ResponseFrame::from).collect()
                            });
                            warn!(error = %error, len = frame.len(), "Classified malformed XMPP frame");
                            MalformedInboundInput::Protocol { sasl, responses }
                        }
                    };
                    ClassifiedInboundInput::Malformed(malformed)
                }
            }
        };
        Self {
            wire_bytes: frame.len(),
            input,
        }
    }

    pub(super) fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }

    pub(super) fn sm_request(&self) -> Option<ClassifiedSmRequest> {
        matches!(
            self.input,
            ClassifiedInboundInput::StreamManagement(SmStanza::Request)
        )
        .then_some(ClassifiedSmRequest {
            wire_bytes: self.wire_bytes,
        })
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
    Box::pin(handle_xmpp_frame_impl(
        ClassifiedInboundFrame::classify(frame),
        domain,
        state,
        conn,
        None,
    ))
    .await
    .into_serialized_frames()
}

pub(super) async fn handle_xmpp_frame_with_admission(
    frame: &str,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> ResponseBatch {
    Box::pin(handle_xmpp_frame_impl(
        ClassifiedInboundFrame::classify(frame),
        domain,
        state,
        conn,
        Some((permit, shutdown)),
    ))
    .await
}

/// Handle an already-classified frame from an actor mailbox. The task command
/// carries typed protocol data and a typed XMPP domain; legacy `/ws` still
/// parses at its current input boundary and shares the dispatch body below.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "generic inbound work awaits bounded producers")
)]
pub(super) async fn handle_classified_xmpp_frame_with_admission(
    input: ClassifiedInboundFrame,
    domain: &jid::BareJid,
    state: &WebSocketState,
    conn: &mut WsConnState,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> ResponseBatch {
    Box::pin(handle_xmpp_frame_impl(
        input,
        domain.as_str(),
        state,
        conn,
        Some((permit, shutdown)),
    ))
    .await
}

async fn await_control_stage<T>(
    admission: Option<(&NodeAdmissionPermit, &tokio_util::sync::CancellationToken)>,
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

/// The dark bounded driver uses the same request handler and authority fence
/// as the ordinary frame dispatcher, but retains its single typed response.
pub(super) async fn handle_classified_sm_request_with_admission(
    state: &WebSocketState,
    conn: &mut WsConnState,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Option<waddle_xmpp::stream_management::SmAck> {
    match await_control_stage(
        Some((permit, shutdown)),
        super::stream_management::handle_sm_request(
            state,
            &mut conn.phase,
            &mut conn.sm_state,
            &mut conn.sm_inbound_completion,
        ),
    )
    .await
    {
        Ok(response) => response,
        Err(terminal) => {
            conn.inbound_frame_terminal = Some(terminal);
            None
        }
    }
}

async fn handle_xmpp_frame_impl(
    input: ClassifiedInboundFrame,
    domain: &str,
    state: &WebSocketState,
    conn: &mut WsConnState,
    admission: Option<(&NodeAdmissionPermit, &tokio_util::sync::CancellationToken)>,
) -> ResponseBatch {
    if matches!(input.input, ClassifiedInboundInput::Oversized) {
        warn!(len = input.wire_bytes, "Dropping oversized XMPP frame");
        if sasl2_authentication_pending(&conn.phase) {
            return close_pending_sasl2_exchange(conn);
        }
        return ResponseBatch::default();
    }

    let WsConnState {
        connection_ref,
        binding_ref,
        #[cfg(test)]
        resume_binding_attempts,
        phase,
        stream_open_from,
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

    // Both transport and dormant owner inputs share the same typed SM dispatch.
    if let ClassifiedInboundInput::StreamManagement(sm) = input.input {
        let ctx = SmCtx {
            connection_ref,
            binding_ref,
            #[cfg(test)]
            resume_binding_attempts,
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

    let parsed = match input.input {
        ClassifiedInboundInput::Parsed(parsed) => *parsed,
        ClassifiedInboundInput::Empty => {
            if sasl2_authentication_pending(phase) {
                clear_sasl2_authentication_pending(phase, AuthFailure::ScramOther);
                *authenticated_session = None;
                *phase = ConnectionPhase::closing(None);
                return vec![websocket_stream_close_element()].into();
            }
            return ResponseBatch::default();
        }
        ClassifiedInboundInput::Malformed(MalformedInboundInput::Open(condition)) => {
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            *stream_open_from = None;
            conn.begin_server_stream_open_response();
            return stream_open_error_response(domain, None, condition);
        }
        ClassifiedInboundInput::Malformed(MalformedInboundInput::Protocol { sasl, responses }) => {
            if sasl2_authentication_pending(phase) {
                if sasl == MalformedSaslInput::Sasl2Response {
                    clear_sasl2_authentication_pending(phase, AuthFailure::ScramMalformed);
                    *authenticated_session = None;
                    if let Some(responses) = responses {
                        return ResponseBatch::from_frames(responses);
                    }
                }
                // XEP-0388 permits only a response or abort during a challenge.
                clear_sasl2_authentication_pending(phase, AuthFailure::ScramOther);
                *phase = ConnectionPhase::closing(None);
                *authenticated_session = None;
                return vec![websocket_stream_close_element()].into();
            }
            if let Some(responses) = responses {
                if phase.scram_pending_username().is_some() && sasl != MalformedSaslInput::None {
                    record_scram_failure(AuthFailure::ScramMalformed, None);
                    let _ = phase.take_scram_pending();
                }
                return ResponseBatch::from_frames(responses);
            }
            return ResponseBatch::default();
        }
        ClassifiedInboundInput::Oversized | ClassifiedInboundInput::StreamManagement(_) => {
            unreachable!("size and stream management inputs are handled before frame dispatch")
        }
    };
    let stanza_lang = parsed.message_stanza_lang;

    if sasl2_authentication_pending(phase)
        && !matches!(
            &parsed.frame,
            InboundFrame::Sasl2Response(_) | InboundFrame::Sasl2Abort
        )
    {
        // XEP-0388 requires disconnecting if the client sends anything other
        // than a SASL2 response or abort during authentication.
        clear_sasl2_authentication_pending(phase, AuthFailure::ScramOther);
        *phase = ConnectionPhase::closing(None);
        *authenticated_session = None;
        return vec![websocket_stream_close_element()].into();
    }

    match parsed.frame {
        InboundFrame::Open { from } => {
            info!("XMPP stream open requested");
            let mismatched_principal = from.as_ref().is_some_and(|requested| {
                phase
                    .authenticated_bare_jid()
                    .is_some_and(|principal| requested != principal)
                    || phase
                        .bound_jid()
                        .is_some_and(|principal| requested != &principal.to_bare())
            });
            if mismatched_principal {
                *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
                *stream_open_from = None;
                conn.begin_server_stream_open_response();
                return stream_open_error_response(
                    domain,
                    from.as_ref(),
                    DefinedCondition::InvalidFrom,
                );
            }
            let open_element = websocket_stream_open_element(domain, from.as_ref());
            *stream_open_from = from;
            let features_element =
                build_stream_features_for_phase_element(phase, state.sasl2_negotiation_enabled());
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
                        handle_sasl_scram_client_first(&data, domain, state, phase, false).await
                    }
                    "OAUTHBEARER" => {
                        handle_sasl_oauthbearer(
                            &data,
                            state,
                            stream_open_from.as_ref(),
                            authenticated_session,
                            phase,
                            false,
                        )
                        .await
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
            if !phase.allows_sasl_response() || phase.scram_pending_is_sasl2() {
                warn!(phase = ?phase, "SASL response received in invalid phase");
                record_scram_failure(AuthFailure::ScramOther, None);
                if phase.scram_pending_is_sasl2() {
                    let _ = phase.take_scram_pending();
                }
                return vec![sasl_failure_element("not-authorized")].into();
            }
            let Some(scram) = phase.take_scram_pending() else {
                record_scram_failure(AuthFailure::ScramOther, None);
                return vec![sasl_failure_element("not-authorized")].into();
            };
            let response = handle_sasl_scram_response(
                &data,
                domain,
                state,
                scram,
                stream_open_from.as_ref(),
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
            vec![response.element].into()
        }

        InboundFrame::Sasl2Authenticate(request) => {
            use waddle_xmpp_core::xep0388::{
                Sasl2FailureCondition, Sasl2Mechanism, Sasl2Success, NS_SASL2,
            };

            // SASL2 is dark by default. Only an explicitly opted-in test
            // fixture can advertise and dispatch this handshake; operator
            // URL configuration is not evidence of the request's TLS or
            // early-data state.
            if !state.sasl2_negotiation_enabled() {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::InvalidMechanism,
                )]
                .into();
            }
            if phase.is_authenticated() {
                // XEP-0388 §4.8 requires a stream error for any new SASL2
                // authentication after success. Retain session and binding
                // authority until normal transport-close cleanup runs.
                let bound_jid = phase.bound_jid().cloned();
                *phase = ConnectionPhase::closing(bound_jid);
                return sasl2_stream_error_response();
            }
            if !phase.allows_sasl_auth() {
                return vec![sasl2_failure_element(Sasl2FailureCondition::NotAuthorized)].into();
            }
            if !matches!(
                &request.mechanism,
                Sasl2Mechanism::OAuthBearer | Sasl2Mechanism::ScramSha256
            ) {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::InvalidMechanism,
                )]
                .into();
            }
            if request
                .additional_children()
                .iter()
                .any(|child| !child.is("user-agent", NS_SASL2))
            {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::MalformedRequest,
                )]
                .into();
            }
            let Some(initial_response) = request.initial_response else {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::MalformedRequest,
                )]
                .into();
            };

            let initial_response = initial_response.encoded();
            let mechanism = request.mechanism;
            let phase_was_unauthenticated = matches!(phase, ConnectionPhase::Unauthenticated);
            let responses = match await_control_stage(admission, async {
                match &mechanism {
                    Sasl2Mechanism::OAuthBearer => {
                        handle_sasl_oauthbearer(
                            &initial_response,
                            state,
                            stream_open_from.as_ref(),
                            authenticated_session,
                            phase,
                            true,
                        )
                        .await
                    }
                    Sasl2Mechanism::ScramSha256 => {
                        handle_sasl_scram_client_first(
                            &initial_response,
                            domain,
                            state,
                            phase,
                            true,
                        )
                        .await
                    }
                    _ => vec![sasl2_failure_element(
                        Sasl2FailureCondition::InvalidMechanism,
                    )],
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

            if responses
                .first()
                .is_some_and(|response| response.is("challenge", NS_SASL2))
            {
                return responses.into();
            }

            if responses
                .first()
                .is_some_and(|response| response.is("success", waddle_xmpp::ns::SASL))
            {
                if mechanism != Sasl2Mechanism::OAuthBearer {
                    warn!("SASL2 SCRAM client-first unexpectedly completed authentication");
                    *authenticated_session = None;
                    *phase = ConnectionPhase::closing(None);
                    return vec![
                        sasl2_failure_element(Sasl2FailureCondition::TemporaryAuthFailure),
                        websocket_stream_close_element(),
                    ]
                    .into();
                }
                let Some(authorization_identifier) = phase.authenticated_bare_jid().cloned() else {
                    warn!("SASL2 OAuth verifier succeeded without an authenticated principal");
                    *authenticated_session = None;
                    *phase = ConnectionPhase::closing(None);
                    return vec![
                        sasl2_failure_element(Sasl2FailureCondition::TemporaryAuthFailure),
                        websocket_stream_close_element(),
                    ]
                    .into();
                };
                let success = Sasl2Success {
                    additional_data: None,
                    authorization_identifier: authorization_identifier.into(),
                    inline_results: Vec::new(),
                }
                .to_element();
                return vec![success, build_stream_features_element(true)].into();
            }

            if phase_was_unauthenticated && phase.is_closing() {
                // SASL2 failure leaves stream and session state unchanged.
                // The shared legacy verifier closes on a stream/from mismatch;
                // clear that legacy side effect for this SASL2 exchange.
                *phase = ConnectionPhase::Unauthenticated;
            }
            vec![sasl2_failure_from_sasl1(&responses)].into()
        }

        InboundFrame::Sasl2Response(response) => {
            use waddle_xmpp_core::xep0388::{Sasl2FailureCondition, Sasl2Success};

            if !state.sasl2_negotiation_enabled() {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::InvalidMechanism,
                )]
                .into();
            }
            if phase.take_sasl2_oauth_error_pending() {
                *authenticated_session = None;
                if response.data.as_bytes() != [0x01] {
                    warn!("SASL2 OAUTHBEARER error challenge received a non-dummy response");
                }
                return vec![sasl2_failure_element(Sasl2FailureCondition::NotAuthorized)].into();
            }
            if !phase.allows_sasl_response() || !phase.scram_pending_is_sasl2() {
                return vec![sasl2_failure_element(Sasl2FailureCondition::NotAuthorized)].into();
            }
            let Some(scram) = phase.take_scram_pending() else {
                return vec![sasl2_failure_element(Sasl2FailureCondition::NotAuthorized)].into();
            };
            let response_result = handle_sasl_scram_response(
                &response.data.encoded(),
                domain,
                state,
                scram,
                stream_open_from.as_ref(),
                authenticated_session,
                phase,
            )
            .await;
            if response_result.element.is("success", waddle_xmpp::ns::SASL) {
                let Some(authorization_identifier) = phase.authenticated_bare_jid().cloned() else {
                    return vec![sasl2_failure_element(Sasl2FailureCondition::NotAuthorized)]
                        .into();
                };
                let Some(server_final) = response_result.server_final else {
                    warn!("SASL2 SCRAM verifier omitted server-final data");
                    record_scram_failure(AuthFailure::ScramOther, Some(&authorization_identifier));
                    *authenticated_session = None;
                    *phase = ConnectionPhase::closing(None);
                    return vec![
                        sasl2_failure_element(Sasl2FailureCondition::TemporaryAuthFailure),
                        websocket_stream_close_element(),
                    ]
                    .into();
                };
                let success = Sasl2Success {
                    additional_data: Some(waddle_xmpp_core::xep0388::Sasl2Data::new(server_final)),
                    authorization_identifier: authorization_identifier.into(),
                    inline_results: Vec::new(),
                }
                .to_element();
                return vec![success, build_stream_features_element(true)].into();
            }
            if phase.is_closing() {
                *phase = ConnectionPhase::Unauthenticated;
            }
            vec![sasl2_failure_from_sasl1(std::slice::from_ref(
                &response_result.element,
            ))]
            .into()
        }

        InboundFrame::Sasl2Abort => {
            use waddle_xmpp_core::xep0388::Sasl2FailureCondition;

            if !state.sasl2_negotiation_enabled() {
                return vec![sasl2_failure_element(
                    Sasl2FailureCondition::InvalidMechanism,
                )]
                .into();
            }
            let was_pending = if phase.is_sasl2_oauth_error_pending() {
                phase.take_sasl2_oauth_error_pending()
            } else if phase.scram_pending_is_sasl2() {
                let _ = phase.take_scram_pending();
                record_scram_failure(AuthFailure::ScramOther, None);
                true
            } else {
                false
            };
            if was_pending {
                *authenticated_session = None;
            }
            vec![sasl2_failure_element(Sasl2FailureCondition::Aborted)].into()
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
                            ingress_failure_stream_error(IngressDecisionClass::ClaimFenceMissing),
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

            let client_room_origin = authenticated_client_room_origin(
                phase,
                authenticated_session.as_ref(),
                *occupancy_session,
            );
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
                            client_room_origin,
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
                            authenticated_session.as_ref(),
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
                                authenticated_session: authenticated_session.as_ref(),
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
                ordered_relay_origin_was_deferred(ordered_relay_origin.as_ref()),
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
    inbound_sequence: Option<OrderedRelayInboundSequence>,
    completion: &mut SmInboundCompletionTracker,
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
    registry_owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    inbound_sequence: Option<OrderedRelayInboundSequence>,
    handoff_tx: Option<&tokio::sync::mpsc::UnboundedSender<OrderedRelayHandoffCompletion>>,
) -> Option<OrderedRelayRouteOrigin> {
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
            return Some(OrderedRelayRouteOrigin {
                kind: OrderedRelayRouteOriginKind::RemoteResource(remote),
                sender_entity,
                inbound_sequence: inbound_sequence.map(|sequence| sequence.0).unwrap_or(0),
                handoff: if sm_state.enabled {
                    inbound_sequence.and_then(|sequence| {
                        handoff_tx.map(|tx| OrderedRelayHandoffHandle::new(sequence, tx.clone()))
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
        return Some(OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::SmSession(
                waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            ),
            sender_entity,
            inbound_sequence: inbound_sequence.0,
            handoff: handoff_tx
                .map(|tx| OrderedRelayHandoffHandle::new(inbound_sequence, tx.clone())),
        });
    }
    Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
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
    registry_owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    inbound_sequence: Option<OrderedRelayInboundSequence>,
    handoff_tx: Option<&tokio::sync::mpsc::UnboundedSender<OrderedRelayHandoffCompletion>>,
) -> Option<OrderedRelayRouteOrigin> {
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
fn ordered_relay_origin_was_deferred(origin: Option<&OrderedRelayRouteOrigin>) -> bool {
    origin
        .and_then(|origin| origin.handoff.as_ref())
        .is_some_and(|handoff| handoff.was_deferred())
}

#[cfg(not(feature = "clustering"))]
fn ordered_relay_origin_was_deferred(origin: Option<&OrderedRelayRouteOrigin>) -> bool {
    let _ = origin;
    false
}

#[cfg(feature = "clustering")]
pub(super) fn ordered_relay_origin_from_sm(
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
) -> Option<OrderedRelayRouteOrigin> {
    let sender_entity = bound_jid.map(|jid| {
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            jid.to_bare().to_string(),
        )
    })?;
    if sm_state.enabled {
        let stream_id = sm_state.stream_id.as_ref()?;
        return Some(OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::SmSession(
                waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            ),
            sender_entity,
            inbound_sequence: sm_state.inbound_count(),
            handoff: None,
        });
    }
    Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    })
}

#[cfg(not(feature = "clustering"))]
pub(super) fn ordered_relay_origin_from_sm(
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
    bound_jid: Option<&jid::FullJid>,
) -> Option<OrderedRelayRouteOrigin> {
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

enum DigestRejection {
    Malformed,
    Resource,
}

struct MessageIngressDispatch {
    message: xmpp_parsers::message::Message,
    stanza_lang: Option<xmpp_parsers::message::Lang>,
    principal: waddle_xmpp::auth::AuthenticatedPrincipalRef,
    sequence: Option<OrderedRelayInboundSequence>,
    origin: Option<OrderedRelayRouteOrigin>,
    client_room_origin: Option<ClientRoomOrigin>,
    backstop: StanzaBackstop,
}

async fn dispatch_authoritative_message(
    request: MessageIngressDispatch,
    state: &WebSocketState,
    conn: &mut WsConnState,
    admission: Option<(&NodeAdmissionPermit, &tokio_util::sync::CancellationToken)>,
) -> ResponseBatch {
    use crate::ingress::{IngressDecisionClass, IngressStreamIdentity, IngressSubmission};
    use waddle_xmpp::ingress::{DigestContext, NormalizedTarget, WireHandledCount};
    let MessageIngressDispatch {
        message,
        stanza_lang,
        principal,
        sequence,
        origin,
        client_room_origin,
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
        let (digest_input, rejection) =
            match crate::ingress::submission::digest_input(&message, &digest_context) {
                Ok(input) => (input, None),
                Err(
                    waddle_xmpp::ingress::DigestInputError::DuplicateOriginId
                    | waddle_xmpp::ingress::DigestInputError::MalformedOriginId
                    | waddle_xmpp::ingress::DigestInputError::DuplicateThread
                    | waddle_xmpp::ingress::DigestInputError::DuplicateReply
                    | waddle_xmpp::ingress::DigestInputError::ReplyMalformed,
                ) => match waddle_xmpp::ingress::DigestInput::from_rejected_parsed(
                    &message,
                    &digest_context,
                ) {
                    Ok(input) => (input, Some(DigestRejection::Malformed)),
                    Err(_) => (
                        resource_rejection_digest(&message, &digest_context)?,
                        Some(DigestRejection::Resource),
                    ),
                },
                Err(waddle_xmpp::ingress::DigestInputError::ForgedServerStanzaId { .. }) => {
                    return Err(IngressDecisionClass::Storage);
                }
                Err(_) => (
                    resource_rejection_digest(&message, &digest_context)?,
                    Some(DigestRejection::Resource),
                ),
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
        let plan = match rejection {
            Some(DigestRejection::Resource) => reject_resource_message(message, &sender),
            Some(DigestRejection::Malformed) => {
                crate::server::routes::interpret::reject_malformed_message(message, &sender)
            }
            None => {
                crate::server::routes::interpret::plan_message_dispatch_with_client_origin(
                    machine,
                    message,
                    &deps,
                    client_room_origin.as_ref(),
                )
                .await
            }
        };
        #[cfg(test)]
        if let crate::server::routes::interpret::effects::RoomExecutionPath::Local {
            client_origin,
            ..
        } = &plan.room_execution
        {
            let _ = TEST_LOCAL_ROOM_PLAN_ORIGINS.try_with(|captured| {
                captured
                    .lock()
                    .expect("room plan origin capture")
                    .push(client_origin.clone());
            });
        }
        let submission = IngressSubmission {
            sender,
            identity,
            principal: IngressPrincipal::Authenticated(principal),
            target,
            plan,
            digest_input,
            connection_generation: waddle_xmpp::ingress::TransportGeneration::Connection(
                conn.ingress_generation,
            ),
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
                Ok(Err(class)) => {
                    waddle_xmpp::telemetry::reliability::increment_ingress_decision(class);
                    class
                }
                Err(error) => {
                    let class = match error {
                        StanzaTimeout::AdmissionRevoked => IngressDecisionClass::ClaimFenceMissing,
                        StanzaTimeout::Unhandled | StanzaTimeout::HandledIq(_) => {
                            IngressDecisionClass::Timeout
                        }
                    };
                    // Cancellation drops planning/commit before commit_submission
                    // can meter its result. Returned decisions are already metered.
                    waddle_xmpp::telemetry::reliability::increment_ingress_decision(class);
                    class
                }
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
            .execute(&decision, &ImmediateSink, &deps),
        std::time::Duration::from_secs(5),
    )
    .await
}

pub(super) async fn execute_committed_message(
    execution: impl std::future::Future<Output = ExecutionReport>,
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
            // The ordered batch discharges its receipts once its last frame is
            // written. Carry that obligation on the frame itself so a transport
            // failure retains it with the XEP-0198 replay entry.
            if let Some(frame) = batch.frames.pop() {
                batch
                    .frames
                    .push(frame.with_ingress_receipts(report.frame_receipts()));
            }
            batch.ingress_reports.push(report);
            batch
        }
        Err(_) => ResponseBatch::default(),
    }
}

fn ingress_failure_stream_error(class: IngressDecisionClass) -> Element {
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

/// Limit failures cannot pass through the bounded semantic digest again. Hash
/// the complete offered XML at the frame boundary into a bounded rejection-only
/// surrogate; keep the original typed stanza in the committed envelope.
fn resource_rejection_digest(
    message: &xmpp_parsers::message::Message,
    context: &waddle_xmpp::ingress::DigestContext,
) -> Result<waddle_xmpp::ingress::DigestInput, IngressDecisionClass> {
    use sha2::Digest;
    struct DigestWriter(sha2::Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(sha2::Sha256::new());
    writer.0.update(b"waddle:resource-rejected-frame:v1\0");
    let lang = context
        .stanza_lang
        .as_ref()
        .map_or("", |lang| lang.0.as_str());
    writer.0.update([u8::from(context.stanza_lang.is_some())]);
    writer.0.update((lang.len() as u64).to_be_bytes());
    writer.0.update(lang.as_bytes());
    Element::from(message.clone())
        .write_to(&mut writer)
        .map_err(|_| IngressDecisionClass::Storage)?;
    let mut surrogate = xmpp_parsers::message::Message::new(None);
    surrogate
        .bodies
        .insert(Default::default(), hex::encode(writer.0.finalize()));
    let context = waddle_xmpp::ingress::DigestContext {
        target: context.target.clone(),
        server_authorities: vec![],
        stanza_lang: None,
    };
    waddle_xmpp::ingress::DigestInput::from_rejected_parsed(&surrogate, &context)
        .map_err(|_| IngressDecisionClass::Storage)
}

fn reject_resource_message(
    message: xmpp_parsers::message::Message,
    sender: &jid::FullJid,
) -> IngressPlan {
    use crate::server::routes::interpret::effects::{
        Effect, ExternalEffect, PlanRejection, PlannedEffect, PolicyDeniedReason,
    };
    let mut plan = crate::server::routes::interpret::reject_malformed_message(message, sender);
    // RFC 6120 §8.3.1: retain silent handling for an offered error stanza.
    if plan.error_reply.is_none() {
        return plan;
    }
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Wait,
        xmpp_parsers::stanza_error::DefinedCondition::ResourceConstraint,
        "en",
        "The message exceeds the ingress resource limit.",
    );
    let reply = Stanza::Message(
        waddle_xmpp::protocol::handlers::errors::message_error_reply(
            &plan.sanitized_message,
            error.clone(),
        ),
    );
    plan.rejection = Some(PlanRejection::PolicyDenied(
        PolicyDeniedReason::CaptureOverflow,
    ));
    plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Frame(
        Box::new(reply.clone()),
    )))];
    plan.intents = vec![waddle_xmpp::ingress::IngressEffectIntent::ErrorReply {
        recipient: sender.clone(),
        error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error)
            .expect("typed standard error"),
    }];
    plan.error_reply = Some(reply);
    plan
}

#[cfg(test)]
#[path = "tests/ingress_frame_limits.rs"]
mod ingress_frame_limits;
