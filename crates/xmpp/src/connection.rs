use std::time::Duration;

#[cfg(feature = "native")]
use std::sync::Arc;

pub use crate::transport::ConnectionConfig;
use crate::{
    carbons::{CarbonsManager, CarbonsState, is_carbons_iq_response},
    csi::{ClientState, CsiManager},
    error::ConnectionError,
    stream_management::{
        StreamManagementAction, StreamManagementState, StreamManager, decode_nonza, encode_nonza,
    },
    transport::XmppTransport,
};

#[cfg(feature = "native")]
use waddle_core::event::{Channel, Event, EventBus, EventPayload, EventSource};

#[cfg(not(any(feature = "native", feature = "web")))]
compile_error!("waddle-xmpp requires either the `native` or `web` feature.");

#[cfg(feature = "native")]
type DefaultTransport = crate::transport::NativeTcpTransport;

#[cfg(all(feature = "web", not(feature = "native")))]
type DefaultTransport = crate::transport::WebSocketTransport;

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
}

pub struct ConnectionManager<T = DefaultTransport>
where
    T: XmppTransport,
{
    state: ConnectionState,
    config: ConnectionConfig,
    transport: Option<T>,
    stream_manager: StreamManager,
    carbons_manager: CarbonsManager,
    csi_manager: CsiManager,
    #[cfg(feature = "native")]
    event_bus: Option<Arc<dyn EventBus>>,
}

