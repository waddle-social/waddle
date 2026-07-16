use super::*;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::{pending, Either};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

impl WasmDriverWire for WasmWebSocket {
    fn events(&mut self) -> &mut mpsc::Receiver<WasmTransportEvent> {
        &mut self.rx
    }

    fn send_frame(&mut self, frame: &str) -> DriverResult<()> {
        self.send(frame).map_err(|_| ClientError::TransportClosed)
    }

    fn close_websocket(&mut self) -> DriverResult<()> {
        WasmWebSocket::close(self).map_err(|_| ClientError::TransportClosed)
    }
}

#[derive(Default)]
struct WindowDriverTimerBackend;

impl DriverTimerBackend for WindowDriverTimerBackend {
    fn wait(&self, delay_ms: Option<u64>) -> futures::future::LocalBoxFuture<'static, ()> {
        let Some(delay_ms) = delay_ms else {
            return Box::pin(std::future::pending());
        };
        match WindowTimeout::new(delay_ms) {
            Some(timeout) => Box::pin(timeout),
            None => Box::pin(std::future::pending()),
        }
    }
}

struct WindowTimeout {
    window: web_sys::Window,
    timeout_id: i32,
    receiver: oneshot::Receiver<()>,
    _callback: Closure<dyn FnMut()>,
}

impl WindowTimeout {
    fn new(delay_ms: u64) -> Option<Self> {
        let window = web_sys::window()?;
        let (sender, receiver) = oneshot::channel();
        let mut sender = Some(sender);
        let callback = Closure::wrap(Box::new(move || {
            if let Some(sender) = sender.take() {
                let _ = sender.send(());
            }
        }) as Box<dyn FnMut()>);
        let timeout_ms = i32::try_from(delay_ms).unwrap_or(i32::MAX);
        let timeout_id = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                timeout_ms,
            )
            .ok()?;
        Some(Self {
            window,
            timeout_id,
            receiver,
            _callback: callback,
        })
    }
}

impl Future for WindowTimeout {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(context).map(|_| ())
    }
}

impl Drop for WindowTimeout {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.timeout_id);
    }
}

enum DriverInput {
    Wire(Option<WasmTransportEvent>),
    Command(Option<WasmCommand>),
    DriverTimer,
}

/// Private attribution for one ordered browser wire-pump failure.
///
/// `send_frame` is synchronous: a thrown browser WebSocket send means that
/// exact frame was not accepted. A later generated control-frame failure must
/// therefore terminate the stream without reclassifying an already-confirmed
/// initiating stanza as retryable.
#[derive(Debug)]
struct WasmPumpFailure {
    failed_message: TransportMessage,
    initiating_message_confirmed: bool,
    source: ClientError,
}

impl WasmPumpFailure {
    fn initiating_message_confirmed(&self) -> bool {
        self.initiating_message_confirmed
    }

    fn attributed_message(&self) -> &TransportMessage {
        &self.failed_message
    }

    fn source(&self) -> &ClientError {
        &self.source
    }

    fn into_source(self) -> ClientError {
        self.source
    }
}

async fn select_driver_input<WireFuture, CommandFuture, TimerFuture>(
    wire_future: WireFuture,
    command_future: CommandFuture,
    timer_future: TimerFuture,
) -> DriverInput
where
    WireFuture: Future<Output = Option<WasmTransportEvent>>,
    CommandFuture: Future<Output = Option<WasmCommand>>,
    TimerFuture: Future<Output = ()>,
{
    let wire_future = wire_future.fuse();
    let command_future = command_future.fuse();
    let timer_future = timer_future.fuse();
    pin_mut!(wire_future, command_future, timer_future);

    select! {
        event = wire_future => DriverInput::Wire(event),
        command = command_future => DriverInput::Command(command),
        _ = timer_future => DriverInput::DriverTimer,
    }
}

pub(crate) async fn driver_loop(
    config: ClientConfig,
    ws: WasmWebSocket,
    cmd_rx: mpsc::Receiver<WasmCommand>,
    event_tx: mpsc::Sender<DriverEvent>,
    inner: Rc<RefCell<WaddleClientInner>>,
) {
    let mut task = match WasmDriverTask::new(config, ws, cmd_rx, event_tx.clone(), inner) {
        Ok(task) => task,
        Err(err) => {
            let mut event_tx = event_tx;
            let _ = event_tx.send(DriverEvent::Error(err.to_string())).await;
            let _ = event_tx.send(DriverEvent::Disconnected).await;
            return;
        }
    };
    task.run().await;
}

impl WasmDriverTask {
    fn new(
        config: ClientConfig,
        ws: WasmWebSocket,
        cmd_rx: mpsc::Receiver<WasmCommand>,
        event_tx: mpsc::Sender<DriverEvent>,
        inner: Rc<RefCell<WaddleClientInner>>,
    ) -> DriverResult<Self> {
        Self::new_with_dependencies(
            config,
            Box::new(ws),
            Rc::new(WindowDriverTimerBackend),
            cmd_rx,
            event_tx,
            inner,
        )
    }

    fn new_with_dependencies(
        config: ClientConfig,
        ws: Box<dyn WasmDriverWire>,
        timer: Rc<dyn DriverTimerBackend>,
        cmd_rx: mpsc::Receiver<WasmCommand>,
        event_tx: mpsc::Sender<DriverEvent>,
        inner: Rc<RefCell<WaddleClientInner>>,
    ) -> DriverResult<Self> {
        Ok(Self {
            runtime: XmppRuntime::new(config)?,
            ws,
            timer,
            cmd_rx,
            event_tx,
            inner,
            pending_iqs: HashMap::new(),
            pending_mam_queries: HashMap::new(),
            pending_inbox_queries: HashMap::new(),
            deferred_commands: VecDeque::new(),
            explicit_disconnect: false,
            websocket_close_started: false,
            commands_closed: false,
        })
    }

    async fn run(&mut self) {
        match self.runtime.queue_request(ClientRequest::Connect) {
            Ok(events) => {
                self.publish_resume_state_snapshot();
                for event in events {
                    if !self.handle_client_event(event).await {
                        self.finish().await;
                        return;
                    }
                }
            }
            Err(err) => {
                self.emit_error(err.to_string()).await;
                self.finish().await;
                return;
            }
        }

        loop {
            let now_ms = waddle_xmpp_client::runtime::monotonic_now_ms();
            let sm_wakeup_ms = self.runtime.next_stream_management_wakeup_in_ms(now_ms);
            let close_wakeup_ms = self.runtime.next_stream_close_wakeup_in_ms(now_ms);
            let driver_wakeup_ms = minimum_wakeup(sm_wakeup_ms, close_wakeup_ms);
            let close_deadline_scheduled =
                close_wakeup_ms.is_some_and(|close| sm_wakeup_ms.is_none_or(|sm| close <= sm));
            let command_future = if self.commands_closed {
                Either::Left(pending())
            } else {
                Either::Right(self.cmd_rx.next())
            };
            let input = select_driver_input(
                self.ws.events().next(),
                command_future,
                self.timer.wait(driver_wakeup_ms),
            )
            .await;

            let keep_running = match input {
                DriverInput::Wire(event) => self.handle_wasm_transport_event(event).await,
                DriverInput::Command(command) => self.handle_command(command).await,
                DriverInput::DriverTimer => {
                    self.handle_driver_timer(close_deadline_scheduled).await
                }
            };

            if !keep_running {
                break;
            }
        }

        self.finish().await;
    }

    async fn handle_driver_timer(&mut self, close_deadline_scheduled: bool) -> bool {
        if close_deadline_scheduled {
            self.terminate_uncleanly().await;
            return false;
        }
        self.handle_stream_management_timer_at(waddle_xmpp_client::runtime::monotonic_now_ms())
            .await
    }

