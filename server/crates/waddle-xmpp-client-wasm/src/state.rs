use super::*;

pub(crate) type DriverResult<T> = Result<T, ClientError>;

pub(crate) trait WasmDriverWire {
    fn events(&mut self) -> &mut mpsc::Receiver<WasmTransportEvent>;
    fn send_frame(&mut self, frame: &str) -> DriverResult<()>;
    /// Begin the WebSocket closing handshake. The typed RFC 7395 XML close is
    /// sent separately through [`Self::send_frame`].
    fn close_websocket(&mut self) -> DriverResult<()>;
}

pub(crate) trait DriverTimerBackend {
    fn wait(&self, delay_ms: Option<u64>) -> futures::future::LocalBoxFuture<'static, ()>;
}

#[wasm_bindgen]
pub struct WaddleResumeState {
    pub(crate) inner: waddle_xmpp_client::SmResumeState,
}

#[wasm_bindgen]
pub struct WaddleConfig {
    pub(crate) server_url: String,
    pub(crate) jid: String,
    pub(crate) access_token: String,
    pub(crate) resource: String,
    pub(crate) resume_state: Option<waddle_xmpp_client::SmResumeState>,
}

#[wasm_bindgen]
impl WaddleConfig {
    #[wasm_bindgen(constructor)]
    pub fn new(
        server_url: String,
        jid: String,
        access_token: String,
        resource: String,
    ) -> WaddleConfig {
        WaddleConfig {
            server_url,
            jid,
            access_token,
            resource,
            resume_state: None,
        }
    }

