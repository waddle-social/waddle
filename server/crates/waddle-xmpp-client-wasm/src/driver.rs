use super::*;

pub(crate) async fn driver_loop(
    config: ClientConfig,
    ws: WasmWebSocket,
    cmd_rx: mpsc::Receiver<WasmCommand>,
    event_tx: mpsc::Sender<DriverEvent>,
) {
    let mut task = match WasmDriverTask::new(config, ws, cmd_rx, event_tx.clone()) {
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
    ) -> DriverResult<Self> {
        Ok(Self {
            runtime: XmppRuntime::new(config)?,
            ws,
            cmd_rx,
            event_tx,
            pending_iqs: HashMap::new(),
            pending_mam_queries: HashMap::new(),
        })
    }

    async fn run(&mut self) {
        match self.runtime.queue_request(ClientRequest::Connect) {
            Ok(events) => {
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
            Some(WasmCommand::SendIq { stanza, responder }) => {
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
            Some(WasmCommand::SendMamQuery {
                stanza,
                query_id,
                responder,
            }) => {
                let id = stanza.attr("id").map(|value| value.to_string());
                match self
                    .send_transport_message(TransportMessage::Element(stanza))
                    .await
                {
                    Ok(()) => match id {
                        Some(id) => {
                            self.pending_mam_queries.insert(
                                id,
                                PendingMamQuery {
                                    query_id,
                                    messages: Vec::new(),
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
            Some(WasmCommand::Disconnect { responder }) => {
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

    async fn apply_transport_event(&mut self, event: TransportEvent) -> bool {
        let events = match self.runtime.apply_transport_event(event) {
            Ok(events) => events,
            Err(err) => {
                self.emit_error(err.to_string()).await;
                return false;
            }
        };

        for event in events {
            if !self.handle_client_event(event).await {
                return false;
            }
        }

        self.publish_resume_state().await;
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
        for event in events {
            if self.dispatch_client_event(event).await.is_some() {
                return Err(ClientError::Disconnected);
            }
        }
        self.publish_resume_state().await;
        Ok(())
    }

    async fn publish_resume_state(&mut self) {
        let _ = self
            .event_tx
            .clone()
            .send(DriverEvent::ResumeState(self.runtime.resume_state()))
            .await;
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
                    let result = if element.attr("type") == Some("result") {
                        let (rsm, is_complete) = mam::parse_fin_from_iq_result(&element);
                        Ok(waddle_xmpp_client::MamPage {
                            messages: pending.messages,
                            rsm,
                            query_id: pending.query_id,
                            is_complete,
                        })
                    } else {
                        Err(ClientError::StanzaError(parse_stanza_error(&element)))
                    };
                    let _ = pending.responder.send(result);
                }
                None
            }
            ClientEvent::MamResult(archived) => {
                if let Some(query_id) = archived.query_id.as_deref() {
                    if let Some((_, pending)) = self
                        .pending_mam_queries
                        .iter_mut()
                        .find(|(_, pending)| pending.query_id == query_id)
                    {
                        pending.messages.push(archived);
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

    async fn finish(&mut self) {
        for (_, responder) in self.pending_iqs.drain() {
            let _ = responder.send(Err(ClientError::Disconnected));
        }
        for (_, pending) in self.pending_mam_queries.drain() {
            let _ = pending.responder.send(Err(ClientError::Disconnected));
        }

        let _ = self.event_tx.clone().send(DriverEvent::Disconnected).await;
    }
}