    async fn handle_stream_management_timer_at(&mut self, now_ms: u64) -> bool {
        let events = self.runtime.poll_stream_management_at(now_ms);
        let progress_stalled = events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::AckProgressStalled { .. }
                ))
            )
        });

        for event in events {
            if !self.handle_client_event(event).await {
                return false;
            }
        }

        if progress_stalled {
            self.terminate_uncleanly().await;
            return false;
        }
        true
    }

    async fn handle_wasm_transport_event(&mut self, event: Option<WasmTransportEvent>) -> bool {
        match event {
            Some(WasmTransportEvent::Open) => {
                self.apply_transport_event(TransportEvent::StateChanged(TransportState::Connecting))
                    .await
                    && self
                        .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
                        .await
            }
            Some(WasmTransportEvent::Message(text)) => {
                match waddle_xmpp_client::decode_message(&text) {
                    Ok(message) => {
                        self.apply_transport_event(TransportEvent::MessageReceived(message))
                            .await
                    }
                    Err(err) => {
                        self.emit_error(err.to_string()).await;
                        let _ = self
                            .apply_transport_event(TransportEvent::StateChanged(
                                TransportState::Failed,
                            ))
                            .await;
                        let _ = self.apply_transport_event(TransportEvent::Closed).await;
                        false
                    }
                }
            }
            Some(WasmTransportEvent::Close { .. }) | None => {
                let _ = self
                    .apply_transport_event(TransportEvent::StateChanged(TransportState::Closed))
                    .await;
                let _ = self.apply_transport_event(TransportEvent::Closed).await;
                false
            }
            Some(WasmTransportEvent::Error) => {
                self.emit_error("websocket transport error".to_string())
                    .await;
                let _ = self
                    .apply_transport_event(TransportEvent::StateChanged(TransportState::Failed))
                    .await;
                let _ = self.apply_transport_event(TransportEvent::Closed).await;
                false
            }
        }
    }

    async fn handle_command(&mut self, cmd: Option<WasmCommand>) -> bool {
        match cmd {
            Some(WasmCommand::SendStanza { stanza, responder }) => {
                if !self.runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::Stanza { stanza, responder });
                    return true;
                }

                self.send_stanza_command(stanza, responder).await
            }
            Some(WasmCommand::SendIq { stanza, responder }) => {
                if !self.runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::Iq { stanza, responder });
                    return true;
                }

                self.send_iq_command(stanza, responder).await
            }
            Some(WasmCommand::SendMamQuery {
                stanza,
                query_id,
                responder,
            }) => {
                if !self.runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::MamQuery {
                            stanza,
                            query_id,
                            responder,
                        });
                    return true;
                }

                self.send_mam_query_command(stanza, query_id, responder)
                    .await
            }
            Some(WasmCommand::SendInboxQuery {
                stanza,
                query_id,
                responder,
            }) => {
                if !self.runtime.can_send_app_stanza() || !self.pending_inbox_queries.is_empty() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::InboxQuery {
                            stanza,
                            query_id,
                            responder,
                        });
                    return true;
                }

                self.send_inbox_query_command(stanza, query_id, responder)
                    .await
            }
            Some(WasmCommand::CancelIq { id, responder }) => {
                self.cancel_iq_command(&id);
                let _ = responder.send(Ok(()));
                true
            }
            Some(WasmCommand::RequestStreamManagementAck { responder }) => {
                let events = self.runtime.request_stream_management_ack_at(
                    waddle_xmpp_client::runtime::monotonic_now_ms(),
                );
                let mut result = Ok(());
                for event in events {
                    if !self.handle_client_event(event).await {
                        result = Err(ClientError::TransportClosed);
                        break;
                    }
                }
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
            Some(WasmCommand::Disconnect { responder }) => {
                let result = match self.runtime.request_stream_close() {
                    Ok(events) => {
                        let mut result = Ok(());
                        for event in events {
                            if !self.handle_client_event(event).await {
                                result = Err(ClientError::TransportClosed);
                                break;
                            }
                        }
                        result
                    }
                    Err(error) => Err(error),
                };
                if result.is_err() {
                    self.terminate_uncleanly().await;
                }
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
            None => {
                self.commands_closed = true;
                if self.runtime.stream_close_sent_confirmed() {
                    true
                } else {
                    self.terminate_uncleanly().await;
                    false
                }
            }
        }
    }

    fn cancel_iq_command(&mut self, id: &str) {
        cancel_raw_iq_state(&mut self.pending_iqs, &mut self.deferred_commands, id);
    }

    async fn send_stanza_command(
        &mut self,
        stanza: Element,
        responder: oneshot::Sender<DriverResult<()>>,
    ) -> bool {
        let initiating_message = TransportMessage::Element(stanza.clone());
        match self
            .send_transport_message(initiating_message.clone())
            .await
        {
            Ok(()) => {
                let _ = responder.send(Ok(()));
                true
            }
            Err(failure) => {
                self.emit_error(failure.source().to_string()).await;
                if failure.initiating_message_confirmed() {
                    // The application stanza is already owned by XEP-0198.
                    // Resolving success prevents the persisted browser queue
                    // from rolling it back and sending it again after refresh.
                    let _ = responder.send(Ok(()));
                } else {
                    if failure.attributed_message() == &initiating_message {
                        if let Some(stanza_id) = message_delivery_stanza_id(&stanza) {
                            self.emit_message_delivery_failed(stanza_id).await;
                        }
                    }
                    let _ = responder.send(Err(failure.into_source()));
                }
                self.terminate_uncleanly().await;
                false
            }
        }
    }

    async fn send_iq_command(
        &mut self,
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
    ) -> bool {
        let id = stanza.attr("id").map(|value| value.to_string());
        match self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
        {
            Ok(()) => match id {
                Some(id) => {
                    self.pending_iqs.insert(id, responder);
                    true
                }
                None => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                    false
                }
            },
            Err(failure) => {
                self.emit_error(failure.source().to_string()).await;
                if failure.initiating_message_confirmed() {
                    if let Some(id) = id {
                        self.pending_iqs.insert(id, responder);
                    } else {
                        let _ = responder.send(Err(ClientError::Disconnected));
                    }
                } else {
                    let _ = responder.send(Err(failure.into_source()));
                }
                self.terminate_uncleanly().await;
                false
            }
        }
    }

    async fn send_mam_query_command(
        &mut self,
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
    ) -> bool {
        let id = stanza.attr("id").map(|value| value.to_string());
        match self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
        {
            Ok(()) => match id {
                Some(id) => {
                    self.pending_mam_queries
                        .insert(id, PendingMamQuery::new(&query_id, responder));
                    true
                }
                None => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                    false
                }
            },
            Err(failure) => {
                self.emit_error(failure.source().to_string()).await;
                if failure.initiating_message_confirmed() {
                    if let Some(id) = id {
                        self.pending_mam_queries
                            .insert(id, PendingMamQuery::new(&query_id, responder));
                    } else {
                        let _ = responder.send(Err(ClientError::Disconnected));
                    }
                } else {
                    let _ = responder.send(Err(failure.into_source()));
                }
                self.terminate_uncleanly().await;
                false
            }
        }
    }

    async fn send_inbox_query_command(
        &mut self,
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<InboxPage>>,
    ) -> bool {
        let id = stanza.attr("id").map(|value| value.to_string());
        match self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
        {
            Ok(()) => match id {
                Some(id) => {
                    self.pending_inbox_queries.insert(
                        id,
                        PendingInboxQuery {
                            query_id,
                            entries: Vec::new(),
                            responder,
                        },
                    );
                    true
                }
                None => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                    false
                }
            },
            Err(failure) => {
                self.emit_error(failure.source().to_string()).await;
                if failure.initiating_message_confirmed() {
                    if let Some(id) = id {
                        self.pending_inbox_queries.insert(
                            id,
                            PendingInboxQuery {
                                query_id,
                                entries: Vec::new(),
                                responder,
                            },
                        );
                    } else {
                        let _ = responder.send(Err(ClientError::Disconnected));
                    }
                } else {
                    let _ = responder.send(Err(failure.into_source()));
                }
                self.terminate_uncleanly().await;
                false
            }
        }
    }

    async fn flush_deferred_commands(&mut self) -> bool {
        if !self.runtime.can_send_app_stanza() {
            return true;
        }

        let mut blocked_inbox_queries = VecDeque::new();
        while let Some(command) = self.deferred_commands.pop_front() {
            let keep_running = match command {
                DeferredWasmCommand::Stanza { stanza, responder } => {
                    self.send_stanza_command(stanza, responder).await
                }
                DeferredWasmCommand::Iq { stanza, responder } => {
                    self.send_iq_command(stanza, responder).await
                }
                DeferredWasmCommand::MamQuery {
                    stanza,
                    query_id,
                    responder,
                } => {
                    self.send_mam_query_command(stanza, query_id, responder)
                        .await
                }
                DeferredWasmCommand::InboxQuery {
                    stanza,
                    query_id,
                    responder,
                } => {
                    if !self.pending_inbox_queries.is_empty() {
                        blocked_inbox_queries.push_back(DeferredWasmCommand::InboxQuery {
                            stanza,
                            query_id,
                            responder,
                        });
                        true
                    } else {
                        self.send_inbox_query_command(stanza, query_id, responder)
                            .await
                    }
                }
            };

            if !keep_running {
                return false;
            }
        }

        while let Some(command) = blocked_inbox_queries.pop_back() {
            self.deferred_commands.push_front(command);
        }

        true
    }

    async fn apply_transport_event(&mut self, event: TransportEvent) -> bool {
        let events = match self.runtime.apply_transport_event(event) {
            Ok(events) => events,
            Err(err) => {
                self.emit_error(err.to_string()).await;
                return false;
            }
        };
        self.publish_resume_state_snapshot();

        for event in events {
            if !self.handle_client_event(event).await {
                return false;
            }
        }

        if self.runtime.stream_close_complete() && self.begin_websocket_close().is_err() {
            self.terminate_uncleanly().await;
            return false;
        }

        if !self.flush_deferred_commands().await {
            return false;
        }

        true
    }

    async fn handle_client_event(&mut self, event: ClientEvent) -> bool {
        if let Some(message) = self.dispatch_client_event(event).await {
            if let Err(failure) = self.send_transport_message(message).await {
                self.emit_error(failure.source().to_string()).await;
                self.terminate_uncleanly().await;
                return false;
            }
        }
        true
    }

    async fn dispatch_client_event(&mut self, event: ClientEvent) -> Option<TransportMessage> {
        match event {
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::OutboundMessage(message.clone()),
                    )))
                    .await;
                Some(message)
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Enabled { previd },
            )) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Enabled {
                            previd,
                        }),
                    )))
                    .await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Resumed { h },
            )) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Resumed { h }),
                    )))
                    .await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckReceived { h },
            )) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::AckReceived { h }),
                    )))
                    .await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::Failed,
            )) => {
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::Failed),
                    )))
                    .await;
                None
            }
            ClientEvent::IqResult { id, element } => {
                if let Some(responder) = self.pending_iqs.remove(&id) {
                    let result = if element.attr("type") == Some("result") {
                        Ok(element)
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = responder.send(result);
                } else if let Some(pending) = self.pending_mam_queries.remove(&id) {
                    resolve_pending_mam_query(pending, &element);
                } else if let Some(pending) = self.pending_inbox_queries.remove(&id) {
                    let result = if element.attr("type") == Some("result") {
                        let fin = waddle_xmpp_client::inbox::parse_inbox_fin(&element)
                            .unwrap_or_default();
                        Ok(InboxPage {
                            entries: pending.entries,
                            fin,
                        })
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = pending.responder.send(result);
                }
                None
            }
            ClientEvent::MamResult(archived) => {
                collect_pending_mam_result(&mut self.pending_mam_queries, *archived);
                None
            }
            ClientEvent::InboxStreamEntry(entry) => {
                if let Some(query_id) = entry.query_id.as_deref() {
                    if let Some((_, pending)) = self
                        .pending_inbox_queries
                        .iter_mut()
                        .find(|(_, pending)| pending.query_id == query_id)
                    {
                        pending.entries.push(entry);
                    }
                } else if entry.source == waddle_xmpp_client::inbox::InboxStreamEntrySource::Push {
                    let _ = self
                        .event_tx
                        .clone()
                        .send(client_driver_event(ClientEvent::InboxStreamEntry(entry)))
                        .await;
                } else if self.pending_inbox_queries.len() == 1 {
                    if let Some(pending) = self.pending_inbox_queries.values_mut().next() {
                        pending.entries.push(entry);
                    }
                }
                None
            }
            other => {
                let _ = self.event_tx.clone().send(client_driver_event(other)).await;
                None
            }
        }
    }

    async fn send_transport_message(
        &mut self,
        message: TransportMessage,
    ) -> Result<(), WasmPumpFailure> {
        enum PumpItem {
            Message(TransportMessage),
            Event(ClientEvent),
        }

        let mut pending = VecDeque::from([PumpItem::Message(message)]);
        let mut initiating_message_confirmed = false;
        while let Some(item) = pending.pop_front() {
            match item {
                PumpItem::Message(message) => {
                    let frame = waddle_xmpp_client::encode_message(&message).map_err(|source| {
                        WasmPumpFailure {
                            failed_message: message.clone(),
                            initiating_message_confirmed,
                            source,
                        }
                    })?;
                    self.ws
                        .send_frame(&frame)
                        .map_err(|source| WasmPumpFailure {
                            failed_message: message.clone(),
                            initiating_message_confirmed,
                            source,
                        })?;
                    if !initiating_message_confirmed {
                        initiating_message_confirmed = true;
                    }
                    let events = self
                        .runtime
                        .apply_transport_event(TransportEvent::MessageSent(message.clone()))
                        .map_err(|source| WasmPumpFailure {
                            failed_message: message.clone(),
                            initiating_message_confirmed,
                            source,
                        })?;
                    self.publish_resume_state_snapshot();
                    if self.runtime.stream_close_complete() {
                        self.begin_websocket_close()
                            .map_err(|source| WasmPumpFailure {
                                failed_message: message.clone(),
                                initiating_message_confirmed,
                                source,
                            })?;
                    }
                    for event in events.into_iter().rev() {
                        pending.push_front(PumpItem::Event(event));
                    }
                }
                PumpItem::Event(event) => {
                    if let Some(follow_up) = self.dispatch_client_event(event).await {
                        pending.push_front(PumpItem::Message(follow_up));
                    }
                }
            }
        }

        Ok(())
    }

    async fn terminate_uncleanly(&mut self) {
        self.apply_terminal_transport_event(TransportEvent::StateChanged(TransportState::Failed))
            .await;
        let _ = self.ws.close_websocket();
        self.apply_terminal_transport_event(TransportEvent::StateChanged(TransportState::Closed))
            .await;
        self.apply_terminal_transport_event(TransportEvent::Closed)
            .await;
    }

    fn begin_websocket_close(&mut self) -> DriverResult<()> {
        if self.websocket_close_started {
            return Ok(());
        }
        self.websocket_close_started = true;
        self.explicit_disconnect = true;
        self.publish_resume_state_snapshot();
        self.ws.close_websocket()
    }

    async fn apply_terminal_transport_event(&mut self, event: TransportEvent) {
        let Ok(events) = self.runtime.apply_transport_event(event) else {
            return;
        };
        self.publish_resume_state_snapshot();
        for event in events {
            let _ = self.dispatch_client_event(event).await;
        }
    }
    async fn emit_message_delivery_failed(&mut self, stanza_id: StanzaId) {
        let _ = self
            .event_tx
            .clone()
            .send(client_driver_event(ClientEvent::MessageDelivery(
                MessageDeliveryEvent::Failed { stanza_id },
            )))
            .await;
    }

    async fn emit_error(&mut self, description: String) {
        let _ = self
            .event_tx
            .clone()
            .send(DriverEvent::Error(description))
            .await;
    }

    fn publish_resume_state_snapshot(&self) {
        publish_resume_state_snapshot(&self.inner, &self.runtime, self.explicit_disconnect);
    }

    async fn finish(&mut self) {
        self.publish_resume_state_snapshot();
        let resume_state = self.inner.borrow().resume_state.clone();
        let _ = self
            .event_tx
            .clone()
            .send(DriverEvent::ResumeState(resume_state))
            .await;

        for command in self.deferred_commands.drain(..) {
            match command {
                DeferredWasmCommand::Stanza { responder, .. } => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                }
                DeferredWasmCommand::Iq { responder, .. } => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                }
                DeferredWasmCommand::MamQuery { responder, .. } => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                }
                DeferredWasmCommand::InboxQuery { responder, .. } => {
                    let _ = responder.send(Err(ClientError::Disconnected));
                }
            }
        }
        for (_, responder) in self.pending_iqs.drain() {
            let _ = responder.send(Err(ClientError::Disconnected));
        }
        for (_, pending) in self.pending_mam_queries.drain() {
            let _ = pending.responder.send(Err(ClientError::Disconnected));
        }
        for (_, pending) in self.pending_inbox_queries.drain() {
            let _ = pending.responder.send(Err(ClientError::Disconnected));
        }

        let _ = self.event_tx.clone().send(DriverEvent::Disconnected).await;
    }
}

