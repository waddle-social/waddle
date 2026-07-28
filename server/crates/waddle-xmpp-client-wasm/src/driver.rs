use super::*;

#[derive(Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub(crate) enum JsStreamManagementTelemetry {
    AckRequested { reason: JsSmAckRequestReason },
    AckValidated { progress: bool },
    AckRetry { attempt: u8 },
    AckRequestTimedOut,
    ProgressTimedOut,
    Failed,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum JsSmAckRequestReason {
    OutboundStanza,
    ResumedUnackedTail,
    PeerRequest,
    Pagehide,
}

impl From<waddle_xmpp_client::SmAckRequestReason> for JsSmAckRequestReason {
    fn from(reason: waddle_xmpp_client::SmAckRequestReason) -> Self {
        match reason {
            waddle_xmpp_client::SmAckRequestReason::OutboundStanza => Self::OutboundStanza,
            waddle_xmpp_client::SmAckRequestReason::ResumedUnackedTail => Self::ResumedUnackedTail,
            waddle_xmpp_client::SmAckRequestReason::PeerRequest => Self::PeerRequest,
            waddle_xmpp_client::SmAckRequestReason::Pagehide => Self::Pagehide,
        }
    }
}

/// The browser owns only wakeups. This small, deterministic scheduler owns
/// their lifetime, while the Rust runtime remains the sole owner of every
/// XEP-0198 deadline and transition.
#[derive(Debug, Default)]
pub(crate) struct SmClockTimerSchedule {
    next_generation: u64,
    active_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmClockTimerTransition {
    Noop,
    Arm { generation: u64 },
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmClockTimerArmFailure {
    WindowUnavailable,
    IntervalUnavailable,
}

impl From<SmClockTimerArmFailure> for ClientError {
    fn from(_: SmClockTimerArmFailure) -> Self {
        Self::StreamManagementClockUnavailable
    }
}

impl SmClockTimerSchedule {
    fn sync(&mut self, pending: bool) -> SmClockTimerTransition {
        match (pending, self.active_generation) {
            (true, Some(_)) | (false, None) => SmClockTimerTransition::Noop,
            (true, None) => {
                self.next_generation = self.next_generation.wrapping_add(1);
                // Zero is reserved for the pre-arm state so an obsolete
                // callback can never be mistaken for a live timer.
                if self.next_generation == 0 {
                    self.next_generation = 1;
                }
                SmClockTimerTransition::Arm {
                    generation: self.next_generation,
                }
            }
            (false, Some(_)) => {
                self.active_generation = None;
                SmClockTimerTransition::Clear
            }
        }
    }

    fn accepts_wakeup(&self, generation: u64, pending: bool) -> bool {
        pending && self.active_generation == Some(generation)
    }

    /// Commit a successfully installed browser interval. Until this point a
    /// generation is provisional and cannot admit a wakeup.
    fn install<T>(
        &mut self,
        generation: u64,
        install: impl FnOnce(u64) -> Result<T, SmClockTimerArmFailure>,
    ) -> Result<T, SmClockTimerArmFailure> {
        let installed = install(generation)?;
        debug_assert!(self.active_generation.is_none());
        self.active_generation = Some(generation);
        Ok(installed)
    }

    fn clear(&mut self) {
        self.active_generation = None;
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    #[test]
    fn stream_management_telemetry_uses_closed_kebab_case_values() {
        let pagehide = serde_json::to_value(JsStreamManagementTelemetry::AckRequested {
            reason: JsSmAckRequestReason::Pagehide,
        })
        .unwrap();
        let no_progress =
            serde_json::to_value(JsStreamManagementTelemetry::AckValidated { progress: false })
                .unwrap();
        let timeout = serde_json::to_value(JsStreamManagementTelemetry::ProgressTimedOut).unwrap();
        let failed = serde_json::to_value(JsStreamManagementTelemetry::Failed).unwrap();

        assert_eq!(
            pagehide,
            serde_json::json!({ "kind": "ack-requested", "reason": "pagehide" })
        );
        assert_eq!(
            no_progress,
            serde_json::json!({ "kind": "ack-validated", "progress": false })
        );
        assert_eq!(timeout, serde_json::json!({ "kind": "progress-timed-out" }));
        assert_eq!(failed, serde_json::json!({ "kind": "failed" }));
    }
}

#[cfg(test)]
mod sm_clock_timer_schedule_tests {
    use super::*;

    #[test]
    fn arms_only_for_pending_work_and_rejects_stale_wakeups() {
        let mut schedule = SmClockTimerSchedule::default();

        // An idle client owns no browser interval.
        assert_eq!(schedule.sync(false), SmClockTimerTransition::Noop);

        let first_generation = match schedule.sync(true) {
            SmClockTimerTransition::Arm { generation } => generation,
            transition => panic!("expected first arm, got {transition:?}"),
        };
        schedule
            .install(first_generation, |_| Ok(()))
            .expect("timer installed");
        // Repeated runtime transitions preserve the single interval.
        assert_eq!(schedule.sync(true), SmClockTimerTransition::Noop);
        assert!(schedule.accepts_wakeup(first_generation, true));

        // A no-progress ack clears retries but retains the 30-second
        // progress deadline, so it does not tear down the wakeup.
        assert_eq!(schedule.sync(true), SmClockTimerTransition::Noop);

        // Full ack, failed/reset state, and a terminal clock poll all supply
        // the same false predicate. Only the first transition clears.
        assert_eq!(schedule.sync(false), SmClockTimerTransition::Clear);
        assert_eq!(schedule.sync(false), SmClockTimerTransition::Noop);
        assert!(!schedule.accepts_wakeup(first_generation, false));

        let second_generation = match schedule.sync(true) {
            SmClockTimerTransition::Arm { generation } => generation,
            transition => panic!("expected re-arm, got {transition:?}"),
        };
        schedule
            .install(second_generation, |_| Ok(()))
            .expect("timer reinstalled");
        assert_ne!(second_generation, first_generation);
        assert!(schedule.accepts_wakeup(second_generation, true));

        // A callback queued before clear must not poll or emit a retry after
        // a reset, nor after a fresh timer generation replaces it.
        assert!(!schedule.accepts_wakeup(first_generation, true));
    }

    #[test]
    fn arm_failures_are_typed_transactional_and_leave_fresh_drivers_rearmable() {
        let mut schedule = SmClockTimerSchedule::default();
        let missing_window_generation = match schedule.sync(true) {
            SmClockTimerTransition::Arm { generation } => generation,
            transition => panic!("expected arm, got {transition:?}"),
        };

        assert_eq!(
            schedule.install(missing_window_generation, |_| {
                Err::<(), _>(SmClockTimerArmFailure::WindowUnavailable)
            }),
            Err(SmClockTimerArmFailure::WindowUnavailable)
        );
        assert!(
            !schedule.accepts_wakeup(missing_window_generation, true),
            "a missing window must never publish an active timer generation"
        );
        assert_eq!(schedule.sync(false), SmClockTimerTransition::Noop);

        let interval_failure_generation = match schedule.sync(true) {
            SmClockTimerTransition::Arm { generation } => generation,
            transition => panic!("expected retryable arm, got {transition:?}"),
        };
        assert_eq!(
            schedule.install(interval_failure_generation, |_| {
                Err::<(), _>(SmClockTimerArmFailure::IntervalUnavailable)
            }),
            Err(SmClockTimerArmFailure::IntervalUnavailable)
        );
        assert!(!schedule.accepts_wakeup(interval_failure_generation, true));
        assert!(matches!(
            ClientError::from(SmClockTimerArmFailure::IntervalUnavailable),
            ClientError::StreamManagementClockUnavailable
        ));

        // The failing driver exits and tears down. A fresh driver starts with
        // a fresh schedule and can install exactly one interval normally.
        let mut fresh_driver_schedule = SmClockTimerSchedule::default();
        let fresh_generation = match fresh_driver_schedule.sync(true) {
            SmClockTimerTransition::Arm { generation } => generation,
            transition => panic!("expected fresh arm, got {transition:?}"),
        };
        fresh_driver_schedule
            .install(fresh_generation, |_| Ok(()))
            .expect("fresh driver installs its timer");
        assert!(fresh_driver_schedule.accepts_wakeup(fresh_generation, true));
        assert_eq!(
            fresh_driver_schedule.sync(false),
            SmClockTimerTransition::Clear
        );
    }
}

pub(crate) async fn driver_loop(
    config: ClientConfig,
    ws: WasmWebSocket,
    command_wake_rx: mpsc::Receiver<()>,
    event_tx: mpsc::Sender<DriverEvent>,
    inner: Rc<RefCell<WaddleClientInner>>,
    command_lane: Rc<RefCell<WasmCommandLane>>,
) {
    let mut task = match WasmDriverTask::new(
        config,
        ws,
        command_wake_rx,
        event_tx.clone(),
        inner,
        command_lane,
    ) {
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
        command_wake_rx: mpsc::Receiver<()>,
        event_tx: mpsc::Sender<DriverEvent>,
        inner: Rc<RefCell<WaddleClientInner>>,
        command_lane: Rc<RefCell<WasmCommandLane>>,
    ) -> DriverResult<Self> {
        let (sm_clock_tx, sm_clock_rx) = mpsc::channel(1);
        let core = Rc::new(RefCell::new(WasmDriverCore {
            runtime: XmppRuntime::new(config)?,
            web_socket: ws.web_socket().clone(),
        }));
        inner.borrow_mut().driver_core = Some(core.clone());
        Ok(Self {
            core,
            ws,
            command_lane,
            command_wake_rx,
            event_tx,
            inner,
            pending_iqs: HashMap::new(),
            pending_mam_queries: HashMap::new(),
            pending_inbox_queries: HashMap::new(),
            deferred_commands: VecDeque::new(),
            explicit_disconnect: false,
            sm_clock_timer: None,
            sm_clock_callback: None,
            sm_clock_tx,
            sm_clock_rx,
            sm_clock_schedule: SmClockTimerSchedule::default(),
        })
    }

    async fn run(&mut self) {
        let connect = self
            .core
            .borrow_mut()
            .runtime
            .queue_request(ClientRequest::Connect);
        match connect {
            Ok(events) => {
                self.publish_resume_state_snapshot();
                for event in events {
                    if !self.handle_client_event(event).await {
                        self.finish().await;
                        return;
                    }
                }
                if let Err(err) = self.sync_sm_clock_timer() {
                    self.emit_error(err.to_string()).await;
                    self.finish().await;
                    return;
                }
            }
            Err(err) => {
                self.emit_error(err.to_string()).await;
                self.finish().await;
                return;
            }
        }

        loop {
            let ws_event_fut = self.ws.rx.next().fuse();
            let command_wake_fut = self.command_wake_rx.next().fuse();
            let sm_clock_fut = self.sm_clock_rx.next().fuse();
            pin_mut!(ws_event_fut, command_wake_fut, sm_clock_fut);

            let keep_running = select! {
                ws_event = ws_event_fut => self.handle_wasm_transport_event(ws_event).await,
                wake = command_wake_fut => self.handle_command_wakeup(wake).await,
                tick = sm_clock_fut => match tick {
                    Some(generation) if self.sm_clock_schedule.accepts_wakeup(
                        generation,
                        self.core.borrow().runtime.acknowledgement_clock_pending(),
                    ) => self.poll_stream_management_clock().await,
                    Some(_) => true,
                    None => true,
                },
            };

            if !keep_running {
                break;
            }
        }

        self.finish().await;
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

    async fn poll_stream_management_clock(&mut self) -> bool {
        let events = self
            .core
            .borrow_mut()
            .runtime
            .poll_stream_management_clock(chrono::Utc::now());
        for event in events {
            if !self.handle_client_event(event).await {
                return false;
            }
        }
        match self.sync_sm_clock_timer() {
            Ok(()) => true,
            Err(err) => {
                self.emit_error(err.to_string()).await;
                false
            }
        }
    }

    /// Bring the browser wakeup in line with the runtime's XEP-0198 state.
    ///
    /// This is deliberately called after every runtime transition. In
    /// particular, a valid no-progress `<a/>` clears retry state but retains
    /// the progress deadline, whereas a fully acknowledged tail removes both.
    fn sync_sm_clock_timer(&mut self) -> DriverResult<()> {
        let pending = self.core.borrow().runtime.acknowledgement_clock_pending();
        match self.sm_clock_schedule.sync(pending) {
            SmClockTimerTransition::Noop => Ok(()),
            SmClockTimerTransition::Arm { generation } => self.arm_sm_clock_timer(generation),
            SmClockTimerTransition::Clear => {
                self.clear_sm_clock_timer();
                Ok(())
            }
        }
    }

    fn arm_sm_clock_timer(&mut self, generation: u64) -> DriverResult<()> {
        debug_assert!(self.sm_clock_timer.is_none());
        debug_assert!(self.sm_clock_callback.is_none());

        let sm_clock_tx = self.sm_clock_tx.clone();
        let (timer, callback) = self
            .sm_clock_schedule
            .install(generation, move |generation| {
                let window = web_sys::window().ok_or(SmClockTimerArmFailure::WindowUnavailable)?;
                let callback = Closure::wrap(Box::new(move || {
                    // Coalesce browser timer wakeups; the runtime owns every
                    // deadline and sees the authoritative wall clock on
                    // polling. The generation makes a callback queued before
                    // clear harmless after a reset or fresh arm.
                    let _ = sm_clock_tx.clone().try_send(generation);
                }) as Box<dyn FnMut()>);
                let timer = window
                    .set_interval_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        250,
                    )
                    .map_err(|_| SmClockTimerArmFailure::IntervalUnavailable)?;
                Ok((timer, callback))
            })
            .map_err(ClientError::from)?;
        self.sm_clock_timer = Some(timer);
        self.sm_clock_callback = Some(callback);
        Ok(())
    }

    fn clear_sm_clock_timer(&mut self) {
        if let Some(timer) = self.sm_clock_timer.take() {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(timer);
            }
        }
        self.sm_clock_callback.take();
    }

    async fn handle_command_wakeup(&mut self, wake: Option<()>) -> bool {
        // The wake channel carries only a coalesced hint. The shared FIFO is
        // authoritative, so a full wake channel cannot lose a command and a
        // stale wake merely finds an empty queue.
        if wake.is_none() {
            return true;
        }
        if !self.handle_pagehide_completions().await {
            return false;
        }
        loop {
            let command = { self.command_lane.borrow_mut().pop_ready() };
            let Some(command) = command else {
                break;
            };
            if !self.handle_command(command).await {
                return false;
            }
        }
        true
    }

    async fn handle_pagehide_completions(&mut self) -> bool {
        loop {
            let completion = { self.command_lane.borrow_mut().pop_pagehide_completion() };
            let Some(completion) = completion else {
                return true;
            };
            match completion {
                PagehideCommandCompletion::Stanza { responder, result } => {
                    let _ = responder.send(result);
                }
                PagehideCommandCompletion::Iq {
                    stanza,
                    responder,
                    result,
                } => match result {
                    Ok(()) => match stanza.attr("id") {
                        Some(id) => {
                            self.pending_iqs.insert(id.to_owned(), responder);
                        }
                        None => {
                            let _ = responder.send(Err(ClientError::Disconnected));
                            return false;
                        }
                    },
                    Err(err) => {
                        let _ = responder.send(Err(err));
                        return false;
                    }
                },
                PagehideCommandCompletion::MamQuery {
                    stanza,
                    query_id,
                    responder,
                    result,
                } => match result {
                    Ok(()) => match stanza.attr("id") {
                        Some(id) => {
                            self.pending_mam_queries
                                .insert(id.to_owned(), PendingMamQuery::new(&query_id, responder));
                        }
                        None => {
                            let _ = responder.send(Err(ClientError::Disconnected));
                            return false;
                        }
                    },
                    Err(err) => {
                        let _ = responder.send(Err(err));
                        return false;
                    }
                },
                PagehideCommandCompletion::InboxQuery {
                    stanza,
                    query_id,
                    responder,
                    result,
                } => match result {
                    Ok(()) => match stanza.attr("id") {
                        Some(id) => {
                            self.pending_inbox_queries.insert(
                                id.to_owned(),
                                PendingInboxQuery {
                                    query_id,
                                    entries: Vec::new(),
                                    responder,
                                },
                            );
                        }
                        None => {
                            let _ = responder.send(Err(ClientError::Disconnected));
                            return false;
                        }
                    },
                    Err(err) => {
                        let _ = responder.send(Err(err));
                        return false;
                    }
                },
                PagehideCommandCompletion::Deferred(command) => {
                    self.deferred_commands.push_back(command);
                }
                PagehideCommandCompletion::CancelIq { id, responder } => {
                    self.cancel_iq_command(&id);
                    let _ = responder.send(Ok(()));
                }
                PagehideCommandCompletion::Disconnect { responder, result } => {
                    self.explicit_disconnect = result.is_ok();
                    let keep_running = result.is_ok();
                    let _ = responder.send(result);
                    if !keep_running {
                        return false;
                    }
                }
                PagehideCommandCompletion::StreamManagementAck { responder, result } => {
                    let keep_running = result.is_ok();
                    let _ = responder.send(result);
                    if !keep_running {
                        return false;
                    }
                }
                PagehideCommandCompletion::Event(event) => {
                    if !self.handle_client_event(event).await {
                        return false;
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: WasmCommand) -> bool {
        match cmd {
            WasmCommand::SendStanza { stanza, responder } => {
                if !self.core.borrow().runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::Stanza { stanza, responder });
                    return true;
                }

                self.send_stanza_command(stanza, responder).await
            }
            WasmCommand::SendIq { stanza, responder } => {
                if !self.core.borrow().runtime.can_send_app_stanza() {
                    self.deferred_commands
                        .push_back(DeferredWasmCommand::Iq { stanza, responder });
                    return true;
                }

                self.send_iq_command(stanza, responder).await
            }
            WasmCommand::SendMamQuery {
                stanza,
                query_id,
                responder,
            } => {
                if !self.core.borrow().runtime.can_send_app_stanza() {
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
            WasmCommand::SendInboxQuery {
                stanza,
                query_id,
                responder,
            } => {
                if !self.core.borrow().runtime.can_send_app_stanza()
                    || !self.pending_inbox_queries.is_empty()
                {
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
            WasmCommand::CancelIq { id, responder } => {
                self.cancel_iq_command(&id);
                let _ = responder.send(Ok(()));
                true
            }
            WasmCommand::Disconnect { responder } => {
                self.explicit_disconnect = true;
                self.publish_resume_state_snapshot();
                let result = self
                    .send_transport_message(TransportMessage::Close(StreamClose))
                    .await;
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
            WasmCommand::RequestStreamManagementAck { responder } => {
                let events = self.core.borrow().runtime.request_stream_management_ack();
                let mut result = Ok(());
                for event in events {
                    if !self.handle_client_event(event).await {
                        result = Err(ClientError::Disconnected);
                        break;
                    }
                }
                let timer_result = self.sync_sm_clock_timer();
                if let Err(err) = timer_result {
                    self.emit_error(err.to_string()).await;
                    result = Err(err);
                }
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
        }
    }

    fn cancel_iq_command(&mut self, id: &waddle_xmpp_client::request::StanzaId) {
        cancel_raw_iq_state(&mut self.pending_iqs, &mut self.deferred_commands, id);
    }

    async fn send_stanza_command(
        &mut self,
        stanza: Element,
        responder: oneshot::Sender<DriverResult<()>>,
    ) -> bool {
        let result = self
            .send_transport_message(TransportMessage::Element(stanza.clone()))
            .await;
        let keep_running = result.is_ok();
        if let Err(err) = &result {
            if let Some(stanza_id) = message_delivery_stanza_id(&stanza) {
                self.emit_message_delivery_failed(stanza_id).await;
            }
            self.emit_error(err.to_string()).await;
        }
        let _ = responder.send(result);
        keep_running
    }

    async fn send_iq_command(
        &mut self,
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
    ) -> bool {
        // Enforce the `StanzaId` invariant at tracking time: an IQ whose
        // id would not round-trip through the typed cancellation path
        // (`WasmCommand::CancelIq`) must never become a pending entry,
        // or a reply-less server would leave it uncancellable until the
        // disconnect sweep. Reject before the stanza reaches the wire —
        // an untrackable request IQ is a caller bug, not a send.
        let Some(id) = trackable_iq_id(&stanza) else {
            let _ = responder.send(Err(ClientError::EmptyStanzaId));
            return true;
        };
        match self
            .send_transport_message(TransportMessage::Element(stanza))
            .await
        {
            Ok(()) => {
                self.pending_iqs.insert(id, responder);
                true
            }
            Err(err) => {
                self.emit_error(err.to_string()).await;
                let _ = responder.send(Err(err));
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
            Err(err) => {
                self.emit_error(err.to_string()).await;
                let _ = responder.send(Err(err));
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
            Err(err) => {
                self.emit_error(err.to_string()).await;
                let _ = responder.send(Err(err));
                false
            }
        }
    }

    async fn flush_deferred_commands(&mut self) -> bool {
        if !self.core.borrow().runtime.can_send_app_stanza() {
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
        let result = self.core.borrow_mut().runtime.apply_transport_event(event);
        let events = match result {
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

        if !self.flush_deferred_commands().await {
            return false;
        }

        match self.sync_sm_clock_timer() {
            Ok(()) => true,
            Err(err) => {
                self.emit_error(err.to_string()).await;
                false
            }
        }
    }

    async fn handle_client_event(&mut self, event: ClientEvent) -> bool {
        if let Some(message) = self.dispatch_client_event(event).await {
            if let Err(err) = self.send_transport_message(message).await {
                self.emit_error(err.to_string()).await;
                return false;
            }
        }
        true
    }

    async fn apply_sent_event(&mut self, event: TransportEvent) -> DriverResult<()> {
        let events = self
            .core
            .borrow_mut()
            .runtime
            .apply_transport_event(event)?;
        self.publish_resume_state_snapshot();
        for event in events {
            if let Some(follow_up) = self.dispatch_client_event(event).await {
                // A successful application-stanza write can open the first
                // XEP-0198 unhandled tail, which immediately generates an
                // `<r/>`. Write that typed control frame in-order; do not
                // treat it as an impossible re-entrant transport event.
                // `MessageSent(<r/>)` cannot itself create another request.
                Box::pin(self.send_transport_message(follow_up)).await?;
            }
        }
        self.sync_sm_clock_timer()?;
        Ok(())
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
                StreamManagementEvent::AckReceived { h, progressed },
            )) => {
                self.emit_stream_management_telemetry(JsStreamManagementTelemetry::AckValidated {
                    progress: progressed,
                });
                let _ = self
                    .event_tx
                    .clone()
                    .send(client_driver_event(ClientEvent::Connection(
                        ConnectionEvent::StreamManagement(StreamManagementEvent::AckReceived {
                            h,
                            progressed,
                        }),
                    )))
                    .await;
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequested { reason },
            )) => {
                self.emit_stream_management_telemetry(JsStreamManagementTelemetry::AckRequested {
                    reason: reason.into(),
                });
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRetry { attempt },
            )) => {
                self.emit_stream_management_telemetry(JsStreamManagementTelemetry::AckRetry {
                    attempt,
                });
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::AckRequestTimedOut,
            )) => {
                self.emit_stream_management_telemetry(
                    JsStreamManagementTelemetry::AckRequestTimedOut,
                );
                None
            }
            ClientEvent::Connection(ConnectionEvent::StreamManagement(
                StreamManagementEvent::ReconnectRequired,
            )) => {
                self.emit_stream_management_telemetry(
                    JsStreamManagementTelemetry::ProgressTimedOut,
                );
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
                let iq_key = waddle_xmpp_client::request::StanzaId::new(id.as_str()).ok();
                if let Some(responder) = iq_key.and_then(|key| self.pending_iqs.remove(&key)) {
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
            ClientEvent::IqCancelled { stanza_id } => {
                if let Some(responder) = self.pending_iqs.remove(stanza_id.as_str()) {
                    let _ = responder.send(Err(ClientError::RequestCancelled));
                } else if let Some(pending) = self.pending_mam_queries.remove(stanza_id.as_str()) {
                    let _ = pending.responder.send(Err(ClientError::RequestCancelled));
                } else if let Some(pending) = self.pending_inbox_queries.remove(stanza_id.as_str())
                {
                    let _ = pending.responder.send(Err(ClientError::RequestCancelled));
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

    async fn send_transport_message(&mut self, message: TransportMessage) -> DriverResult<()> {
        let sent_event = TransportEvent::MessageSent(message.clone());
        let frame = waddle_xmpp_client::encode_message(&message)?;
        self.core
            .borrow()
            .web_socket
            .send_with_str(&frame)
            .map_err(|_| ClientError::TransportClosed)?;

        if matches!(message, TransportMessage::Close(_)) {
            let _ = self.core.borrow().web_socket.close();
        }

        self.apply_sent_event(sent_event).await?;

        Ok(())
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

    fn emit_stream_management_telemetry(&self, event: JsStreamManagementTelemetry) {
        emit_stream_management_callback(&self.inner, event);
    }

    fn publish_resume_state_snapshot(&self) {
        publish_resume_state_snapshot(
            &self.inner,
            &self.core.borrow().runtime,
            self.explicit_disconnect,
        );
    }

    async fn finish(&mut self) {
        // Close admission before emitting terminal callbacks so a re-entrant
        // browser handler cannot enqueue work onto a driver that is draining.
        drop(self.command_lane.borrow_mut().close());
        if self
            .inner
            .borrow()
            .command_lane
            .as_ref()
            .is_some_and(|active| Rc::ptr_eq(active, &self.command_lane))
        {
            self.inner.borrow_mut().command_lane = None;
        }
        self.sm_clock_schedule.clear();
        self.clear_sm_clock_timer();
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

/// The pending-IQ tracking key for a request stanza — the typed
/// [`waddle_xmpp_client::request::StanzaId`], so every entry that can
/// become pending is also cancellable through the typed
/// [`WasmCommand::CancelIq`] path. `StanzaId` preserves the raw id
/// bytes (validation only rejects empty-after-trim), so tracking,
/// reply correlation, and cancellation all use the exact wire string.
/// Absent, empty, or whitespace-only ids yield `None`.
fn trackable_iq_id(stanza: &Element) -> Option<waddle_xmpp_client::request::StanzaId> {
    waddle_xmpp_client::request::StanzaId::new(stanza.attr("id")?).ok()
}

fn cancel_raw_iq_state(
    pending_iqs: &mut HashMap<
        waddle_xmpp_client::request::StanzaId,
        oneshot::Sender<DriverResult<Element>>,
    >,
    deferred_commands: &mut VecDeque<DeferredWasmCommand>,
    id: &waddle_xmpp_client::request::StanzaId,
) {
    if let Some(responder) = pending_iqs.remove(id) {
        let _ = responder.send(Err(ClientError::RequestCancelled));
    }

    let mut retained = VecDeque::with_capacity(deferred_commands.len());
    while let Some(command) = deferred_commands.pop_front() {
        if command.raw_iq_id() == Some(id.as_str()) {
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
    use futures::executor::block_on;
    use waddle_xmpp_client::discovery::DISCO_INFO_NS;

    fn iq_with_id(id: Option<&str>) -> Element {
        let mut builder = Element::builder("iq", "jabber:client");
        if let Some(id) = id {
            builder = builder.attr(minidom::rxml::xml_ncname!("id").to_owned(), id);
        }
        builder.build()
    }

    #[test]
    fn trackable_iq_id_holds_the_stanza_id_invariant() {
        // Every id that can become a pending entry must round-trip
        // through the typed CancelIq path (#1606 review): absent,
        // empty, and whitespace-only ids are untrackable — and a
        // trackable id preserves its raw bytes (including surrounding
        // whitespace) so reply correlation and cancellation always use
        // the exact wire string.
        let tracked =
            |raw: Option<&str>| trackable_iq_id(&iq_with_id(raw)).map(|id| id.as_str().to_string());
        assert_eq!(tracked(Some("iq-1")).as_deref(), Some("iq-1"));
        assert_eq!(tracked(Some(" iq-1 ")).as_deref(), Some(" iq-1 "));
        assert_eq!(tracked(None), None);
        assert_eq!(tracked(Some("")), None);
        assert_eq!(tracked(Some("   ")), None);
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
            command_lane: None,
            driver_core: None,
            on_message: None,
            on_presence: None,
            on_connected: None,
            on_session_lifecycle: None,
            on_disconnected: None,
            on_error: None,
            on_message_delivery_acked: None,
            on_message_delivery_failed: None,
            on_mds_displayed: None,
            on_pubsub_event: None,
            on_call: None,
            on_stream_management: None,
            resume_state: None,
        }))
    }

    fn send_bound_stream_management_enable(runtime: &mut XmppRuntime) {
        let server: BareJid = "example.test".parse().expect("server JID");

        runtime
            .queue_request(ClientRequest::Connect)
            .expect("connect request");
        runtime
            .apply_transport_event(TransportEvent::StateChanged(TransportState::Open))
            .expect("transport open");
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                waddle_xmpp_client::transport::StreamOpen::from_server(server.clone()),
            )))
            .expect("server stream open");
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("features", "http://etherx.jabber.org/streams")
                    .append(
                        Element::builder("mechanisms", "urn:ietf:params:xml:ns:xmpp-sasl")
                            .append(
                                Element::builder("mechanism", "urn:ietf:params:xml:ns:xmpp-sasl")
                                    .append("OAUTHBEARER")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )))
            .expect("authentication features");
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("success", "urn:ietf:params:xml:ns:xmpp-sasl").build(),
            )))
            .expect("authentication success");
        runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Open(
                waddle_xmpp_client::transport::StreamOpen::from_server(server),
            )))
            .expect("post-authentication stream open");
        let binding_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("features", "http://etherx.jabber.org/streams")
                    .append(Element::builder("bind", "urn:ietf:params:xml:ns:xmpp-bind").build())
                    .append(
                        Element::builder("sm", waddle_xmpp_client::stream_management::NS_SM)
                            .build(),
                    )
                    .build(),
            )))
            .expect("post-authentication features");
        let binding_id = binding_events
            .iter()
            .find_map(|event| match event {
                ClientEvent::Connection(ConnectionEvent::ResourceBindingRequested(request)) => {
                    Some(request.stanza_id.clone())
                }
                _ => None,
            })
            .expect("resource binding request");
        let ready_events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("iq", NS_CLIENT)
                    .attr(
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        binding_id.as_str(),
                    )
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                    .append(
                        Element::builder("bind", "urn:ietf:params:xml:ns:xmpp-bind")
                            .append(
                                Element::builder("jid", "urn:ietf:params:xml:ns:xmpp-bind")
                                    .append("alice@example.test/web")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )))
            .expect("resource binding result");
        assert_eq!(
            runtime.snapshot().phase,
            waddle_xmpp_client::SessionPhase::Established,
            "SM may only be enabled after resource binding has reached Ready",
        );
        let enable = ready_events
            .iter()
            .find_map(|event| match event {
                ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Element(element),
                )) if element.name() == "enable"
                    && element.ns() == waddle_xmpp_client::stream_management::NS_SM =>
                {
                    Some(element.clone())
                }
                _ => None,
            })
            .expect("bound stream-management enable");
        runtime
            .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
                enable,
            )))
            .expect("bound stream-management enable sent");
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
                waddle_xmpp_client::SmResumeState::new(
                    waddle_xmpp_client::StreamId::new("previous-stream"),
                    4,
                    9,
                )
                .map(|state| state.with_max_resume_seconds(Some(300)))
                .expect("resume state"),
            ),
        };
        let runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");

        publish_resume_state_snapshot(&inner, &runtime, false);

        let borrowed = inner.borrow();
        let snapshot = borrowed.resume_state.as_ref().expect("snapshot");
        assert_eq!(snapshot.previd().as_str(), "previous-stream");
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
        send_bound_stream_management_enable(&mut runtime);
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
    fn publish_resume_state_snapshot_rejects_unbound_enable_and_enabled() {
        let inner = test_inner();
        let stored = inner.borrow().config.clone();
        let mut runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");

        runtime
            .apply_transport_event(TransportEvent::MessageSent(TransportMessage::Element(
                waddle_xmpp_client::stream_management::SmState::build_enable(true),
            )))
            .expect("unbound enable write is observed but cannot negotiate SM");
        let events = runtime
            .apply_transport_event(TransportEvent::MessageReceived(TransportMessage::Element(
                Element::builder("enabled", waddle_xmpp_client::stream_management::NS_SM)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "forged-stream")
                    .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                    .build(),
            )))
            .expect("forged enabled is handled as a protocol violation");

        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(element)
            )) if element.name() == "error"
                && element
                    .get_child("policy-violation", "urn:ietf:params:xml:ns:xmpp-streams")
                    .is_some()
        )));
        publish_resume_state_snapshot(&inner, &runtime, false);
        assert!(
            inner.borrow().resume_state.is_none(),
            "unbound or unauthenticated SM controls must never create a resume snapshot",
        );
    }

    #[test]
    fn publish_resume_state_snapshot_clears_on_explicit_disconnect() {
        let inner = test_inner();
        inner.borrow_mut().resume_state = Some(
            waddle_xmpp_client::SmResumeState::new(
                waddle_xmpp_client::StreamId::new("previous-stream"),
                4,
                9,
            )
            .expect("resume state"),
        );
        let stored = inner.borrow().config.clone();
        let runtime =
            XmppRuntime::new(build_client_config(&stored).expect("config")).expect("runtime");

        publish_resume_state_snapshot(&inner, &runtime, true);

        assert!(inner.borrow().resume_state.is_none());
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
        let sent_id =
            waddle_xmpp_client::request::StanzaId::new("sent-1").expect("valid stanza id");
        let mut pending_iqs = HashMap::from([(sent_id.clone(), responder)]);
        let mut deferred_commands = VecDeque::new();

        cancel_raw_iq_state(&mut pending_iqs, &mut deferred_commands, &sent_id);

        assert!(!pending_iqs.contains_key(&sent_id));
        assert!(matches!(
            block_on(rx).expect("responder should send"),
            Err(ClientError::RequestCancelled)
        ));

        let late = iq("sent-1");
        assert!(pending_iqs.remove(&sent_id).is_none());
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

        cancel_raw_iq_state(
            &mut pending_iqs,
            &mut deferred_commands,
            &waddle_xmpp_client::request::StanzaId::new("deferred-1").expect("valid stanza id"),
        );

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
