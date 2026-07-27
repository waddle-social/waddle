use std::collections::VecDeque;

use crate::bootstrap::{
    AuthMechanism, AuthenticationRequest, BootstrapElement, RequiredStreamFeature,
    ResourceBindingRequest,
};
use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent, LifecycleEvent, StreamManagementEvent};
use crate::request::{ClientRequest, PendingRequest, RequestTracker, StanzaId};
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::{SmAcknowledgementClockAction, SmResumeState, SmState};
use crate::transport::{StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState};
use crate::AuthenticationConfig;
use chrono::{DateTime, Utc};
use minidom::Element;

mod app_stanza;
mod sm;
#[cfg(test)]
mod tests;

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
    sm_negotiation: SmNegotiationState,
    sm_advertised: bool,
    pending_fallback_retries: VecDeque<Element>,
    fallback_resume_state: Option<SmResumeState>,
    fallback_retry_writes_in_flight: VecDeque<Element>,
}

/// Client-side XEP-0198 negotiation ownership for the current stream.
///
/// This is deliberately distinct from the SM counters: sending `<enable/>`
/// starts the sender counter, while the session becomes enabled only after the
/// peer's matching `<enabled/>` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SmNegotiationState {
    #[default]
    Inactive,
    AwaitingEnableResponse,
    Enabled,
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
            sm_negotiation: SmNegotiationState::Inactive,
            sm_advertised: false,
            pending_fallback_retries: VecDeque::new(),
            fallback_resume_state: None,
            fallback_retry_writes_in_flight: VecDeque::new(),
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

    /// Whether a host timer must continue polling the Rust-owned XEP-0198
    /// acknowledgement policy.
    pub fn acknowledgement_clock_pending(&self) -> bool {
        self.sm_state.acknowledgement_clock_pending()
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
                self.bootstrap = BootstrapState::Idle;
                self.set_phase(SessionPhase::Disconnecting)?;
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Close(StreamClose),
                )));
            }
            _ => {}
        }

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
        let previous = self.snapshot.clone();
        let mut events = vec![ClientEvent::Transport(event.clone())];
        self.apply_transport_side_effects(event, &mut events)?;
        self.push_state_change(previous, &mut events);
        Ok(events)
    }

    /// Drive the Rust-owned XEP-0198 acknowledgement clock. Hosts supply time
    /// so browser and native timer implementations cannot redefine protocol
    /// cadence and tests can exercise every boundary deterministically.
    pub fn poll_stream_management_clock(&mut self, now: DateTime<Utc>) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        for action in self.sm_state.poll_acknowledgement_clock(now) {
            match action {
                SmAcknowledgementClockAction::Retry { attempt } => {
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Element(SmState::build_request_ack()),
                    )));
                    events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                        StreamManagementEvent::AckRetry { attempt },
                    )));
                }
                SmAcknowledgementClockAction::RequestTimedOut => {
                    events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                        StreamManagementEvent::AckRequestTimedOut,
                    )));
                }
                SmAcknowledgementClockAction::ProgressTimedOut => {
                    // A reconnect terminates this stream's acknowledgement
                    // policy. Actions returned earlier in the same poll (such
                    // as a retry on the five-second plateau) must not reach
                    // the transport after the terminal decision.
                    self.sm_state.cancel_acknowledgement_clock();
                    events.clear();
                    events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                        StreamManagementEvent::ReconnectRequired,
                    )));
                    events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                        TransportMessage::Close(StreamClose),
                    )));
                    break;
                }
            }
        }
        events
    }

    /// Request a peer acknowledgement through the runtime-owned XEP-0198
    /// builder. Browser callers use this during `pagehide`; the host never
    /// constructs XML or redefines whether the current stream can carry SM.
    pub fn request_stream_management_ack(&self) -> Vec<ClientEvent> {
        if !self.sm_state.enabled {
            return Vec::new();
        }
        vec![
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Element(
                SmState::build_request_ack(),
            ))),
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested {
                    reason: crate::event::SmAckRequestReason::Pagehide,
                },
            )),
        ]
    }

    fn resolved_event(&self, pending: PendingRequest) -> ClientEvent {
        ClientEvent::RequestResolved(pending.correlation())
    }

    fn apply_transport_side_effects(
        &mut self,
        event: TransportEvent,
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
                self.handle_received_element(element, events)?;
            }
            TransportEvent::MessageSent(TransportMessage::Element(element)) => {
                self.handle_sent_element(&element, events);
            }
            TransportEvent::MessageReceived(TransportMessage::Close(_))
            | TransportEvent::StateChanged(TransportState::Closed)
            | TransportEvent::Closed => {
                self.snapshot.binding = None;
                self.bootstrap = BootstrapState::Idle;
                self.sm_state.stop();
                self.sm_negotiation = SmNegotiationState::Inactive;
                self.set_phase(SessionPhase::Disconnected)?;
            }
            TransportEvent::StateChanged(TransportState::Closing) => {
                self.set_phase(SessionPhase::Disconnecting)?;
            }
            TransportEvent::StateChanged(TransportState::Idle)
            | TransportEvent::MessageSent(_)
            | TransportEvent::StateChanged(TransportState::Failed) => {}
        }

        Ok(())
    }

    fn handle_sent_element(&mut self, element: &Element, events: &mut Vec<ClientEvent>) {
        if element.ns() == crate::stream_management::NS_SM && element.name() == "enable" {
            if matches!(self.bootstrap, BootstrapState::Ready)
                && self.snapshot.binding.is_some()
                && matches!(self.sm_negotiation, SmNegotiationState::Inactive)
            {
                self.sm_state.start_outbound();
                self.sm_negotiation = SmNegotiationState::AwaitingEnableResponse;
            }
            return;
        }

        if self.sm_state.outbound_enabled
            && matches!(element.name(), "iq" | "message" | "presence")
            && self.sm_state.record_sent_stanza(element)
        {
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(SmState::build_request_ack()),
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested {
                    reason: crate::event::SmAckRequestReason::OutboundStanza,
                },
            )));
        }
        self.mark_fallback_retry_sent(element);
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
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        use crate::stream_management::NS_SM;

        if element.ns() == NS_SM {
            self.handle_sm_element(&element, events)?;
            return Ok(());
        }

        if element.name() == "error" && element.ns() == crate::bootstrap::NS_STREAMS {
            self.handle_stream_error_element(&element, events);
            return Ok(());
        }

        // Track inbound stanzas for SM once enabled.
        if self.sm_state.enabled && matches!(element.name(), "iq" | "message" | "presence") {
            self.sm_state.record_received(1);
        }

        // Once bootstrap is complete, route to the app-level stanza dispatcher.
        if matches!(self.bootstrap, BootstrapState::Ready) {
            events.extend(self.handle_app_stanza(&element));
            return Ok(());
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
