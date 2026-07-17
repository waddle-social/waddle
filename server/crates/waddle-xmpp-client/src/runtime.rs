use std::collections::VecDeque;

use crate::bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, RequiredStreamFeature,
    ResourceBindingRequest,
};
use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent, LifecycleEvent};
use crate::request::{ClientRequest, PendingRequest, RequestTracker, StanzaId};
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::{AckRequest, SmResumeState, SmState};
use crate::transport::{StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState};
use crate::AuthenticationConfig;
use minidom::Element;

#[cfg(not(target_arch = "wasm32"))]
pub fn monotonic_now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(target_arch = "wasm32")]
pub fn monotonic_now_ms() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now().max(0.0) as u64)
        .unwrap_or_else(|| js_sys::Date::now().max(0.0) as u64)
}

mod app_stanza;
mod sm;
#[cfg(test)]
mod tests;

/// RFC 7395 stream close is a two-half exchange. Five seconds bounds a
/// confirmed local half-close without waiting long enough to consume the
/// XEP-0198 30-second no-progress budget; expiry is an unfinished stream so
/// its resumable state remains authoritative.
pub const RFC7395_PEER_CLOSE_TIMEOUT_MS: u64 = 5_000;

/// High-level lifecycle of the scaffolded runtime wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Prepared,
    Active,
}

/// Stateful request/session coordinator for the client runtime boundary.
#[derive(Debug)]
pub struct XmppRuntime {
    config: ClientConfig,
    snapshot: SessionSnapshot,
    requests: RequestTracker,
    bootstrap: BootstrapState,
    next_bootstrap_stanza: u64,
    sm_state: SmState,
    sm_advertised: bool,
    pending_fallback_retries: VecDeque<Element>,
    fallback_resume_state: Option<SmResumeState>,
    fallback_retry_writes_in_flight: VecDeque<Element>,
    /// Ack-request metadata waiting for transport confirmation of the exact
    /// generated `<r/>`. Keeping this inside the typed runtime prevents a
    /// driver from publishing `AckRequestSent` before the write succeeds.
    pending_ack_request: Option<AckRequest>,
    stream_close: StreamCloseHandshake,
}