    pub fn with_resume_state(
        &mut self,
        previd: String,
        inbound_h: u32,
        outbound_h: u32,
    ) -> Result<(), JsValue> {
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::new(previd, inbound_h, outbound_h)
                .map_err(|err| js_error(err.to_string()))?,
        );
        Ok(())
    }

    pub fn with_resume_state_with_max(
        &mut self,
        previd: String,
        inbound_h: u32,
        outbound_h: u32,
        max_resume_seconds: u32,
    ) -> Result<(), JsValue> {
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::new(previd, inbound_h, outbound_h)
                .map(|state| state.with_max_resume_seconds(Some(max_resume_seconds)))
                .map_err(|err| js_error(err.to_string()))?,
        );
        Ok(())
    }

    pub fn with_resume_state_entries(
        &mut self,
        previd: String,
        inbound_h: u32,
        outbound_h: u32,
        entries: JsValue,
    ) -> Result<(), JsValue> {
        let entries = parse_resume_entries(entries)?;
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
                previd, inbound_h, outbound_h, entries,
            )
            .map_err(|err| js_error(err.to_string()))?,
        );
        Ok(())
    }

    pub fn with_resume_state_entries_with_max(
        &mut self,
        previd: String,
        inbound_h: u32,
        outbound_h: u32,
        entries: JsValue,
        max_resume_seconds: u32,
    ) -> Result<(), JsValue> {
        let entries = parse_resume_entries(entries)?;
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
                previd, inbound_h, outbound_h, entries,
            )
            .map(|state| state.with_max_resume_seconds(Some(max_resume_seconds)))
            .map_err(|err| js_error(err.to_string()))?,
        );
        Ok(())
    }

    pub fn with_resume_state_handle(&mut self, state: &WaddleResumeState) {
        self.resume_state = Some(state.inner.clone());
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JsResumeEntryInput {
    stanza: waddle_xmpp_client::ResumeStanzaSnapshot,
    sent_at_epoch_ms: f64,
}

fn parse_resume_entries(
    entries: JsValue,
) -> Result<Vec<waddle_xmpp_client::SmResumeEntry>, JsValue> {
    let entries: Vec<JsResumeEntryInput> = serde_wasm_bindgen::from_value(entries)
        .map_err(|err| js_error(format!("invalid resume entries: {err}")))?;
    entries
        .into_iter()
        .map(|entry| resume_entry_from_input(entry).map_err(js_error))
        .collect()
}

fn resume_entry_from_input(
    entry: JsResumeEntryInput,
) -> Result<waddle_xmpp_client::SmResumeEntry, String> {
    if !entry.sent_at_epoch_ms.is_finite()
        || entry.sent_at_epoch_ms.fract() != 0.0
        || entry.sent_at_epoch_ms.abs() > 9_007_199_254_740_991.0
    {
        return Err("invalid resume sentAtEpochMs".to_owned());
    }
    let sent_at_epoch_ms = entry.sent_at_epoch_ms as i64;
    let sent_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(sent_at_epoch_ms)
        .ok_or_else(|| "resume sentAtEpochMs is outside the supported range".to_owned())?;
    let stanza = waddle_xmpp_client::ResumeStanza::from_snapshot(entry.stanza)
        .map_err(|err| format!("invalid resume stanza: {err}"))?;
    Ok(waddle_xmpp_client::SmResumeEntry::new(stanza, sent_at))
}

#[derive(Clone)]
pub(crate) struct StoredConfig {
    pub(crate) server_url: String,
    pub(crate) jid: String,
    pub(crate) access_token: String,
    pub(crate) resource: String,
    pub(crate) resume_state: Option<waddle_xmpp_client::SmResumeState>,
}

impl From<&WaddleConfig> for StoredConfig {
    fn from(value: &WaddleConfig) -> Self {
        Self {
            server_url: value.server_url.clone(),
            jid: value.jid.clone(),
            access_token: value.access_token.clone(),
            resource: value.resource.clone(),
            resume_state: value.resume_state.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsResumeState {
    pub(crate) previd: String,
    pub(crate) inbound_h: u32,
    pub(crate) outbound_h: u32,
    pub(crate) has_unacked_outbound: bool,
    pub(crate) unhandled_outbound_entries: Vec<JsResumeEntry>,
    pub(crate) max_resume_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsResumeEntry {
    pub(crate) stanza: waddle_xmpp_client::ResumeStanzaSnapshot,
    pub(crate) sent_at_epoch_ms: f64,
}

impl From<waddle_xmpp_client::SmResumeState> for JsResumeState {
    fn from(value: waddle_xmpp_client::SmResumeState) -> Self {
        let unhandled_outbound_entries = value
            .unhandled_outbound_entries()
            .map(|entry| JsResumeEntry {
                stanza: entry.stanza().snapshot(),
                sent_at_epoch_ms: entry.sent_at().timestamp_millis() as f64,
            })
            .collect();
        Self {
            previd: value.previd().to_string(),
            inbound_h: value.inbound_h(),
            outbound_h: value.outbound_h(),
            has_unacked_outbound: value.has_unhandled_outbound_stanzas(),
            unhandled_outbound_entries,
            max_resume_seconds: value.max_resume_seconds(),
        }
    }
}

#[wasm_bindgen]
pub struct WaddleClient {
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
    /// The JavaScript wrapper is the sole strong owner of command input.
    /// Driver/event tasks retain only the weak reference in `inner`, so the
    /// final wrapper drop closes the channel and self-fences the old socket.
    pub(crate) _command_owner: Rc<RefCell<Option<mpsc::Sender<WasmCommand>>>>,
}

pub(crate) struct WaddleClientInner {
    pub(crate) config: StoredConfig,
    pub(crate) command_owner: Weak<RefCell<Option<mpsc::Sender<WasmCommand>>>>,
    pub(crate) disposed: bool,
    pub(crate) on_message: Option<Function>,
    pub(crate) on_presence: Option<Function>,
    pub(crate) on_connected: Option<Function>,
    pub(crate) on_session_lifecycle: Option<Function>,
    pub(crate) on_stream_management: Option<Function>,
    pub(crate) on_disconnected: Option<Function>,
    pub(crate) on_error: Option<Function>,
    pub(crate) on_message_delivery_acked: Option<Function>,
    pub(crate) on_message_delivery_failed: Option<Function>,
    /// XEP-0490 §3 displayed-event callback. Receives one
    /// `WaddleMdsDisplayedEntry`-shaped value per item carried in the
    /// inbound PEP event, so the chat layer can apply each one
    /// independently without re-parsing the message.
    pub(crate) on_mds_displayed: Option<Function>,
    pub(crate) on_pubsub_event: Option<Function>,
    pub(crate) on_call: Option<Function>,
    pub(crate) resume_state: Option<waddle_xmpp_client::SmResumeState>,
}

impl WaddleClientInner {
    pub(crate) fn retire(&mut self) {
        self.disposed = true;
        if let Some(command_owner) = self.command_owner.upgrade() {
            command_owner.borrow_mut().take();
        }
        self.on_message.take();
        self.on_presence.take();
        self.on_connected.take();
        self.on_session_lifecycle.take();
        self.on_stream_management.take();
        self.on_disconnected.take();
        self.on_error.take();
        self.on_message_delivery_acked.take();
        self.on_message_delivery_failed.take();
        self.on_mds_displayed.take();
        self.on_pubsub_event.take();
        self.on_call.take();
    }
}

pub(crate) enum WasmCommand {
    SendStanza {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    SendIq {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
    },
    SendMamQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
    },
    SendInboxQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<InboxPage>>,
    },
    CancelIq {
        id: String,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    RequestStreamManagementAck {
        responder: oneshot::Sender<DriverResult<()>>,
    },
    Disconnect {
        responder: oneshot::Sender<DriverResult<()>>,
    },
}

pub(crate) enum DeferredWasmCommand {
    Stanza {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    Iq {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
    },
    MamQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
    },
    InboxQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<InboxPage>>,
    },
}

impl DeferredWasmCommand {
    pub(crate) fn raw_iq_id(&self) -> Option<&str> {
        match self {
            Self::Iq { stanza, .. } => stanza.attr("id"),
            Self::Stanza { .. } | Self::MamQuery { .. } | Self::InboxQuery { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resume_entry_json(timestamp_ms: i64) -> serde_json::Value {
        json!({
            "stanza": {
                "stanzaKind": "message",
                "tokens": [
                    {
                        "kind": "start",
                        "name": { "namespace": "jabber:client", "localName": "message" },
                        "attributes": [
                            {
                                "name": { "namespace": "", "localName": "id" },
                                "value": "wasm-roundtrip"
                            },
                            {
                                "name": {
                                    "namespace": "http://www.w3.org/XML/1998/namespace",
                                    "localName": "lang"
                                },
                                "value": "en"
                            }
                        ]
                    },
                    { "kind": "text", "value": "prefix" },
                    {
                        "kind": "start",
                        "name": { "namespace": "urn:waddle:test:resume", "localName": "rich" },
                        "attributes": []
                    },
                    { "kind": "text", "value": "nested" },
                    { "kind": "end" },
                    { "kind": "text", "value": "tail" },
                    { "kind": "end" }
                ]
            },
            "sentAtEpochMs": timestamp_ms
        })
    }

    #[test]
    fn wasm_resume_entry_round_trips_structured_stanza_and_millisecond_time() {
        let timestamp_ms = 1_748_779_140_123_i64;
        let input: JsResumeEntryInput =
            serde_json::from_value(resume_entry_json(timestamp_ms)).expect("typed input");
        let entry = resume_entry_from_input(input).expect("entry converts");
        let state = waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
            "wasm-prev",
            2,
            3,
            [entry],
        )
        .expect("resume state");
        let output = serde_json::to_value(JsResumeState::from(state)).expect("output serializes");

        assert_eq!(
            output["unhandledOutboundEntries"][0]["sentAtEpochMs"].as_f64(),
            Some(timestamp_ms as f64)
        );
        assert!(output["unhandledOutboundEntries"][0]["stanza"].is_object());
        assert!(output["unhandledOutboundEntries"][0]["stanza"]["tokens"].is_array());
        assert_eq!(
            output["unhandledOutboundEntries"][0]["stanza"]["tokens"][1]["value"],
            "prefix"
        );
    }

    #[test]
    fn wasm_resume_entry_rejects_raw_xml_unknown_fields_and_fractional_time() {
        let raw_xml = json!({
            "stanza": "<message xmlns='jabber:client'/>",
            "sentAtEpochMs": 1_748_779_140_123_i64
        });
        assert!(serde_json::from_value::<JsResumeEntryInput>(raw_xml).is_err());

        let mut unknown = resume_entry_json(1_748_779_140_123);
        unknown
            .as_object_mut()
            .expect("object")
            .insert("stanzaXml".to_owned(), json!("<message/>"));
        assert!(serde_json::from_value::<JsResumeEntryInput>(unknown).is_err());

        let fractional = JsResumeEntryInput {
            stanza: serde_json::from_value(resume_entry_json(0)["stanza"].clone())
                .expect("snapshot"),
            sent_at_epoch_ms: 1.5,
        };
        assert_eq!(
            resume_entry_from_input(fractional).expect_err("fractional time rejected"),
            "invalid resume sentAtEpochMs"
        );
    }

    #[test]
    fn wasm_resume_entry_rejects_unbalanced_oversized_and_too_deep_tokens() {
        let base: JsResumeEntryInput =
            serde_json::from_value(resume_entry_json(0)).expect("typed input");

        let mut unbalanced = base.stanza.clone();
        unbalanced.tokens.pop();
        assert!(resume_entry_from_input(JsResumeEntryInput {
            stanza: unbalanced,
            sent_at_epoch_ms: 0.0,
        })
        .is_err());

        let mut oversized = base.stanza.clone();
        oversized.tokens = vec![waddle_xmpp_client::ResumeXmlToken::End; 16_385];
        assert!(resume_entry_from_input(JsResumeEntryInput {
            stanza: oversized,
            sent_at_epoch_ms: 0.0,
        })
        .expect_err("token limit rejected")
        .contains("token limit"));

        let root_name = waddle_xmpp_client::ResumeXmlName {
            namespace: waddle_xmpp_client::ResumeXmlNamespace::new("jabber:client"),
            local_name: waddle_xmpp_client::ResumeXmlLocalName::new("message").expect("root name"),
        };
        let nested_name = waddle_xmpp_client::ResumeXmlName {
            namespace: waddle_xmpp_client::ResumeXmlNamespace::new("urn:waddle:test:resume"),
            local_name: waddle_xmpp_client::ResumeXmlLocalName::new("nested").expect("nested name"),
        };
        let mut tokens = vec![waddle_xmpp_client::ResumeXmlToken::Start {
            name: root_name,
            attributes: Vec::new(),
        }];
        tokens.extend((0..64).map(|_| waddle_xmpp_client::ResumeXmlToken::Start {
            name: nested_name.clone(),
            attributes: Vec::new(),
        }));
        tokens.extend((0..65).map(|_| waddle_xmpp_client::ResumeXmlToken::End));
        assert!(resume_entry_from_input(JsResumeEntryInput {
            stanza: waddle_xmpp_client::ResumeStanzaSnapshot {
                stanza_kind: waddle_xmpp_client::ResumeStanzaKind::Message,
                tokens,
            },
            sent_at_epoch_ms: 0.0,
        })
        .expect_err("depth limit rejected")
        .contains("depth limit"));
    }
}

/// Result of one streamed XEP-0430 inbox query.
#[derive(Debug, Clone)]
pub(crate) struct InboxPage {
    pub entries: Vec<waddle_xmpp_client::inbox::InboxStreamEntry>,
    pub fin: waddle_xmpp_client::inbox::InboxFin,
}

pub(crate) enum DriverEvent {
    Client(Box<ClientEvent>),
    ResumeState(Option<waddle_xmpp_client::SmResumeState>),
    Error(String),
    Disconnected,
}

pub(crate) fn client_driver_event(event: ClientEvent) -> DriverEvent {
    DriverEvent::Client(Box::new(event))
}

pub(crate) struct PendingMamQuery {
    pub(crate) collector: MamResultCollector,
    pub(crate) responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
}

impl PendingMamQuery {
    pub(crate) fn new(
        query_id: &str,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
    ) -> Self {
        Self {
            collector: MamResultCollector::new(query_id),
            responder,
        }
    }

    pub(crate) fn query_id(&self) -> &str {
        self.collector.query_id()
    }

    pub(crate) fn collect(&mut self, archived: ArchivedMessage) {
        self.collector.collect(archived);
    }
}

pub(crate) struct PendingInboxQuery {
    pub(crate) query_id: String,
    pub(crate) entries: Vec<waddle_xmpp_client::inbox::InboxStreamEntry>,
    pub(crate) responder: oneshot::Sender<DriverResult<InboxPage>>,
}

pub(crate) struct WasmDriverTask {
    pub(crate) runtime: XmppRuntime,
    pub(crate) ws: Box<dyn WasmDriverWire>,
    pub(crate) timer: Rc<dyn DriverTimerBackend>,
    pub(crate) cmd_rx: mpsc::Receiver<WasmCommand>,
    pub(crate) event_tx: mpsc::Sender<DriverEvent>,
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
    pub(crate) pending_iqs: HashMap<String, oneshot::Sender<DriverResult<Element>>>,
    pub(crate) pending_mam_queries: HashMap<String, PendingMamQuery>,
    pub(crate) pending_inbox_queries: HashMap<String, PendingInboxQuery>,
    pub(crate) deferred_commands: VecDeque<DeferredWasmCommand>,
    pub(crate) explicit_disconnect: bool,
    pub(crate) websocket_close_started: bool,
    pub(crate) commands_closed: bool,
}