fn minimum_wakeup(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(delay), None) | (None, Some(delay)) => Some(delay),
        (None, None) => None,
    }
}

fn publish_resume_state_snapshot(
    inner: &Rc<RefCell<WaddleClientInner>>,
    runtime: &XmppRuntime,
    explicit_disconnect: bool,
) {
    let resume_state = if explicit_disconnect {
        None
    } else {
        runtime.resume_state()
    };
    inner.borrow_mut().resume_state = resume_state;
}

fn cancel_raw_iq_state(
    pending_iqs: &mut HashMap<String, oneshot::Sender<DriverResult<Element>>>,
    deferred_commands: &mut VecDeque<DeferredWasmCommand>,
    id: &str,
) {
    if let Some(responder) = pending_iqs.remove(id) {
        let _ = responder.send(Err(ClientError::RequestCancelled));
    }

    let mut retained = VecDeque::with_capacity(deferred_commands.len());
    while let Some(command) = deferred_commands.pop_front() {
        if command.raw_iq_id() == Some(id) {
            if let DeferredWasmCommand::Iq { responder, .. } = command {
                let _ = responder.send(Err(ClientError::RequestCancelled));
            }
        } else {
            retained.push_back(command);
        }
    }
    *deferred_commands = retained;
}

/// Route a MAM result stanza to the pending query it belongs to, matching on
/// the XEP-0313 `queryid`. Results without a `queryid`, or for a query that is
/// no longer pending, are dropped; the pending query's collector weeds out
/// XEP-0198 resume-replayed duplicates by `mam_id`.
fn collect_pending_mam_result(
    pending_mam_queries: &mut HashMap<String, PendingMamQuery>,
    archived: ArchivedMessage,
) {
    if let Some(query_id) = archived.query_id.as_deref() {
        if let Some((_, pending)) = pending_mam_queries
            .iter_mut()
            .find(|(_, pending)| pending.query_id() == query_id)
        {
            pending.collect(archived);
        }
    }
}

