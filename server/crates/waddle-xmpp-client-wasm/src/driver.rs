use super::*;

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
        Ok(Self {
            runtime: XmppRuntime::new(config)?,
            ws,
            cmd_rx,
            event_tx,
            inner,
            pending_iqs: HashMap::new(),
            pending_mam_queries: HashMap::new(),
            pending_inbox_queries: HashMap::new(),
            deferred_commands: VecDeque::new(),
            explicit_disconnect: false,
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
            let ws_event_fut = self.ws.rx.next().fuse();
            let cmd_fut = self.cmd_rx.next().fuse();
            pin_mut!(ws_event_fut, cmd_fut);

            let keep_running = select! {
                ws_event = ws_event_fut => self.handle_wasm_transport_event(ws_event).await,
                cmd = cmd_fut => self.handle_command(cmd).await,
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
                self.cancel_iq_command(id.as_str());
                let _ = responder.send(Ok(()));
                true
            }
            Some(WasmCommand::Disconnect { responder }) => {
                self.explicit_disconnect = true;
                self.publish_resume_state_snapshot();
                let result = self
                    .send_transport_message(TransportMessage::Close(StreamClose))
                    .await;
                let keep_running = result.is_ok();
                let _ = responder.send(result);
                keep_running
            }
            None => false,
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

        if !self.flush_deferred_commands().await {
            return false;
        }

        true
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
        let events = self.runtime.apply_transport_event(event)?;
        self.publish_resume_state_snapshot();
        for event in events {
            if self.dispatch_client_event(event).await.is_some() {
                return Err(ClientError::Disconnected);
            }
        }
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

    async fn send_transport_message(&mut self, message: TransportMessage) -> DriverResult<()> {
        let sent_event = TransportEvent::MessageSent(message.clone());
        let frame = waddle_xmpp_client::encode_message(&message)?;
        self.ws
            .send(&frame)
            .map_err(|_| ClientError::TransportClosed)?;

        if matches!(message, TransportMessage::Close(_)) {
            let _ = self.ws.close();
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
    use futures::executor::block_on;
    use waddle_xmpp_client::discovery::DISCO_INFO_NS;

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
