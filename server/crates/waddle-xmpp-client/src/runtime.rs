use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};
use crate::event::{ClientEvent, ConnectionEvent, LifecycleEvent, StreamManagementEvent};
use crate::request::{ClientRequest, PendingRequest, RequestTracker, StanzaId};
use crate::state::{SessionBinding, SessionPhase, SessionSnapshot};
use crate::stream_management::SmState;
use crate::transport::{StreamClose, StreamOpen, TransportEvent, TransportMessage, TransportState};
use crate::{
    bootstrap::{
        AuthMechanism, AuthenticationRequest, BootstrapElement, RequiredStreamFeature,
        ResourceBindingRequest,
    },
    AuthenticationConfig,
};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BootstrapState {
    Idle,
    AwaitingTransport,
    AwaitingStreamOpen { authenticated: bool },
    AwaitingFeatures { authenticated: bool },
    AwaitingAuthenticationOutcome,
    AwaitingBindResult { stanza_id: StanzaId },
    Ready,
}

impl XmppRuntime {
    pub fn new(config: ClientConfig) -> ClientResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            snapshot: SessionSnapshot::new(),
            requests: RequestTracker::default(),
            bootstrap: BootstrapState::Idle,
            next_bootstrap_stanza: 0,
            sm_state: SmState::new(),
            sm_advertised: false,
        })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
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
            TransportEvent::MessageReceived(TransportMessage::Close(_))
            | TransportEvent::StateChanged(TransportState::Closed)
            | TransportEvent::Closed => {
                self.snapshot.binding = None;
                self.bootstrap = BootstrapState::Idle;
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

    fn handle_sm_element(
        &mut self,
        element: &minidom::Element,
        events: &mut Vec<ClientEvent>,
    ) -> ClientResult<()> {
        if SmState::is_request_ack(element) {
            let h = self.sm_state.inbound_count;
            let ack = SmState::build_ack(h);
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(ack),
            )));
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested,
            )));
        } else if let Some(h) = SmState::parse_ack_h(element) {
            self.sm_state.process_ack(h);
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )));
        } else if element.name() == "enabled" {
            let previd = SmState::parse_enabled(element);
            self.sm_state.previd = previd.clone();
            self.sm_state.enabled = true;
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )));
        } else if element.name() == "resumed" {
            let h = element.attr("h").and_then(|v| v.parse().ok()).unwrap_or(0);
            self.sm_state.previd = element.attr("previd").map(|s| s.to_string());
            self.sm_state.enabled = true;
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )));
        } else if element.name() == "failed" {
            self.sm_state.enabled = false;
            events.push(ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )));
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
                if !features.bind {
                    return Err(ClientError::MissingStreamFeature {
                        feature: RequiredStreamFeature::ResourceBinding,
                    });
                }

                if features.stream_management {
                    self.sm_advertised = true;
                }

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
                self.sm_state.record_sent(1);
            }
            _ => {}
        }

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
            let enable = SmState::build_enable(sm_cfg.allow_resume);
            events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(enable),
            )));
        }

        Ok(())
    }

    /// Route a post-bootstrap application stanza to a typed [`ClientEvent`].
    ///
    /// IQ result and error stanzas are returned as [`ClientEvent::IqResult`] so the
    /// driver can route them to its IQ correlation map without broadcasting them on
    /// the public event bus.  Message stanzas are dispatched to the typed protocol
    /// handlers in priority order: MAM results, PEP events, then general messaging.
    /// Unrecognised stanzas fall through to [`ClientEvent::UnhandledStanza`].
    pub fn handle_app_stanza(&mut self, element: &minidom::Element) -> Vec<ClientEvent> {
        use crate::{mam, messaging, pep};

        let type_attr = element.attr("type").unwrap_or("");

        // IQ results/errors are routed to the pending IQ correlation map.
        if element.name() == "iq" && (type_attr == "result" || type_attr == "error") {
            if let Some(id) = element.attr("id") {
                return vec![ClientEvent::IqResult {
                    id: id.to_string(),
                    element: element.clone(),
                }];
            }
        }

        // MAM result messages must be checked before generic message parsing
        // so the MAM query collector can match on ClientEvent::MamResult.
        if element.name() == "message" {
            if let Some(archived) = mam::parse_mam_result(element) {
                return vec![ClientEvent::MamResult(archived)];
            }

            if let Some(pep_item) = pep::parse(element) {
                return vec![ClientEvent::PepEvent(pep_item)];
            }
        }

        // General message + presence handling.
        if let Some(ev) = messaging::parse(element) {
            return vec![ClientEvent::Messaging(ev)];
        }

        vec![ClientEvent::UnhandledStanza(element.clone())]
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
                | (SessionPhase::Authenticating, SessionPhase::Binding)
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use jid::{BareJid, FullJid};
    use minidom::Element;
    use url::Url;

    use super::*;
    use crate::bootstrap::{NS_BIND, NS_SASL, NS_STREAMS};
    use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
    use crate::ConnectionConfig;

    fn config() -> ClientConfig {
        ClientConfig::new(
            ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
            WebSocketConfig::new(Url::parse("wss://chat.example.com/xmpp").unwrap()).unwrap(),
            OAuthBearerConfig::new(
                BareJid::from_str("alice@example.com").unwrap(),
                ClientResource::new("macbook").unwrap(),
                AccessToken::new("token"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn runtime_updates_state_when_connecting() {
        let mut runtime = XmppRuntime::new(config()).unwrap();
        let events = runtime.queue_request(ClientRequest::Connect).unwrap();
        assert_eq!(runtime.snapshot().phase, SessionPhase::Connecting);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn runtime_emits_initial_open_when_transport_opens() {
        let mut runtime = XmppRuntime::new(config()).unwrap();
        runtime.queue_request(ClientRequest::Connect).unwrap();
        let events = runtime
            .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
            .unwrap();

        assert_eq!(runtime.snapshot().phase, SessionPhase::OpeningStream);
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::StreamOpening(open))
                if open.to.as_ref() == Some(&BareJid::from_str("waddle.example").unwrap())
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Open(_)))
        )));
    }

    #[test]
    fn runtime_bootstraps_auth_bind_and_ready_state() {
        let mut runtime = XmppRuntime::new(config()).unwrap();
        runtime.queue_request(ClientRequest::Connect).unwrap();
        runtime
            .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
            .unwrap();
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
            )))
            .unwrap();

        let auth_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                pre_auth_features(),
            )))
            .unwrap();
        assert_eq!(runtime.snapshot().phase, SessionPhase::Authenticating);
        assert!(auth_events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::AuthenticationRequested(
                AuthenticationRequest::OAuthBearer(_)
            ))
        )));

        let reopen_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("success", NS_SASL).build(),
            )))
            .unwrap();
        assert!(reopen_events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::AuthenticationSucceeded)
        )));
        assert!(reopen_events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(TransportMessage::Open(_)))
        )));

        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
            )))
            .unwrap();
        let bind_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                post_auth_features(),
            )))
            .unwrap();

        let bind_id = bind_events
            .iter()
            .find_map(|event| match event {
                ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                    Some(request.stanza_id.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(runtime.snapshot().phase, SessionPhase::Binding);

        let ready_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                bind_result(&bind_id),
            )))
            .unwrap();

        assert_eq!(runtime.snapshot().phase, SessionPhase::Established);
        assert_eq!(runtime.snapshot().client_state(), crate::ClientState::Ready);
        assert_eq!(
            runtime.snapshot().binding,
            Some(SessionBinding {
                jid: FullJid::from_str("alice@example.com/macbook").unwrap(),
                stream_id: None,
                resumable: false,
            })
        );
        assert!(ready_events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::SessionReady(binding))
                if binding.jid == FullJid::from_str("alice@example.com/macbook").unwrap()
        )));
        assert!(ready_events.iter().any(|event| matches!(
            event,
            ClientEvent::Lifecycle(LifecycleEvent::SessionReady(binding))
                if binding.jid == FullJid::from_str("alice@example.com/macbook").unwrap()
        )));
    }

    #[test]
    fn runtime_requires_oauthbearer_feature() {
        let mut runtime = XmppRuntime::new(config()).unwrap();
        runtime.queue_request(ClientRequest::Connect).unwrap();
        runtime
            .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
            .unwrap();
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
            )))
            .unwrap();

        let error = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("features", NS_STREAMS).build(),
            )))
            .unwrap_err();

        assert!(matches!(
            error,
            ClientError::MissingStreamFeature {
                feature: RequiredStreamFeature::Authentication(AuthMechanism::OAuthBearer)
            }
        ));
    }

    #[test]
    fn runtime_emits_typed_auth_failure() {
        let mut runtime = XmppRuntime::new(config()).unwrap();
        runtime.queue_request(ClientRequest::Connect).unwrap();
        runtime
            .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
            .unwrap();
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                StreamOpen::from_server(BareJid::from_str("waddle.example").unwrap()),
            )))
            .unwrap();
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                pre_auth_features(),
            )))
            .unwrap();

        let events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("failure", NS_SASL)
                    .append(Element::builder("not-authorized", NS_SASL).build())
                    .build(),
            )))
            .unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::AuthenticationFailed(failure))
                if failure.condition == crate::SaslFailureCondition::NotAuthorized
        )));
        assert_eq!(runtime.snapshot().phase, SessionPhase::Disconnecting);
    }

    fn pre_auth_features() -> Element {
        Element::builder("features", NS_STREAMS)
            .append(
                Element::builder("mechanisms", NS_SASL)
                    .append(
                        Element::builder("mechanism", NS_SASL)
                            .append("OAUTHBEARER")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn post_auth_features() -> Element {
        Element::builder("features", NS_STREAMS)
            .append(Element::builder("bind", NS_BIND).build())
            .build()
    }

    fn bind_result(stanza_id: &StanzaId) -> Element {
        Element::builder("iq", crate::NS_CLIENT)
            .attr("id", stanza_id.as_str())
            .attr("type", "result")
            .append(
                Element::builder("bind", NS_BIND)
                    .append(
                        Element::builder("jid", NS_BIND)
                            .append("alice@example.com/macbook")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }
}