/// Complete a pending MAM query with the IQ that resolved it: a `<fin/>` IQ
/// result yields the deduped [`waddle_xmpp_client::MamPage`], anything else a
/// stanza error.
fn resolve_pending_mam_query(pending: PendingMamQuery, element: &Element) {
    let PendingMamQuery {
        collector,
        responder,
    } = pending;
    let result = if element.attr("type") == Some("result") {
        let (rsm, is_complete) = mam::parse_fin_from_iq_result(element);
        Ok(waddle_xmpp_client::MamPage {
            query_id: collector.query_id().to_string(),
            messages: collector.into_messages(),
            rsm,
            is_complete,
        })
    } else {
        Err(ClientError::StanzaError(parse_stanza_error(element)))
    };
    let _ = responder.send(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashSet;
    use std::str::FromStr;

    use futures::executor::block_on;
    use futures::future;
    use waddle_xmpp_client::discovery::DISCO_INFO_NS;
    use waddle_xmpp_client::transport::StreamClose;

    #[derive(Clone, Default)]
    struct FakeWireState {
        frames: Rc<RefCell<Vec<String>>>,
        close_count: Rc<Cell<usize>>,
        send_attempt_count: Rc<Cell<usize>>,
        fail_on_send_attempt: Rc<Cell<Option<usize>>>,
    }

    impl FakeWireState {
        fn take_messages(&self) -> Vec<TransportMessage> {
            self.frames
                .borrow_mut()
                .drain(..)
                .map(|frame| waddle_xmpp_client::decode_message(&frame).expect("typed frame"))
                .collect()
        }

        fn fail_after_successful_sends(&self, successful_sends: usize) {
            self.fail_on_send_attempt
                .set(Some(self.send_attempt_count.get() + successful_sends + 1));
        }

        fn send_attempt_count(&self) -> usize {
            self.send_attempt_count.get()
        }
    }

    struct FakeWire {
        events: mpsc::Receiver<WasmTransportEvent>,
        state: FakeWireState,
    }

    impl FakeWire {
        fn new() -> (Self, FakeWireState) {
            let (_sender, events) = mpsc::channel(16);
            let state = FakeWireState::default();
            (
                Self {
                    events,
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl WasmDriverWire for FakeWire {
        fn events(&mut self) -> &mut mpsc::Receiver<WasmTransportEvent> {
            &mut self.events
        }

        fn send_frame(&mut self, frame: &str) -> DriverResult<()> {
            let attempt = self.state.send_attempt_count.get() + 1;
            self.state.send_attempt_count.set(attempt);
            if self.state.fail_on_send_attempt.get() == Some(attempt) {
                self.state.fail_on_send_attempt.set(None);
                return Err(ClientError::TransportClosed);
            }
            self.state.frames.borrow_mut().push(frame.to_string());
            Ok(())
        }

        fn close_websocket(&mut self) -> DriverResult<()> {
            self.state.close_count.set(self.state.close_count.get() + 1);
            Ok(())
        }
    }

    #[derive(Default)]
    struct ManualTimerState {
        next_id: u64,
        active: HashSet<u64>,
        cancelled: HashSet<u64>,
        max_active: usize,
    }

    #[derive(Clone, Default)]
    struct ManualTimerBackend {
        state: Rc<RefCell<ManualTimerState>>,
    }

    impl ManualTimerBackend {
        fn active_count(&self) -> usize {
            self.state.borrow().active.len()
        }

        fn max_active(&self) -> usize {
            self.state.borrow().max_active
        }

        fn last_id(&self) -> u64 {
            self.state.borrow().next_id
        }

        fn callback_can_act(&self, id: u64) -> bool {
            let state = self.state.borrow();
            state.active.contains(&id) && !state.cancelled.contains(&id)
        }
    }

    struct ManualTimerWait {
        id: u64,
        state: Rc<RefCell<ManualTimerState>>,
    }

    impl Future for ManualTimerWait {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for ManualTimerWait {
        fn drop(&mut self) {
            let mut state = self.state.borrow_mut();
            state.active.remove(&self.id);
            state.cancelled.insert(self.id);
        }
    }

    impl DriverTimerBackend for ManualTimerBackend {
        fn wait(&self, delay_ms: Option<u64>) -> futures::future::LocalBoxFuture<'static, ()> {
            if delay_ms.is_none() {
                return Box::pin(future::pending());
            }
            let id = {
                let mut state = self.state.borrow_mut();
                state.next_id += 1;
                let id = state.next_id;
                state.active.insert(id);
                state.max_active = state.max_active.max(state.active.len());
                id
            };
            Box::pin(ManualTimerWait {
                id,
                state: self.state.clone(),
            })
        }
    }

    fn test_inner() -> Rc<RefCell<WaddleClientInner>> {
        Rc::new(RefCell::new(WaddleClientInner {
            config: StoredConfig {
                server_url: "wss://xmpp.example.test".to_string(),
                jid: "alice@example.test".to_string(),
                access_token: "token".to_string(),
                resource: "web".to_string(),
                resume_state: None,
            },
            cmd_tx: None,
            on_message: None,
            on_presence: None,
            on_connected: None,
            on_session_lifecycle: None,
            on_stream_management: None,
            on_disconnected: None,
            on_error: None,
            on_message_delivery_acked: None,
            on_message_delivery_failed: None,
            on_mds_displayed: None,
            on_pubsub_event: None,
            on_call: None,
            resume_state: None,
        }))
    }

    fn test_config(resume_state: Option<waddle_xmpp_client::SmResumeState>) -> ClientConfig {
        build_client_config(&StoredConfig {
            server_url: "wss://xmpp.example.test/ws".to_string(),
            jid: "alice@example.test".to_string(),
            access_token: "token".to_string(),
            resource: "web".to_string(),
            resume_state,
        })
        .expect("test config")
    }

    fn test_driver(
        config: ClientConfig,
    ) -> (
        WasmDriverTask,
        FakeWireState,
        mpsc::Receiver<DriverEvent>,
        Rc<RefCell<WaddleClientInner>>,
    ) {
        let (wire, wire_state) = FakeWire::new();
        let (_command_sender, command_receiver) = mpsc::channel(16);
        let (event_sender, event_receiver) = mpsc::channel(256);
        let inner = test_inner();
        let task = WasmDriverTask::new_with_dependencies(
            config,
            Box::new(wire),
            Rc::new(ManualTimerBackend::default()),
            command_receiver,
            event_sender,
            inner.clone(),
        )
        .expect("driver task");
        (task, wire_state, event_receiver, inner)
    }

    fn pre_auth_features() -> Element {
        Element::builder("features", waddle_xmpp_client::NS_STREAMS)
            .append(
                Element::builder("mechanisms", waddle_xmpp_client::NS_SASL)
                    .append(
                        Element::builder("mechanism", waddle_xmpp_client::NS_SASL)
                            .append("OAUTHBEARER")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    fn post_auth_features_with_sm() -> Element {
        Element::builder("features", waddle_xmpp_client::NS_STREAMS)
            .append(Element::builder("bind", waddle_xmpp_client::NS_BIND).build())
            .append(
                Element::builder("sm", waddle_xmpp_client::stream_management::NS_SM)
                    .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                    .build(),
            )
            .build()
    }

    fn bind_result(stanza_id: &str) -> Element {
        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("bind", waddle_xmpp_client::NS_BIND)
                    .append(
                        Element::builder("jid", waddle_xmpp_client::NS_BIND)
                            .append("alice@example.test/web")
                            .build(),
                    )
                    .build(),
            )
            .build()
    }

    async fn drive_to_post_auth_sm_features(task: &mut WasmDriverTask) {
        task.runtime
            .queue_request(ClientRequest::Connect)
            .expect("connect request");
        assert!(
            task.apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                waddle_xmpp_client::transport::StreamOpen::from_server(
                    BareJid::from_str("example.test").expect("server jid"),
                )
            ),))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                pre_auth_features()
            ),))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("success", waddle_xmpp_client::NS_SASL).build(),
            ),))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                waddle_xmpp_client::transport::StreamOpen::from_server(
                    BareJid::from_str("example.test").expect("server jid"),
                )
            ),))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                post_auth_features_with_sm()
            ),))
                .await
        );
    }

    async fn drive_to_fresh_sm_enabled(task: &mut WasmDriverTask) {
        drive_to_post_auth_sm_features(task).await;
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                bind_result("bind-1")
            ),))
                .await
        );
        assert!(
            task.apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("enabled", waddle_xmpp_client::stream_management::NS_SM,)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "stream-1")
                    .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                    .build(),
            ),))
                .await
        );
        assert!(task.runtime.can_send_app_stanza());
    }

    async fn apply_test_transport_event_at(
        task: &mut WasmDriverTask,
        event: TransportEvent,
        now_ms: u64,
    ) -> bool {
        let events = task
            .runtime
            .apply_transport_event_at(event, now_ms)
            .expect("test transport event");
        task.publish_resume_state_snapshot();
        for event in events {
            if !task.handle_client_event(event).await {
                return false;
            }
        }
        task.flush_deferred_commands().await
    }

    fn take_ack_request_attempts(events: &mut mpsc::Receiver<DriverEvent>) -> Vec<(u32, u32)> {
        let mut attempts = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let DriverEvent::Client(event) = event {
                if let ClientEvent::Connection(ConnectionEvent::StreamManagement(
                    StreamManagementEvent::AckRequestSent { attempt, unacked },
                )) = *event
                {
                    attempts.push((attempt, unacked));
                }
            }
        }
        attempts
    }

    fn build_archived(mam_id: &str, query_id: &str, body: &str) -> ArchivedMessage {
        let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
            mam_id,
            "room@muc.example.com".parse().expect("valid archive jid"),
        );
        ArchivedMessage {
            mam_id: mam_id.to_string(),
            query_id: Some(query_id.to_string()),
            id: None,
            stanza_id: Some(stanza_id.clone()),
            origin_id: None,
            timestamp: None,
            from: Some("room@muc.example.com/alice".to_string()),
            to: Some("alice@example.com/res".to_string()),
            stanza_ids: vec![stanza_id],
            parent_thread_id: None,
            message_type: "groupchat".to_string(),
            body: Some(body.to_string()),
            thread: None,
            author_real_jid: None,
            inner: Element::builder("message", NS_CLIENT).build(),
            payload: Default::default(),
        }
    }

    #[test]
    fn mam_collection_dedups_replayed_results_and_filters_foreign_query_ids() {
        let mut pending_mam_queries = HashMap::new();
        let (responder, _rx) = oneshot::channel();
        pending_mam_queries.insert(
            "iq-1".to_string(),
            PendingMamQuery::new("query-1", responder),
        );

        // Original delivery.
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-0", "query-1", "hello 0"),
        );
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-1", "query-1", "hello 1"),
        );
        // XEP-0198 resume replays the unacked tail: same queryid, same mam_id.
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-1", "query-1", "hello 1"),
        );
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-0", "query-1", "hello 0"),
        );
        // A result for another open query must never be collected.
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-9", "some-other-query", "noise"),
        );

        let pending = pending_mam_queries
            .remove("iq-1")
            .expect("pending query still registered");
        let messages = pending.collector.into_messages();
        assert_eq!(
            messages.len(),
            2,
            "replayed results with an already-collected mam_id must be dropped"
        );
        assert_eq!(messages[0].mam_id, "mam-0");
        assert_eq!(messages[1].mam_id, "mam-1");
    }

    #[test]
    fn mam_collection_drops_results_without_a_query_id() {
        let mut pending_mam_queries = HashMap::new();
        let (responder, _rx) = oneshot::channel();
        pending_mam_queries.insert(
            "iq-1".to_string(),
            PendingMamQuery::new("query-1", responder),
        );

        let mut archived = build_archived("mam-0", "query-1", "hello 0");
        archived.query_id = None;
        collect_pending_mam_result(&mut pending_mam_queries, archived);

        let pending = pending_mam_queries
            .remove("iq-1")
            .expect("pending query still registered");
        assert!(
            pending.collector.into_messages().is_empty(),
            "a result without a queryid must not be attributed to any pending query"
        );
    }

    fn build_fin_iq(iq_id: &str, first: &str, last: &str, count: u32) -> Element {
        let set = Element::builder("set", waddle_xmpp_core::mam::RSM_NS)
            .append(
                Element::builder("first", waddle_xmpp_core::mam::RSM_NS)
                    .append(first)
                    .build(),
            )
            .append(
                Element::builder("last", waddle_xmpp_core::mam::RSM_NS)
                    .append(last)
                    .build(),
            )
            .append(
                Element::builder("count", waddle_xmpp_core::mam::RSM_NS)
                    .append(count.to_string())
                    .build(),
            )
            .build();

        let fin = Element::builder("fin", waddle_xmpp_core::mam::MAM_NS)
            .attr(minidom::rxml::xml_ncname!("complete").to_owned(), "true")
            .append(set)
            .build();

        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
            .append(fin)
            .build()
    }

    #[test]
    fn fin_iq_result_resolves_pending_query_with_deduped_page() {
        let mut pending_mam_queries = HashMap::new();
        let (responder, mut rx) = oneshot::channel();
        pending_mam_queries.insert(
            "iq-1".to_string(),
            PendingMamQuery::new("query-1", responder),
        );

        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-0", "query-1", "hello 0"),
        );
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-1", "query-1", "hello 1"),
        );
        // XEP-0198 resume replays the unacked tail before the (also replayed)
        // <fin/> arrives.
        collect_pending_mam_result(
            &mut pending_mam_queries,
            build_archived("mam-1", "query-1", "hello 1"),
        );

        let pending = pending_mam_queries
            .remove("iq-1")
            .expect("pending query still registered");
        resolve_pending_mam_query(pending, &build_fin_iq("iq-1", "mam-0", "mam-1", 2));

        let page = rx
            .try_recv()
            .expect("responder completed")
            .expect("responder sent a result")
            .expect("fin result resolves to a page");
        assert_eq!(page.query_id, "query-1");
        assert_eq!(
            page.messages.len(),
            2,
            "the page must carry the deduped rows, not the replayed duplicates"
        );
        assert_eq!(page.messages[0].mam_id, "mam-0");
        assert_eq!(page.messages[1].mam_id, "mam-1");
        assert!(page.is_complete, "<fin complete='true'/> must propagate");
        assert_eq!(page.rsm.first.as_deref(), Some("mam-0"));
        assert_eq!(page.rsm.last.as_deref(), Some("mam-1"));
        assert_eq!(page.rsm.count, Some(2));
    }

    #[test]
    fn publish_resume_state_snapshot_updates_shared_client_state() {
        let inner = test_inner();
        let stored = StoredConfig {
            server_url: "wss://xmpp.example.test".to_string(),
            jid: "alice@example.test".to_string(),
            access_token: "token".to_string(),
            resource: "web".to_string(),
            resume_state: Some(
                waddle_xmpp_client::SmResumeState::new("previous-stream", 4, 9)
                    .map(|state| state.with_max_resume_seconds(Some(300)))
                    .expect("resume state"),
            ),
        };
        let runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");

        publish_resume_state_snapshot(&inner, &runtime, false);

        let borrowed = inner.borrow();
        let snapshot = borrowed.resume_state.as_ref().expect("snapshot");
        assert_eq!(snapshot.previd(), "previous-stream");
        assert_eq!(snapshot.inbound_h(), 4);
        assert_eq!(snapshot.outbound_h(), 9);
        assert_eq!(snapshot.max_resume_seconds(), Some(300));
    }

    #[test]
    fn publish_resume_state_snapshot_tracks_live_ack_mutations() {
        let inner = test_inner();
        let stanza = Element::builder("message", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unacked")
            .build();
        let stored = StoredConfig {
            server_url: "wss://xmpp.example.test".to_string(),
            jid: "alice@example.test".to_string(),
            access_token: "token".to_string(),
            resource: "web".to_string(),
            resume_state: None,
        };
        let mut runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");
        runtime
            .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
                waddle_xmpp_client::stream_management::SmState::build_enable(true),
            )))
            .expect("enable sent");
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("enabled", waddle_xmpp_client::stream_management::NS_SM)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "live-stream")
                    .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                    .attr(minidom::rxml::xml_ncname!("max").to_owned(), "300")
                    .build(),
            )))
            .expect("enabled");
        runtime
            .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
                stanza,
            )))
            .expect("message sent");

        publish_resume_state_snapshot(&inner, &runtime, false);
        assert!(inner
            .borrow()
            .resume_state
            .as_ref()
            .expect("snapshot")
            .has_unhandled_outbound_stanzas());
        assert_eq!(
            inner
                .borrow()
                .resume_state
                .as_ref()
                .expect("snapshot")
                .max_resume_seconds(),
            Some(300),
        );

        let ack = Element::builder("a", waddle_xmpp_client::stream_management::NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
            .build();
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                ack,
            )))
            .expect("ack");
        publish_resume_state_snapshot(&inner, &runtime, false);

        assert!(!inner
            .borrow()
            .resume_state
            .as_ref()
            .expect("snapshot")
            .has_unhandled_outbound_stanzas());
    }

    #[test]
    fn publish_resume_state_snapshot_clears_on_explicit_disconnect() {
        let inner = test_inner();
        inner.borrow_mut().resume_state = Some(
            waddle_xmpp_client::SmResumeState::new("previous-stream", 4, 9).expect("resume state"),
        );
        let stored = inner.borrow().config.clone();
        let runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");

        publish_resume_state_snapshot(&inner, &runtime, true);

        assert!(inner.borrow().resume_state.is_none());
    }

    #[test]
    fn wasm_peer_initiated_close_writes_reciprocal_before_websocket_close() {
        block_on(async {
            let (mut task, wire, _events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            let (responder, response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza: Element::builder("message", NS_CLIENT)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            "wasm-peer-close",
                        )
                        .build(),
                    responder,
                }))
                .await
            );
            assert!(response.await.expect("send response").is_ok());
            wire.take_messages();
            assert!(inner.borrow().resume_state.is_some());

            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Close(StreamClose),
                ))
                .await
            );

            assert!(matches!(
                wire.take_messages().as_slice(),
                [TransportMessage::Close(_)]
            ));
            assert_eq!(wire.close_count.get(), 1);
            assert!(task.websocket_close_started);
            assert!(task.runtime.stream_close_complete());
            assert!(inner.borrow().resume_state.is_none());
        });
    }

    #[test]
    fn wasm_local_close_waits_for_peer_before_websocket_close_and_sm_destruction() {
        block_on(async {
            let (mut task, wire, _events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            let (send_responder, send_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza: Element::builder("message", NS_CLIENT)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            "wasm-local-close",
                        )
                        .build(),
                    responder: send_responder,
                }))
                .await
            );
            assert!(send_response.await.expect("send response").is_ok());
            wire.take_messages();

            let (responder, response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::Disconnect { responder }))
                    .await
            );
            assert!(response.await.expect("disconnect response").is_ok());
            assert!(matches!(
                wire.take_messages().as_slice(),
                [TransportMessage::Close(_)]
            ));
            assert_eq!(wire.close_count.get(), 0);
            assert!(!task.runtime.stream_close_complete());
            assert!(inner.borrow().resume_state.is_some());

            let (ack_responder, ack_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::RequestStreamManagementAck {
                    responder: ack_responder,
                }))
                .await
            );
            assert!(ack_response.await.expect("ack response").is_ok());
            assert!(wire.take_messages().is_empty());
            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Element(
                        waddle_xmpp_client::stream_management::SmState::build_request_ack(),
                    ),
                ))
                .await
            );
            assert!(wire.take_messages().is_empty());
            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Element(
                        waddle_xmpp_client::stream_management::SmState::build_ack(1),
                    ),
                ))
                .await
            );
            assert!(wire.take_messages().is_empty());
            assert!(inner
                .borrow()
                .resume_state
                .as_ref()
                .is_some_and(|state| !state.has_unhandled_outbound_stanzas()));

            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Close(StreamClose),
                ))
                .await
            );
            assert_eq!(wire.close_count.get(), 1);
            assert!(task.websocket_close_started);
            assert!(task.runtime.stream_close_complete());
            assert!(inner.borrow().resume_state.is_none());
        });
    }

    #[test]
    fn repeated_wasm_disconnects_coalesce_to_one_xml_and_websocket_close() {
        block_on(async {
            let (mut task, wire, _events, _inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();

            for _ in 0..2 {
                let (responder, response) = oneshot::channel();
                assert!(
                    task.handle_command(Some(WasmCommand::Disconnect { responder }))
                        .await
                );
                assert!(response.await.expect("disconnect response").is_ok());
            }
            assert!(matches!(
                wire.take_messages().as_slice(),
                [TransportMessage::Close(_)]
            ));
            assert_eq!(wire.close_count.get(), 0);

            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Close(StreamClose),
                ))
                .await
            );
            assert_eq!(wire.close_count.get(), 1);
        });
    }

    #[test]
    fn wasm_command_channel_loss_before_close_aborts_and_preserves_resume_state() {
        block_on(async {
            let (mut task, wire, _events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            let (responder, response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza: Element::builder("message", NS_CLIENT)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            "wasm-channel-loss",
                        )
                        .build(),
                    responder,
                }))
                .await
            );
            assert!(response.await.expect("send response").is_ok());
            let resume_before = inner.borrow().resume_state.clone();

            assert!(!task.handle_command(None).await);
            assert!(task.commands_closed);
            assert_eq!(wire.close_count.get(), 1);
            assert_eq!(inner.borrow().resume_state, resume_before);
        });
    }

    #[test]
    fn wasm_channel_loss_during_half_close_waits_then_times_out_uncleanly() {
        block_on(async {
            let (mut task, wire, _events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            let (send_responder, send_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza: Element::builder("message", NS_CLIENT)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            "wasm-half-close-timeout",
                        )
                        .build(),
                    responder: send_responder,
                }))
                .await
            );
            assert!(send_response.await.expect("send response").is_ok());
            wire.take_messages();
            let (disconnect_responder, disconnect_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::Disconnect {
                    responder: disconnect_responder,
                }))
                .await
            );
            assert!(disconnect_response
                .await
                .expect("disconnect response")
                .is_ok());
            let resume_before = inner.borrow().resume_state.clone();

            assert!(task.handle_command(None).await);
            assert!(task.commands_closed);
            assert_eq!(wire.close_count.get(), 0);
            assert!(!task.handle_driver_timer(true).await);
            assert_eq!(wire.close_count.get(), 1);
            assert_eq!(inner.borrow().resume_state, resume_before);
        });
    }

    #[test]
    fn wasm_failed_peer_close_reciprocal_preserves_resume_state() {
        block_on(async {
            let (mut task, wire, _events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            let (responder, response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza: Element::builder("message", NS_CLIENT)
                        .attr(
                            minidom::rxml::xml_ncname!("id").to_owned(),
                            "wasm-close-failure",
                        )
                        .build(),
                    responder,
                }))
                .await
            );
            assert!(response.await.expect("send response").is_ok());
            wire.take_messages();
            let resume_before = inner.borrow().resume_state.clone();
            wire.fail_after_successful_sends(0);

            assert!(
                !task
                    .apply_transport_event(TransportEvent::MessageReceived(
                        TransportMessage::Close(StreamClose),
                    ))
                    .await
            );

            assert_eq!(inner.borrow().resume_state, resume_before);
            assert!(!task.runtime.stream_close_complete());
            assert!(!task.websocket_close_started);
            assert_eq!(wire.close_count.get(), 1, "unclean transport abort only");
            assert!(wire.take_messages().is_empty());
        });
    }

    #[test]
    fn fresh_send_command_pumps_stanza_then_exactly_one_typed_ack_request() {
        block_on(async {
            let (mut task, wire, _events, _inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();

            let stanza = Element::builder("message", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fresh-1")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build();
            let (responder, response) = oneshot::channel();

            assert!(
                task.handle_command(Some(WasmCommand::SendStanza { stanza, responder }))
                    .await
            );
            assert!(response.await.expect("command response").is_ok());

            let messages = wire.take_messages();
            assert_eq!(messages.len(), 2);
            assert!(matches!(
                &messages[0],
                TransportMessage::Element(element)
                    if element.name() == "message" && element.attr("id") == Some("fresh-1")
            ));
            assert!(matches!(
                &messages[1],
                TransportMessage::Element(element)
                    if waddle_xmpp_client::stream_management::SmState::is_request_ack(element)
            ));
        });
    }

    #[test]
    fn second_frame_failure_keeps_confirmed_message_sent_and_terminates_uncleanly() {
        block_on(async {
            let (mut task, wire, mut events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();
            wire.fail_after_successful_sends(1);
            let attempts_before = wire.send_attempt_count();

            let stanza = Element::builder("message", NS_CLIENT)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "confirmed-wasm",
                )
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build();
            let (responder, response) = oneshot::channel();

            assert!(
                !task
                    .handle_command(Some(WasmCommand::SendStanza { stanza, responder }))
                    .await
            );
            assert!(
                response.await.expect("command response").is_ok(),
                "a generated control-frame failure must not return a retryable message result"
            );
            task.finish().await;

            assert_eq!(wire.send_attempt_count() - attempts_before, 2);
            let messages = wire.take_messages();
            assert_eq!(messages.len(), 1, "only the initiating frame was accepted");
            assert!(matches!(
                &messages[0],
                TransportMessage::Element(element)
                    if element.name() == "message" && element.attr("id") == Some("confirmed-wasm")
            ));
            let resume = inner
                .borrow()
                .resume_state
                .clone()
                .expect("confirmed stanza remains resumable");
            assert_eq!(
                resume
                    .unhandled_message_stanza_ids()
                    .iter()
                    .map(StanzaId::as_str)
                    .collect::<Vec<_>>(),
                vec!["confirmed-wasm"]
            );
            assert_eq!(wire.close_count.get(), 1);

            let mut failed_delivery_count = 0;
            let mut saw_failed = false;
            let mut saw_closed = false;
            let mut saw_disconnected = false;
            while let Ok(event) = events.try_recv() {
                match event {
                    DriverEvent::Client(event) => match *event {
                        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
                            ref stanza_id,
                        }) if stanza_id.as_str() == "confirmed-wasm" => {
                            failed_delivery_count += 1;
                        }
                        ClientEvent::Transport(TransportEvent::StateChanged(
                            TransportState::Failed,
                        )) => saw_failed = true,
                        ClientEvent::Transport(TransportEvent::Closed) => saw_closed = true,
                        _ => {}
                    },
                    DriverEvent::Disconnected => saw_disconnected = true,
                    _ => {}
                }
            }
            assert_eq!(failed_delivery_count, 0);
            assert!(saw_failed);
            assert!(saw_closed);
            assert!(saw_disconnected);
        });
    }

    #[test]
    fn successful_resume_pumps_replay_then_exactly_one_immediate_ack_request() {
        block_on(async {
            let replay = Element::builder("message", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "replay-1")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build();
            let resume_state = waddle_xmpp_client::SmResumeState::from_unhandled_outbound_stanzas(
                "previous-stream",
                0,
                1,
                [replay],
            )
            .expect("resume state");
            let (mut task, wire, _events, _inner) = test_driver(test_config(Some(resume_state)));
            drive_to_post_auth_sm_features(&mut task).await;
            wire.take_messages();

            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Element(
                        Element::builder("resumed", waddle_xmpp_client::stream_management::NS_SM,)
                            .attr(
                                minidom::rxml::xml_ncname!("previd").to_owned(),
                                "previous-stream",
                            )
                            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                            .build(),
                    ),
                ))
                .await
            );

            let messages = wire.take_messages();
            assert_eq!(messages.len(), 2);
            assert!(matches!(
                &messages[0],
                TransportMessage::Element(element)
                    if element.name() == "message" && element.attr("id") == Some("replay-1")
            ));
            assert!(matches!(
                &messages[1],
                TransportMessage::Element(element)
                    if waddle_xmpp_client::stream_management::SmState::is_request_ack(element)
            ));
            assert_eq!(
                task.runtime
                    .resume_state()
                    .expect("live resume state")
                    .outbound_h(),
                1
            );
        });
    }

    #[test]
    fn successful_resume_drives_full_wire_retry_ladder_and_original_stall_deadline() {
        block_on(async {
            let replay = Element::builder("message", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "retry-replay")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
                .build();
            let resume_state = waddle_xmpp_client::SmResumeState::from_unhandled_outbound_stanzas(
                "previous-stream",
                0,
                1,
                [replay],
            )
            .expect("resume state");
            let (mut task, wire, mut events, _inner) = test_driver(test_config(Some(resume_state)));
            drive_to_post_auth_sm_features(&mut task).await;
            wire.take_messages();
            take_ack_request_attempts(&mut events);

            assert!(
                apply_test_transport_event_at(
                    &mut task,
                    TransportEvent::MessageReceived(TransportMessage::Element(
                        Element::builder("resumed", waddle_xmpp_client::stream_management::NS_SM)
                            .attr(
                                minidom::rxml::xml_ncname!("previd").to_owned(),
                                "previous-stream",
                            )
                            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                            .build(),
                    )),
                    10_000,
                )
                .await
            );
            let resume_writes = wire.take_messages();
            assert_eq!(resume_writes.len(), 2, "replay then immediate <r/>");
            assert!(matches!(
                &resume_writes[0],
                TransportMessage::Element(element)
                    if element.name() == "message" && element.attr("id") == Some("retry-replay")
            ));
            assert!(matches!(
                &resume_writes[1],
                TransportMessage::Element(element)
                    if waddle_xmpp_client::stream_management::SmState::is_request_ack(element)
            ));
            assert_eq!(take_ack_request_attempts(&mut events), vec![(1, 1)]);
            assert_eq!(
                task.runtime
                    .resume_state()
                    .expect("resume snapshot")
                    .outbound_h(),
                1,
                "replay must not increment outbound h"
            );

            let mut ack_at_ms = 10_010;
            for (delay_ms, expected_attempt) in [
                (250, 2),
                (500, 3),
                (1_000, 4),
                (2_000, 5),
                (5_000, 6),
                (5_000, 7),
            ] {
                assert!(
                    apply_test_transport_event_at(
                        &mut task,
                        TransportEvent::MessageReceived(TransportMessage::Element(
                            Element::builder("a", waddle_xmpp_client::stream_management::NS_SM,)
                                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                                .build(),
                        )),
                        ack_at_ms,
                    )
                    .await
                );
                assert!(wire.take_messages().is_empty());

                let request_at_ms = ack_at_ms + delay_ms;
                assert!(task.handle_stream_management_timer_at(request_at_ms).await);
                let retry_writes = wire.take_messages();
                assert_eq!(retry_writes.len(), 1);
                assert!(matches!(
                    &retry_writes[0],
                    TransportMessage::Element(element)
                        if waddle_xmpp_client::stream_management::SmState::is_request_ack(element)
                ));
                assert_eq!(
                    take_ack_request_attempts(&mut events),
                    vec![(expected_attempt, 1)]
                );
                ack_at_ms = request_at_ms + 1;
            }

            assert!(
                !task.handle_stream_management_timer_at(40_000).await,
                "30s without h progress must terminate from the original 10s epoch"
            );
            assert_eq!(wire.close_count.get(), 1);
            assert_eq!(
                task.runtime
                    .resume_state()
                    .expect("stalled resume snapshot")
                    .outbound_h(),
                1
            );
        });
    }

    #[test]
    fn typed_pagehide_command_pumps_ack_request_before_resolving() {
        block_on(async {
            let (mut task, wire, _events, _inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();

            let stanza = Element::builder("message", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "pagehide-1")
                .build();
            let (send_responder, send_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza {
                    stanza,
                    responder: send_responder,
                }))
                .await
            );
            assert!(send_response.await.expect("send response").is_ok());
            wire.take_messages();

            assert!(
                task.apply_transport_event(TransportEvent::MessageReceived(
                    TransportMessage::Element(
                        Element::builder("a", waddle_xmpp_client::stream_management::NS_SM)
                            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                            .build(),
                    ),
                ))
                .await
            );
            let (ack_responder, ack_response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::RequestStreamManagementAck {
                    responder: ack_responder,
                }))
                .await
            );
            assert!(ack_response.await.expect("ack command response").is_ok());

            let messages = wire.take_messages();
            assert_eq!(messages.len(), 1);
            assert!(matches!(
                &messages[0],
                TransportMessage::Element(element)
                    if waddle_xmpp_client::stream_management::SmState::is_request_ack(element)
            ));
        });
    }

    #[test]
    fn canceled_sm_timers_cannot_survive_socket_command_or_shutdown_wins() {
        block_on(async {
            let backend = ManualTimerBackend::default();
            for index in 0..12 {
                let input = if index % 2 == 0 {
                    select_driver_input(
                        future::ready(Some(WasmTransportEvent::Open)),
                        future::pending::<Option<WasmCommand>>(),
                        backend.wait(Some(5_000)),
                    )
                    .await
                } else {
                    let (responder, _response) = oneshot::channel();
                    select_driver_input(
                        future::pending::<Option<WasmTransportEvent>>(),
                        future::ready(Some(WasmCommand::RequestStreamManagementAck { responder })),
                        backend.wait(Some(5_000)),
                    )
                    .await
                };
                assert!(matches!(
                    input,
                    DriverInput::Wire(_) | DriverInput::Command(_)
                ));
                assert_eq!(backend.active_count(), 0);
                assert!(backend.max_active() <= 1);
            }

            let old_timer = backend.wait(Some(5_000));
            let old_id = backend.last_id();
            assert!(backend.callback_can_act(old_id));
            drop(old_timer);
            let replacement = backend.wait(Some(5_000));
            let replacement_id = backend.last_id();
            assert!(!backend.callback_can_act(old_id));
            assert!(backend.callback_can_act(replacement_id));
            drop(replacement);
            assert!(!backend.callback_can_act(replacement_id));
            assert_eq!(backend.active_count(), 0);
        });
    }

    #[test]
    fn wasm_stall_preserves_resume_state_and_emits_stall_then_disconnect() {
        block_on(async {
            let (mut task, wire, mut events, inner) = test_driver(test_config(None));
            drive_to_fresh_sm_enabled(&mut task).await;
            wire.take_messages();

            let stanza = Element::builder("message", NS_CLIENT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "stalled-1")
                .build();
            let (responder, response) = oneshot::channel();
            assert!(
                task.handle_command(Some(WasmCommand::SendStanza { stanza, responder }))
                    .await
            );
            assert!(response.await.expect("send response").is_ok());
            let resume_before = inner.borrow().resume_state.clone();
            assert!(resume_before.is_some());

            assert!(!task.handle_stream_management_timer_at(u64::MAX).await);
            task.finish().await;

            assert_eq!(wire.close_count.get(), 1);
            assert_eq!(inner.borrow().resume_state, resume_before);
            let mut saw_stall = false;
            let mut saw_disconnected = false;
            while let Ok(event) = events.try_recv() {
                match event {
                    DriverEvent::Client(event)
                        if matches!(
                            *event,
                            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                                StreamManagementEvent::AckProgressStalled { .. }
                            ))
                        ) =>
                    {
                        saw_stall = true;
                    }
                    DriverEvent::Disconnected => saw_disconnected = true,
                    _ => {}
                }
            }
            assert!(saw_stall, "browser telemetry needs the reconnect signal");
            assert!(
                saw_disconnected,
                "browser lifecycle needs the disconnect signal"
            );
        });
    }

    fn iq(id: &str) -> Element {
        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .append(Element::builder("query", DISCO_INFO_NS).build())
            .build()
    }

    #[test]
    fn cancel_raw_iq_removes_sent_pending_responder() {
        let (responder, rx) = oneshot::channel();
        let mut pending_iqs = HashMap::from([("sent-1".to_string(), responder)]);
        let mut deferred_commands = VecDeque::new();

        cancel_raw_iq_state(&mut pending_iqs, &mut deferred_commands, "sent-1");

        assert!(!pending_iqs.contains_key("sent-1"));
        assert!(matches!(
            block_on(rx).expect("responder should send"),
            Err(ClientError::RequestCancelled)
        ));

        let late = iq("sent-1");
        assert!(pending_iqs.remove("sent-1").is_none());
        drop(late);
    }

    #[test]
    fn cancel_raw_iq_removes_not_yet_sent_deferred_responder() {
        let (cancelled_responder, cancelled_rx) = oneshot::channel();
        let (retained_responder, mut retained_rx) = oneshot::channel();
        let mut pending_iqs = HashMap::new();
        let mut deferred_commands = VecDeque::from([
            DeferredWasmCommand::Iq {
                stanza: iq("deferred-1"),
                responder: cancelled_responder,
            },
            DeferredWasmCommand::Iq {
                stanza: iq("deferred-2"),
                responder: retained_responder,
            },
        ]);

        cancel_raw_iq_state(&mut pending_iqs, &mut deferred_commands, "deferred-1");

        assert_eq!(deferred_commands.len(), 1);
        assert_eq!(deferred_commands[0].raw_iq_id(), Some("deferred-2"));
        assert!(matches!(
            block_on(cancelled_rx).expect("responder should send"),
            Err(ClientError::RequestCancelled)
        ));
        assert!(retained_rx
            .try_recv()
            .expect("receiver should remain open")
            .is_none());
    }
}