impl<T> ConnectionManager<T>
where
    T: XmppTransport,
{
    const INITIAL_RECONNECT_DELAY_SECONDS: u64 = 1;
    const MAX_RECONNECT_DELAY_SECONDS: u64 = 60;

    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            config,
            transport: None,
            stream_manager: StreamManager::new(),
            carbons_manager: CarbonsManager::new(),
            csi_manager: CsiManager::new(),
            #[cfg(feature = "native")]
            event_bus: None,
        }
    }

    #[cfg(feature = "native")]
    pub fn with_event_bus(config: ConnectionConfig, event_bus: Arc<dyn EventBus>) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            config,
            transport: None,
            stream_manager: StreamManager::new(),
            carbons_manager: CarbonsManager::new(),
            csi_manager: CsiManager::new(),
            event_bus: Some(event_bus),
        }
    }

    pub async fn connect(&mut self) -> Result<(), ConnectionError> {
        if matches!(self.state, ConnectionState::Connected) && self.transport.is_some() {
            return Ok(());
        }

        self.state = ConnectionState::Connecting;
        let mut reconnect_attempt = 0_u32;

        loop {
            match T::connect(&self.config).await {
                Ok(mut transport) => {
                    if transport.supports_stream_management() {
                        if let Err(error) = self.bootstrap_stream_management(&mut transport).await {
                            self.stream_manager.on_connect_attempt_failed();
                            reconnect_attempt = self
                                .handle_connect_failure(error, reconnect_attempt)
                                .await?;
                            continue;
                        }
                    } else {
                        self.stream_manager.reset();
                    }

                    self.transport = Some(transport);
                    self.state = ConnectionState::Connected;
                    self.bootstrap_csi().await;
                    #[cfg(feature = "native")]
                    self.emit_connection_established();
                    return Ok(());
                }
                Err(error) => {
                    self.stream_manager.on_connect_attempt_failed();
                    reconnect_attempt = self
                        .handle_connect_failure(error, reconnect_attempt)
                        .await?;
                }
            }
        }
    }

    pub async fn send_stanza(&mut self, stanza: &[u8]) -> Result<(), ConnectionError> {
        self.send_raw(stanza, true).await
    }

    pub async fn recv_frame_with_timeout(
        &mut self,
        timeout_duration: Duration,
    ) -> Result<Option<Vec<u8>>, ConnectionError> {
        let Some(transport) = self.transport.as_mut() else {
            return Ok(None);
        };

        match tokio::time::timeout(timeout_duration, transport.recv()).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None),
        }
    }

    pub fn mark_inbound_stanza_handled(&mut self) {
        self.stream_manager.mark_inbound_handled();
    }

    pub fn stream_management_state(&self) -> StreamManagementState {
        self.stream_manager.state()
    }

    pub fn carbons_state(&self) -> CarbonsState {
        self.carbons_manager.state()
    }

    pub fn csi_state(&self) -> ClientState {
        self.csi_manager.state()
    }

    pub fn set_csi_server_support(&mut self, supported: bool) {
        self.csi_manager.set_server_support(supported);
    }

    pub async fn enable_carbons(&mut self) -> Result<(), ConnectionError> {
        if let Some(iq) = self.carbons_manager.enable() {
            if let Err(error) = self.send_raw(&iq, false).await {
                self.carbons_manager.on_enable_result(false);
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn disable_carbons(&mut self) -> Result<(), ConnectionError> {
        if let Some(iq) = self.carbons_manager.disable() {
            if let Err(error) = self.send_raw(&iq, false).await {
                self.carbons_manager.on_disable_result(false);
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn handle_carbons_iq_response(&mut self, stanza: &[u8]) -> bool {
        let Some((is_enable, success)) = is_carbons_iq_response(stanza) else {
            return false;
        };

        if is_enable {
            self.carbons_manager.on_enable_result(success);
        } else {
            self.carbons_manager.on_disable_result(success);
        }
        true
    }

    pub async fn set_csi_inactive(&mut self) -> Result<(), ConnectionError> {
        if let Some(stanza) = self.csi_manager.set_inactive() {
            if let Err(error) = self.send_raw(&stanza, false).await {
                let _ = self.csi_manager.set_active();
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn set_csi_active(&mut self) -> Result<(), ConnectionError> {
        if let Some(stanza) = self.csi_manager.set_active() {
            if let Err(error) = self.send_raw(&stanza, false).await {
                let _ = self.csi_manager.set_inactive();
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn handle_stream_management_frame(
        &mut self,
        frame: &[u8],
    ) -> Result<bool, ConnectionError> {
        let Some(nonza) = decode_nonza(frame) else {
            return Ok(false);
        };

        let actions = self.stream_manager.process_nonza(nonza)?;
        self.apply_stream_management_actions(actions).await?;
        Ok(true)
    }

    pub async fn recover_after_network_interruption(
        &mut self,
        reason: String,
    ) -> Result<(), ConnectionError> {
        let will_retry = self.should_retry(1);

        if let Some(mut transport) = self.transport.take() {
            let _ = transport.close().await;
        }

        self.state = ConnectionState::Disconnected;
        self.stream_manager.prepare_for_reconnect();
        self.carbons_manager.reset();

        #[cfg(feature = "native")]
        self.emit_connection_lost(reason, will_retry);

        self.connect().await
    }

    pub async fn disconnect(&mut self) -> Result<(), ConnectionError> {
        if let Some(mut transport) = self.transport.take() {
            if let Err(error) = transport.close().await {
                self.state = ConnectionState::Disconnected;
                self.stream_manager.reset();
                self.carbons_manager.reset();
                self.csi_manager.reset();
                #[cfg(feature = "native")]
                {
                    self.emit_connection_lost(error.to_string(), false);
                    self.emit_connection_error(&error);
                }
                return Err(error);
            }
        }

        if !matches!(self.state, ConnectionState::Disconnected) {
            #[cfg(feature = "native")]
            self.emit_connection_lost("user requested disconnect".to_string(), false);
        }

        self.state = ConnectionState::Disconnected;
        self.stream_manager.reset();
        self.carbons_manager.reset();
        self.csi_manager.reset();
        Ok(())
    }

    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    async fn bootstrap_stream_management(
        &mut self,
        transport: &mut T,
    ) -> Result<(), ConnectionError> {
        if let Some(nonza) = self.stream_manager.on_stream_started() {
            let request = encode_nonza(nonza)?;
            transport.send(&request).await?;
        }
        Ok(())
    }

    async fn bootstrap_csi(&mut self) {
        if let Some(stanza) = self.csi_manager.on_stream_started() {
            let _ = self.send_raw(&stanza, false).await;
        }
    }

    async fn handle_connect_failure(
        &mut self,
        error: ConnectionError,
        reconnect_attempt: u32,
    ) -> Result<u32, ConnectionError> {
        self.transport = None;
        let next_attempt = reconnect_attempt.saturating_add(1);
        let will_retry = error.is_retryable() && self.should_retry(next_attempt);

        #[cfg(feature = "native")]
        {
            self.emit_connection_lost(error.to_string(), will_retry);
            self.emit_connection_error(&error);
        }

        if !will_retry {
            self.state = ConnectionState::Disconnected;
            return Err(error);
        }

        self.state = ConnectionState::Reconnecting {
            attempt: next_attempt,
        };
        #[cfg(feature = "native")]
        self.emit_connection_reconnecting(next_attempt);

        tokio::time::sleep(Self::reconnect_delay(next_attempt)).await;
        self.state = ConnectionState::Connecting;
        Ok(next_attempt)
    }

    async fn apply_stream_management_actions(
        &mut self,
        actions: Vec<StreamManagementAction>,
    ) -> Result<(), ConnectionError> {
        for action in actions {
            match action {
                StreamManagementAction::SendNonza(nonza) => {
                    let payload = encode_nonza(nonza)?;
                    self.send_raw(&payload, false).await?;
                }
                StreamManagementAction::ReplayStanzas(stanzas) => {
                    for stanza in stanzas {
                        self.send_raw(&stanza, false).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn send_raw(
        &mut self,
        data: &[u8],
        track_for_resumption: bool,
    ) -> Result<(), ConnectionError> {
        let transport = self.transport.as_mut().ok_or_else(|| {
            ConnectionError::TransportError("cannot send data while disconnected".to_string())
        })?;
        transport.send(data).await?;

        if track_for_resumption {
            self.stream_manager.track_outbound_stanza(data);
        }

        Ok(())
    }

    fn should_retry(&self, attempt: u32) -> bool {
        self.config.max_reconnect_attempts == 0 || attempt <= self.config.max_reconnect_attempts
    }

    fn reconnect_delay(attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1);
        let seconds = 1_u64.checked_shl(shift).unwrap_or(u64::MAX).clamp(
            Self::INITIAL_RECONNECT_DELAY_SECONDS,
            Self::MAX_RECONNECT_DELAY_SECONDS,
        );
        Duration::from_secs(seconds)
    }

    #[cfg(feature = "native")]
    fn emit_connection_established(&self) {
        self.emit_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: self.config.jid.clone(),
            },
        );
    }

    #[cfg(feature = "native")]
    fn emit_connection_lost(&self, reason: String, will_retry: bool) {
        self.emit_event(
            "system.connection.lost",
            EventPayload::ConnectionLost { reason, will_retry },
        );
    }

    #[cfg(feature = "native")]
    fn emit_connection_reconnecting(&self, attempt: u32) {
        self.emit_event(
            "system.connection.reconnecting",
            EventPayload::ConnectionReconnecting { attempt },
        );
    }

    #[cfg(feature = "native")]
    fn emit_connection_error(&self, error: &ConnectionError) {
        self.emit_event(
            "system.error.occurred",
            EventPayload::ErrorOccurred {
                component: "connection".to_string(),
                message: error.to_string(),
                recoverable: error.is_retryable(),
            },
        );
    }

    #[cfg(feature = "native")]
    fn emit_event(&self, channel_name: &str, payload: EventPayload) {
        let Some(event_bus) = &self.event_bus else {
            return;
        };

        let Ok(channel) = Channel::new(channel_name) else {
            return;
        };

        let event = Event::new(channel, EventSource::Xmpp, payload);
        let _ = event_bus.publish(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTransport;

    impl XmppTransport for DummyTransport {
        async fn connect(_config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            Ok(Self)
        }

        async fn send(&mut self, _data: &[u8]) -> Result<(), ConnectionError> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            Ok(Vec::new())
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            Ok(())
        }

        fn supports_stream_management(&self) -> bool {
            true
        }
    }

    #[test]
    fn reconnect_delay_is_exponential_and_capped_at_sixty_seconds() {
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(1),
            Duration::from_secs(1)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(2),
            Duration::from_secs(2)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(3),
            Duration::from_secs(4)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(4),
            Duration::from_secs(8)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(6),
            Duration::from_secs(32)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(7),
            Duration::from_secs(60)
        );
        assert_eq!(
            ConnectionManager::<DummyTransport>::reconnect_delay(99),
            Duration::from_secs(60)
        );
    }

    fn config(max_reconnect_attempts: u32) -> ConnectionConfig {
        ConnectionConfig {
            jid: "alice@example.com".to_string(),
            password: "password".to_string(),
            server: Some("xmpp.example.com".to_string()),
            port: Some(5222),
            timeout_seconds: 30,
            max_reconnect_attempts,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn enable_carbons_while_disconnected_rolls_back_state() {
        let mut manager = ConnectionManager::<DummyTransport>::new(config(0));

        let result = manager.enable_carbons().await;
        assert!(matches!(result, Err(ConnectionError::TransportError(_))));
        assert_eq!(manager.carbons_state(), CarbonsState::Disabled);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_csi_inactive_while_disconnected_rolls_back_state() {
        let mut manager = ConnectionManager::<DummyTransport>::new(config(0));
        manager.set_csi_server_support(true);

        let result = manager.set_csi_inactive().await;
        assert!(matches!(result, Err(ConnectionError::TransportError(_))));
        assert_eq!(manager.csi_state(), ClientState::Active);
    }
}

#[cfg(all(test, feature = "native"))]
mod native_tests {
    use std::{
        collections::VecDeque,
        sync::{Mutex, OnceLock},
    };

    use tokio::{sync::Mutex as AsyncMutex, time};
    use waddle_core::event::{BroadcastEventBus, EventPayload};
    use xmpp_parsers::sm::Nonza;

    use super::*;

    #[derive(Default)]
    struct TestTransportState {
        connect_outcomes: VecDeque<Result<(), ConnectionError>>,
        connect_calls: u32,
        close_calls: u32,
        sent_payloads: Vec<String>,
    }

    fn transport_state() -> &'static Mutex<TestTransportState> {
        static STATE: OnceLock<Mutex<TestTransportState>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(TestTransportState::default()))
    }

    fn test_lock() -> &'static AsyncMutex<()> {
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(()))
    }

    fn configure_transport(outcomes: Vec<Result<(), ConnectionError>>) {
        let mut state = transport_state()
            .lock()
            .expect("failed to lock transport state");
        state.connect_outcomes = outcomes.into_iter().collect();
        state.connect_calls = 0;
        state.close_calls = 0;
        state.sent_payloads.clear();
    }

    fn connect_calls() -> u32 {
        transport_state()
            .lock()
            .expect("failed to lock transport state")
            .connect_calls
    }

    fn close_calls() -> u32 {
        transport_state()
            .lock()
            .expect("failed to lock transport state")
            .close_calls
    }

    fn sent_payloads() -> Vec<String> {
        transport_state()
            .lock()
            .expect("failed to lock transport state")
            .sent_payloads
            .clone()
    }

    fn nonzas_sent() -> Vec<Nonza> {
        sent_payloads()
            .into_iter()
            .filter_map(|payload| decode_nonza(payload.as_bytes()))
            .collect()
    }

    fn config(max_reconnect_attempts: u32) -> ConnectionConfig {
        ConnectionConfig {
            jid: "alice@example.com".to_string(),
            password: "password".to_string(),
            server: Some("xmpp.example.com".to_string()),
            port: Some(5222),
            timeout_seconds: 30,
            max_reconnect_attempts,
        }
    }

    struct TestTransport;

    impl XmppTransport for TestTransport {
        async fn connect(_config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            let mut state = transport_state()
                .lock()
                .expect("failed to lock transport state");
            state.connect_calls += 1;
            match state.connect_outcomes.pop_front().unwrap_or(Ok(())) {
                Ok(()) => Ok(Self),
                Err(error) => Err(error),
            }
        }

        async fn send(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
            let mut state = transport_state()
                .lock()
                .expect("failed to lock transport state");
            state
                .sent_payloads
                .push(String::from_utf8_lossy(data).into_owned());
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            Ok(Vec::new())
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            let mut state = transport_state()
                .lock()
                .expect("failed to lock transport state");
            state.close_calls += 1;
            Ok(())
        }

        fn supports_stream_management(&self) -> bool {
            true
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connect_emits_established_and_transitions_to_connected() {
        let _guard = test_lock().lock().await;
        configure_transport(vec![Ok(())]);

        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(16));
        let mut established = event_bus
            .subscribe("system.connection.established")
            .expect("failed to subscribe established events");

        let mut manager =
            ConnectionManager::<TestTransport>::with_event_bus(config(0), event_bus.clone());
        manager.connect().await.expect("connect should succeed");

        assert_eq!(manager.state(), ConnectionState::Connected);
        assert_eq!(connect_calls(), 1);
        assert_eq!(
            manager.stream_management_state(),
            StreamManagementState::Enabling
        );
        assert!(
            nonzas_sent()
                .iter()
                .any(|nonza| matches!(nonza, Nonza::Enable(enable) if enable.resume))
        );

        let event = time::timeout(Duration::from_millis(100), established.recv())
            .await
            .expect("timed out waiting for established event")
            .expect("failed to receive established event");
        assert_eq!(event.channel.as_str(), "system.connection.established");
        assert!(matches!(
            event.payload,
            EventPayload::ConnectionEstablished { jid } if jid == "alice@example.com"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authentication_failure_is_non_retryable() {
        let _guard = test_lock().lock().await;
        configure_transport(vec![Err(ConnectionError::AuthenticationFailed(
            "invalid credentials".to_string(),
        ))]);

        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(16));
        let mut lost = event_bus
            .subscribe("system.connection.lost")
            .expect("failed to subscribe lost events");
        let mut errors = event_bus
            .subscribe("system.error.occurred")
            .expect("failed to subscribe error events");

        let mut manager =
            ConnectionManager::<TestTransport>::with_event_bus(config(10), event_bus.clone());
        let result = manager.connect().await;

        assert!(matches!(
            result,
            Err(ConnectionError::AuthenticationFailed(_))
        ));
        assert_eq!(manager.state(), ConnectionState::Disconnected);
        assert_eq!(connect_calls(), 1);

        let lost_event = time::timeout(Duration::from_millis(100), lost.recv())
            .await
            .expect("timed out waiting for lost event")
            .expect("failed to receive lost event");
        assert!(matches!(
            lost_event.payload,
            EventPayload::ConnectionLost {
                will_retry: false,
                ..
            }
        ));

        let error_event = time::timeout(Duration::from_millis(100), errors.recv())
            .await
            .expect("timed out waiting for error event")
            .expect("failed to receive error event");
        assert!(matches!(
            error_event.payload,
            EventPayload::ErrorOccurred {
                recoverable: false,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn retryable_errors_emit_reconnecting_and_retry() {
        let _guard = test_lock().lock().await;
        configure_transport(vec![Err(ConnectionError::Timeout), Ok(())]);

        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(16));
        let mut reconnecting = event_bus
            .subscribe("system.connection.reconnecting")
            .expect("failed to subscribe reconnecting events");
        let mut lost = event_bus
            .subscribe("system.connection.lost")
            .expect("failed to subscribe lost events");
        let mut established = event_bus
            .subscribe("system.connection.established")
            .expect("failed to subscribe established events");

        let manager =
            ConnectionManager::<TestTransport>::with_event_bus(config(3), event_bus.clone());
        let connect_task = tokio::spawn(async move {
            let mut manager = manager;
            let result = manager.connect().await;
            (manager, result)
        });

        let reconnecting_event = reconnecting
            .recv()
            .await
            .expect("failed to receive reconnecting event");
        assert!(matches!(
            reconnecting_event.payload,
            EventPayload::ConnectionReconnecting { attempt: 1 }
        ));

        let lost_event = lost.recv().await.expect("failed to receive lost event");
        assert!(matches!(
            lost_event.payload,
            EventPayload::ConnectionLost {
                will_retry: true,
                ..
            }
        ));

        time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;

        let (manager, result) = connect_task.await.expect("connect task failed");
        result.expect("connect should succeed after retry");
        assert_eq!(manager.state(), ConnectionState::Connected);
        assert_eq!(connect_calls(), 2);

        let established_event = established
            .recv()
            .await
            .expect("failed to receive established event");
        assert!(matches!(
            established_event.payload,
            EventPayload::ConnectionEstablished { jid } if jid == "alice@example.com"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn network_interruption_uses_stream_resumption_and_replays_unacked() {
        let _guard = test_lock().lock().await;
        configure_transport(vec![Ok(()), Ok(())]);

        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(16));
        let mut manager =
            ConnectionManager::<TestTransport>::with_event_bus(config(0), event_bus.clone());
        manager.connect().await.expect("connect should succeed");
        manager
            .handle_stream_management_frame(
                br#"<enabled xmlns='urn:xmpp:sm:3' id='stream-1' resume='true'/>"#,
            )
            .await
            .expect("failed to process stream-management enabled response");

        manager
            .send_stanza(b"<message id='one'/>")
            .await
            .expect("first stanza should be sent");
        manager
            .send_stanza(b"<message id='two'/>")
            .await
            .expect("second stanza should be sent");
        manager.mark_inbound_stanza_handled();

        manager
            .recover_after_network_interruption("network lost".to_string())
            .await
            .expect("recovery should reconnect");

        assert_eq!(connect_calls(), 2);
        let resume = nonzas_sent()
            .into_iter()
            .find_map(|nonza| match nonza {
                Nonza::Resume(resume) => Some(resume),
                _ => None,
            })
            .expect("expected a stream resumption request");
        assert_eq!(resume.h, 1);
        assert_eq!(resume.previd.0, "stream-1");

        manager
            .handle_stream_management_frame(
                br#"<resumed xmlns='urn:xmpp:sm:3' h='1' previd='stream-1'/>"#,
            )
            .await
            .expect("failed to process stream resumption");
        assert_eq!(
            manager.stream_management_state(),
            StreamManagementState::Enabled
        );

        let sent = sent_payloads();
        let one_count = sent
            .iter()
            .filter(|payload| payload.as_str() == "<message id='one'/>")
            .count();
        let two_count = sent
            .iter()
            .filter(|payload| payload.as_str() == "<message id='two'/>")
            .count();
        assert_eq!(one_count, 1);
        assert_eq!(two_count, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_closes_transport_and_emits_lost_without_retry() {
        let _guard = test_lock().lock().await;
        configure_transport(vec![Ok(())]);

        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(16));
        let mut lost = event_bus
            .subscribe("system.connection.lost")
            .expect("failed to subscribe lost events");

        let mut manager =
            ConnectionManager::<TestTransport>::with_event_bus(config(0), event_bus.clone());
        manager.connect().await.expect("connect should succeed");
        manager
            .disconnect()
            .await
            .expect("disconnect should succeed");

        assert_eq!(manager.state(), ConnectionState::Disconnected);
        assert_eq!(close_calls(), 1);

        let first_lost_event = time::timeout(Duration::from_millis(100), lost.recv())
            .await
            .expect("timed out waiting for lost event")
            .expect("failed to receive lost event");
        assert!(matches!(
            first_lost_event.payload,
            EventPayload::ConnectionLost {
                will_retry: false,
                ..
            }
        ));
    }
}