#[derive(Debug, Default)]
struct StreamCloseHandshake {
    requested: bool,
    sent_confirmed: bool,
    sent_confirmed_at_ms: Option<u64>,
    received: bool,
    complete: bool,
    peer_stream_error_received: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BootstrapState {
    Idle,
    AwaitingTransport,
    AwaitingStreamOpen { authenticated: bool },
    AwaitingFeatures { authenticated: bool },
    AwaitingAuthenticationOutcome,
    AwaitingBindResult { stanza_id: StanzaId },
    AwaitingResume,
    Ready,
}

impl XmppRuntime {
    pub fn new(config: ClientConfig) -> ClientResult<Self> {
        config.validate()?;
        let sm_state = config
            .session
            .stream_management
            .resume_state
            .as_ref()
            .map(SmState::from_resume_state)
            .unwrap_or_default();

        Ok(Self {
            config,
            snapshot: SessionSnapshot::new(),
            requests: RequestTracker::default(),
            bootstrap: BootstrapState::Idle,
            next_bootstrap_stanza: 0,
            sm_state,
            sm_advertised: false,
            pending_fallback_retries: VecDeque::new(),
            fallback_resume_state: None,
            fallback_retry_writes_in_flight: VecDeque::new(),
            pending_ack_request: None,
            stream_close: StreamCloseHandshake::default(),
        })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn resume_state(&self) -> Option<SmResumeState> {
        if self.fallback_resume_state.is_some()
            && (!self.pending_fallback_retries.is_empty()
                || !self.fallback_retry_writes_in_flight.is_empty())
        {
            return self.fallback_resume_state.clone();
        }

        self.sm_state.resume_state()
    }

    /// True only after both directions of the typed RFC 7395 `<close/>`
    /// exchange are confirmed. Drivers use this edge to begin RFC 6455 close.
    pub fn stream_close_complete(&self) -> bool {
        self.stream_close.complete
    }

    /// True after the local RFC 7395 `<close/>` has reached the transport.
    /// From this edge no later XMPP XML may be emitted on the stream.
    pub fn stream_close_sent_confirmed(&self) -> bool {
        self.stream_close.sent_confirmed
    }

    /// True once either a public request or a peer close has claimed the one
    /// local `<close/>` write for this stream.
    pub fn stream_close_requested(&self) -> bool {
        self.stream_close.requested
    }

    /// Remaining delay before a confirmed local half-close becomes an
    /// unfinished stream. No deadline exists after the peer half arrives.
    pub fn next_stream_close_wakeup_in_ms(&self, now_ms: u64) -> Option<u64> {
        let sent_at = self.stream_close.sent_confirmed_at_ms?;
        if self.stream_close.received || self.stream_close.complete {
            return None;
        }
        Some(RFC7395_PEER_CLOSE_TIMEOUT_MS.saturating_sub(now_ms.saturating_sub(sent_at)))
    }

    pub fn stream_close_timed_out_at(&self, now_ms: u64) -> bool {
        self.next_stream_close_wakeup_in_ms(now_ms) == Some(0)
    }

    pub fn pending_requests(&self) -> Vec<PendingRequest> {
        self.requests.snapshot()
    }

    pub fn status(&self) -> RuntimeStatus {
        match self.snapshot.phase {
            SessionPhase::Disconnected => RuntimeStatus::Prepared,
            _ => RuntimeStatus::Active,
        }
    }

    pub fn can_send_app_stanza(&self) -> bool {
        if !matches!(self.snapshot.phase, SessionPhase::Established) {
            return false;
        }

        let sm_cfg = &self.config.session.stream_management;
        if sm_cfg.enable_stream_management && self.sm_advertised {
            return self.sm_state.enabled;
        }

        true
    }

    pub fn queue_request(&mut self, request: ClientRequest) -> ClientResult<Vec<ClientEvent>> {
        let previous = self.snapshot.clone();
        let pending = self.requests.register(request)?;
        self.snapshot.pending_requests = self.requests.pending_len();

        let mut events = vec![ClientEvent::RequestQueued(pending.clone())];

        match pending.kind() {
            crate::request::RequestKind::Connect => {
                self.bootstrap = BootstrapState::AwaitingTransport;
                self.set_phase(SessionPhase::Connecting)?;
            }
            crate::request::RequestKind::Disconnect => {
                self.begin_local_stream_close(&mut events)?;
            }
            _ => {}
        }

        self.push_state_change(previous, &mut events);

        Ok(events)
    }

    /// Claim and emit the one local RFC 7395 close frame. Repeated native,
    /// WASM, and standalone-runtime requests coalesce without another write.
    pub fn request_stream_close(&mut self) -> ClientResult<Vec<ClientEvent>> {
        let previous = self.snapshot.clone();
        let mut events = Vec::new();
        self.begin_local_stream_close(&mut events)?;
        self.push_state_change(previous, &mut events);
        Ok(events)
    }

    pub fn bind_session(&mut self, binding: SessionBinding) -> ClientResult<Vec<ClientEvent>> {
        let previous = self.snapshot.clone();
        let mut events = Vec::new();
        self.finish_binding(binding, &mut events)?;
        self.push_state_change(previous, &mut events);
        Ok(events)
    }

    pub fn resolve_request(
        &mut self,
        request_id: crate::request::RequestId,
    ) -> ClientResult<ClientEvent> {
        let pending = self.requests.resolve(request_id)?;
        self.snapshot.pending_requests = self.requests.pending_len();
        Ok(self.resolved_event(pending))
    }

    pub fn resolve_request_by_stanza_id(
        &mut self,
        stanza_id: &StanzaId,
    ) -> ClientResult<ClientEvent> {
        let pending = self.requests.resolve_by_stanza_id(stanza_id)?;
        self.snapshot.pending_requests = self.requests.pending_len();
        Ok(self.resolved_event(pending))
    }

    pub fn apply_transport_event(
        &mut self,
        event: TransportEvent,
    ) -> ClientResult<Vec<ClientEvent>> {
        self.apply_transport_event_at(event, monotonic_now_ms())
    }

    /// Apply one transport event at an injected monotonic timestamp.
    /// Drivers and deterministic SM tests use this to share one cadence on
    /// native and WASM without putting a platform clock inside [`SmState`].
    pub fn apply_transport_event_at(
        &mut self,
        event: TransportEvent,
        now_ms: u64,
    ) -> ClientResult<Vec<ClientEvent>> {
        let previous = self.snapshot.clone();
        let mut events = vec![ClientEvent::Transport(event.clone())];
        self.apply_transport_side_effects(event, now_ms, &mut events)?;
        if self.stream_close.sent_confirmed {
            events.retain(|event| {
                !matches!(
                    event,
                    ClientEvent::Connection(ConnectionEvent::OutboundMessage(_))
                )
            });
        }
        self.push_state_change(previous, &mut events);
        Ok(events)
    }

    /// Poll the XEP-0198 acknowledgement cadence at an injected monotonic
    /// timestamp. Returned events are transport instructions and content-free
    /// telemetry; drivers must abort uncleanly when the stalled event appears.
    pub fn poll_stream_management_at(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        if self.stream_close.sent_confirmed || self.stream_close.peer_stream_error_received {
            return Vec::new();
        }
        let poll = self.sm_state.poll_ack_timer_at(now_ms);
        let mut events = Vec::new();
        if poll.request_timed_out {
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                crate::event::StreamManagementEvent::AckRequestTimedOut {
                    unacked: self.sm_state.unacked_count(),
                },
            )));
        }
        if let Some(request) = poll.request {
            self.push_ack_request(request, &mut events);
        }
        if let Some(elapsed_ms) = poll.progress_stalled_ms {
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                crate::event::StreamManagementEvent::AckProgressStalled {
                    unacked: self.sm_state.unacked_count(),
                    elapsed_ms,
                },
            )));
        }
        events
    }

    pub fn next_stream_management_wakeup_in_ms(&self, now_ms: u64) -> Option<u64> {
        if self.stream_close.sent_confirmed || self.stream_close.peer_stream_error_received {
            return None;
        }
        self.sm_state.next_ack_wakeup_in_ms(now_ms)
    }

    /// Best-effort pagehide hook. It shares the normal request-in-flight gate
    /// and therefore never emits a second concurrent `<r/>`.
    pub fn request_stream_management_ack_at(&mut self, now_ms: u64) -> Vec<ClientEvent> {
        if self.stream_close.sent_confirmed || self.stream_close.peer_stream_error_received {
            return Vec::new();
        }
        let mut events = Vec::new();
        if let Some(request) = self.sm_state.request_ack_now_at(now_ms) {
            self.push_ack_request(request, &mut events);
        }
        events
    }

    fn resolved_event(&self, pending: PendingRequest) -> ClientEvent {
        ClientEvent::RequestResolved(pending.correlation())
    }

    fn apply_transport_side_effects(
        &mut self,
        event: TransportEvent,
        now_ms: u64,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        self.snapshot.transport = event.transport_state();

        match event {
            TransportEvent::StateChanged(TransportState::Connecting) => {
                self.set_phase(SessionPhase::Connecting)?;
            }
            TransportEvent::StateChanged(TransportState::Open) => {
                if matches!(self.bootstrap, BootstrapState::AwaitingTransport) {
                    self.begin_stream_open(false, events)?;
                }
            }
            TransportEvent::MessageReceived(TransportMessage::Open(open)) => {
                self.handle_stream_open(open, events)?;
            }
            TransportEvent::MessageReceived(TransportMessage::Element(element)) => {
                self.handle_received_element(element, now_ms, events)?;
            }
            TransportEvent::MessageSent(TransportMessage::Element(element)) => {
                self.handle_sent_element(&element, now_ms, events)?;
            }
            TransportEvent::MessageReceived(TransportMessage::Close(_)) => {
                self.stream_close.received = true;
                self.set_phase(SessionPhase::Disconnecting)?;
                if !self.stream_close.requested {
                    self.stream_close.requested = true;
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Close(StreamClose),
                    )));
                }
                self.finish_stream_close_if_complete()?;
            }
            TransportEvent::MessageSent(TransportMessage::Close(_)) => {
                self.stream_close.requested = true;
                self.stream_close.sent_confirmed = true;
                self.stream_close.sent_confirmed_at_ms.get_or_insert(now_ms);
                self.pending_ack_request = None;
                self.set_phase(SessionPhase::Disconnecting)?;
                self.finish_stream_close_if_complete()?;
            }
            TransportEvent::StateChanged(TransportState::Closed) | TransportEvent::Closed => {
                self.pending_ack_request = None;
                self.snapshot.binding = None;
                self.bootstrap = BootstrapState::Idle;
                self.sm_state.stop();
                self.set_phase(SessionPhase::Disconnected)?;
            }
            TransportEvent::StateChanged(TransportState::Closing) => {
                self.set_phase(SessionPhase::Disconnecting)?;
            }
            TransportEvent::StateChanged(TransportState::Failed) => {
                self.pending_ack_request = None;
            }
            TransportEvent::StateChanged(TransportState::Idle) | TransportEvent::MessageSent(_) => {
            }
        }

        Ok(())
    }

    fn begin_local_stream_close(&mut self, events: &mut Vec<ClientEvent>) -> ClientResult<()> {
        self.set_phase(SessionPhase::Disconnecting)?;
        if self.stream_close.requested || self.stream_close.sent_confirmed {
            return Ok(());
        }
        self.stream_close.requested = true;
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Close(StreamClose),
        )));
        Ok(())
    }

    fn finish_stream_close_if_complete(&mut self) -> ClientResult<()> {
        if self.stream_close.complete
            || !self.stream_close.sent_confirmed
            || !self.stream_close.received
        {
            return Ok(());
        }

        self.stream_close.complete = true;
        self.pending_ack_request = None;
        self.pending_fallback_retries.clear();
        self.fallback_resume_state = None;
        self.fallback_retry_writes_in_flight.clear();
        self.sm_state = SmState::new();
        self.snapshot.binding = None;
        self.bootstrap = BootstrapState::Idle;
        self.set_phase(SessionPhase::Disconnecting)
    }

    fn handle_sent_element(
        &mut self,
        element: &Element,
        now_ms: u64,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if element.ns() == crate::stream_management::NS_SM && element.name() == "enable" {
            self.sm_state.start_outbound();
            return Ok(());
        }

        if SmState::is_request_ack(element) {
            if let Some(request) = self.pending_ack_request.take() {
                if self.sm_state.confirm_ack_request_sent_at(now_ms) {
                    events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                        crate::event::StreamManagementEvent::AckRequestSent {
                            attempt: request.attempt,
                            unacked: request.unacked,
                        },
                    )));
                }
            }
            return Ok(());
        }

        let sent = self.sm_state.record_sent_stanza_at(element, now_ms)?;
        if let Some(request) = sent.request {
            self.push_ack_request(request, events);
        }
        self.mark_fallback_retry_sent(element);
        Ok(())
    }

    fn handle_stream_open(
        &mut self,
        open: StreamOpen,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if let BootstrapState::AwaitingStreamOpen { authenticated } = self.bootstrap {
            self.bootstrap = BootstrapState::AwaitingFeatures { authenticated };
            events.push(ClientEvent::Connection(ConnectionEvent::StreamOpened(open)));
        }

        Ok(())
    }

    fn handle_received_element(
        &mut self,
        element: minidom::Element,
        now_ms: u64,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        use crate::stream_management::NS_SM;

        // RFC 6120 §4.9.1.1: a peer stream error is terminal for the
        // current XML stream. The typed RFC 7395 close is the only XML
        // this side may emit after observing it; later application and SM
        // elements are ignored while the framing close/timeout completes.
        if self.stream_close.peer_stream_error_received {
            return Ok(());
        }

        if element.ns() == NS_SM {
            self.handle_sm_element(&element, now_ms, events)?;
            return Ok(());
        }

        if element.name() == "error" && element.ns() == crate::bootstrap::NS_STREAMS {
            self.handle_stream_error_element(&element, events)?;
            return Ok(());
        }

        // Once bootstrap is complete, route to the app-level stanza dispatcher.
        if matches!(self.bootstrap, BootstrapState::Ready) {
            let app_events = self.handle_app_stanza(&element);
            // A stanza that requires a response cannot be accepted after our
            // confirmed RFC 7395 close half: emitting the response would put
            // XML after `<close/>`, while advancing SM h without it would lie
            // about handled work. Preserve it for server replay instead.
            if self.stream_close_sent_confirmed()
                && app_events.iter().any(|event| {
                    matches!(
                        event,
                        ClientEvent::Connection(ConnectionEvent::OutboundMessage(_))
                    )
                })
            {
                return Ok(());
            }
            if self.sm_state.enabled && matches!(element.name(), "iq" | "message" | "presence") {
                self.sm_state.record_received(1);
            }
            events.extend(app_events);
            return Ok(());
        }

        // Track any countable bootstrap stanza only after deciding that it is
        // not application work fenced by the local close half above.
        if self.sm_state.enabled && matches!(element.name(), "iq" | "message" | "presence") {
            self.sm_state.record_received(1);
        }

        let Some(parsed) = BootstrapElement::parse(&element) else {
            return Ok(());
        };

        match parsed? {
            BootstrapElement::StreamFeatures(features) => {
                self.handle_stream_features(features, events)?;
            }
            BootstrapElement::SaslSuccess => {
                self.handle_auth_success(events)?;
            }
            BootstrapElement::SaslFailure(failure) => {
                self.handle_auth_failure(failure, events)?;
            }
            BootstrapElement::ResourceBindingResult(result) => {
                self.handle_bind_result(result, events)?;
            }
        }

        Ok(())
    }

    fn push_ack_request(
        &mut self,
        request: crate::stream_management::AckRequest,
        events: &mut Vec<ClientEvent>,
    ) {
        debug_assert!(
            self.pending_ack_request.is_none(),
            "SM permits only one generated ack request awaiting a write"
        );
        self.pending_ack_request = Some(request);
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Element(SmState::build_request_ack()),
        )));
    }

    fn handle_stream_features(
        &mut self,
        features: crate::StreamFeatures,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        events.push(ClientEvent::Connection(
            ConnectionEvent::FeaturesAdvertised(features.clone()),
        ));

        match self.bootstrap {
            BootstrapState::AwaitingFeatures {
                authenticated: false,
            } => {
                if !features.supports(AuthMechanism::OAuthBearer) {
                    return Err(ClientError::MissingStreamFeature {
                        feature: RequiredStreamFeature::Authentication(AuthMechanism::OAuthBearer),
                    });
                }

                let request = AuthenticationRequest::from_config(self.oauth_config());
                self.bootstrap = BootstrapState::AwaitingAuthenticationOutcome;
                self.set_phase(SessionPhase::Authenticating)?;
                events.push(ClientEvent::Connection(
                    ConnectionEvent::AuthenticationRequested(request.clone()),
                ));
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    request.to_transport_message(),
                )));
            }
            BootstrapState::AwaitingFeatures {
                authenticated: true,
            } => {
                if features.stream_management {
                    self.sm_advertised = true;
                }

                let sm_cfg = &self.config.session.stream_management;
                if sm_cfg.enable_stream_management
                    && sm_cfg.allow_resume
                    && features.stream_management
                    && self.sm_state.previd.is_some()
                {
                    let resume = SmState::build_resume(
                        self.sm_state.previd.as_deref().unwrap_or_default(),
                        self.sm_state.inbound_count,
                    );
                    self.bootstrap = BootstrapState::AwaitingResume;
                    self.set_phase(SessionPhase::Resuming)?;
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Element(resume),
                    )));
                    return Ok(());
                }

                if !features.bind {
                    return Err(ClientError::MissingStreamFeature {
                        feature: RequiredStreamFeature::ResourceBinding,
                    });
                }

                self.request_resource_binding(events)?;
            }
            _ => {}
        }

        Ok(())
    }

    fn request_resource_binding(&mut self, events: &mut Vec<ClientEvent>) -> ClientResult<()> {
        let request = ResourceBindingRequest::new(
            self.next_bootstrap_stanza_id()?,
            self.oauth_config().resource.clone(),
        );
        self.bootstrap = BootstrapState::AwaitingBindResult {
            stanza_id: request.stanza_id.clone(),
        };
        self.set_phase(SessionPhase::Binding)?;
        events.push(ClientEvent::Connection(
            ConnectionEvent::ResourceBindingRequested(request.clone()),
        ));
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            request.to_transport_message(),
        )));
        Ok(())
    }

    fn handle_auth_success(&mut self, events: &mut Vec<ClientEvent>) -> ClientResult<()> {
        if matches!(
            self.bootstrap,
            BootstrapState::AwaitingAuthenticationOutcome
        ) {
            events.push(ClientEvent::Connection(
                ConnectionEvent::AuthenticationSucceeded,
            ));
            self.begin_stream_open(true, events)?;
        }

        Ok(())
    }

    fn handle_auth_failure(
        &mut self,
        failure: crate::SaslFailure,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if matches!(
            self.bootstrap,
            BootstrapState::AwaitingAuthenticationOutcome
        ) {
            self.bootstrap = BootstrapState::Idle;
            self.set_phase(SessionPhase::Disconnecting)?;
            events.push(ClientEvent::Connection(
                ConnectionEvent::AuthenticationFailed(failure),
            ));
        }

        Ok(())
    }

    fn handle_bind_result(
        &mut self,
        result: crate::ResourceBindingResult,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        let expected = match &self.bootstrap {
            BootstrapState::AwaitingBindResult { stanza_id } => Some(stanza_id.clone()),
            _ => None,
        };

        if let Some(expected) = expected {
            if result.stanza_id != expected {
                return Ok(());
            }

            let binding = result.into_session_binding(None, false);
            self.finish_binding(binding, events)?;
        }

        Ok(())
    }

    fn begin_stream_open(
        &mut self,
        authenticated: bool,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        let open = StreamOpen::from_config(&self.config);
        self.bootstrap = BootstrapState::AwaitingStreamOpen { authenticated };
        self.set_phase(SessionPhase::OpeningStream)?;
        events.push(ClientEvent::Connection(ConnectionEvent::StreamOpening(
            open.clone(),
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
            TransportMessage::Open(open),
        )));
        Ok(())
    }

    fn finish_binding(
        &mut self,
        binding: SessionBinding,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        self.snapshot.binding = Some(binding.clone());
        self.bootstrap = BootstrapState::Ready;
        self.set_phase(SessionPhase::Established)?;

        events.push(ClientEvent::Connection(ConnectionEvent::ResourceBound(
            binding.clone(),
        )));
        events.push(ClientEvent::Lifecycle(LifecycleEvent::SessionBound(
            binding.clone(),
        )));
        events.push(ClientEvent::Connection(ConnectionEvent::SessionReady(
            binding.clone(),
        )));
        events.push(ClientEvent::Lifecycle(LifecycleEvent::SessionReady(
            binding,
        )));

        let sm_cfg = &self.config.session.stream_management;
        if sm_cfg.enable_stream_management && self.sm_advertised {
            let enable = SmState::build_enable_with_max(sm_cfg.allow_resume, sm_cfg.resume_max);
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(enable),
            )));
        }

        Ok(())
    }

    pub(super) fn flush_pending_fallback_retries(&mut self, events: &mut Vec<ClientEvent>) {
        while let Some(element) = self.pending_fallback_retries.pop_front() {
            self.fallback_retry_writes_in_flight
                .push_back(element.clone());
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element),
            )));
        }
    }

    fn mark_fallback_retry_sent(&mut self, element: &Element) {
        if self
            .fallback_retry_writes_in_flight
            .front()
            .is_some_and(|retry| retry == element)
        {
            self.fallback_retry_writes_in_flight.pop_front();
            if self.pending_fallback_retries.is_empty()
                && self.fallback_retry_writes_in_flight.is_empty()
            {
                self.fallback_resume_state = None;
            }
        }
    }

    fn oauth_config(&self) -> &crate::OAuthBearerConfig {
        match &self.config.auth {
            AuthenticationConfig::OAuthBearer(config) => config,
        }
    }

    fn next_bootstrap_stanza_id(&mut self) -> ClientResult<StanzaId> {
        self.next_bootstrap_stanza = self
            .next_bootstrap_stanza
            .checked_add(1)
            .ok_or(ClientError::RequestIdExhausted)?;
        StanzaId::new(format!("bind-{}", self.next_bootstrap_stanza))
    }

    fn push_state_change(&self, previous: SessionSnapshot, events: &mut Vec<ClientEvent>) {
        if previous != self.snapshot {
            events.push(ClientEvent::Lifecycle(LifecycleEvent::StateChanged(
                self.snapshot.clone(),
            )));
        }
    }

    fn set_phase(&mut self, next: SessionPhase) -> ClientResult<()> {
        let current = self.snapshot.phase;
        if current == next {
            return Ok(());
        }

        let valid = matches!(
            (current, next),
            (SessionPhase::Disconnected, SessionPhase::Connecting)
                | (SessionPhase::Connecting, SessionPhase::OpeningStream)
                | (SessionPhase::OpeningStream, SessionPhase::Authenticating)
                | (SessionPhase::Authenticating, SessionPhase::OpeningStream)
                | (SessionPhase::OpeningStream, SessionPhase::Binding)
                | (SessionPhase::OpeningStream, SessionPhase::Resuming)
                | (SessionPhase::Authenticating, SessionPhase::Binding)
                | (SessionPhase::Resuming, SessionPhase::Binding)
                | (SessionPhase::Binding, SessionPhase::Established)
                | (SessionPhase::Established, SessionPhase::Resuming)
                | (SessionPhase::Resuming, SessionPhase::Established)
                | (SessionPhase::Resuming, SessionPhase::Disconnecting)
                | (SessionPhase::Established, SessionPhase::Disconnecting)
                | (SessionPhase::Connecting, SessionPhase::Disconnecting)
                | (SessionPhase::OpeningStream, SessionPhase::Disconnecting)
                | (SessionPhase::Authenticating, SessionPhase::Disconnecting)
                | (SessionPhase::Binding, SessionPhase::Disconnecting)
                | (SessionPhase::Disconnecting, SessionPhase::Disconnected)
                | (_, SessionPhase::Disconnected)
        );

        if !valid {
            return Err(ClientError::InvalidPhaseTransition {
                from: current,
                to: next,
            });
        }

        self.snapshot.phase = next;
        Ok(())
    }
}
