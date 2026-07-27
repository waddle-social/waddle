use super::*;

pub(crate) type DriverResult<T> = Result<T, ClientError>;

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
        let entries = resume_entries_from_js(entries)?;
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
        let entries = resume_entries_from_js(entries)?;
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
    pub(crate) unhandled_outbound_entries: Vec<JsUnhandledOutboundEntry>,
    pub(crate) max_resume_seconds: Option<u32>,
}

impl From<waddle_xmpp_client::SmResumeState> for JsResumeState {
    fn from(value: waddle_xmpp_client::SmResumeState) -> Self {
        let unhandled_outbound_entries = value
            .unhandled_outbound_entries()
            .filter_map(|entry| {
                element_to_xml_string(entry.stanza_for_persistence())
                    .ok()
                    .map(|xml| JsUnhandledOutboundEntry {
                        xml,
                        sent_at: entry
                            .sent_at()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    })
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsUnhandledOutboundEntry {
    xml: String,
    sent_at: String,
}

fn resume_entries_from_js(
    entries: JsValue,
) -> Result<Vec<waddle_xmpp_client::UnhandledOutboundEntry>, JsValue> {
    let entries: Vec<JsUnhandledOutboundEntry> = serde_wasm_bindgen::from_value(entries)
        .map_err(|err| js_error(format!("invalid resume entries: {err}")))?;
    entries
        .into_iter()
        .map(resume_entry_from_persisted_js)
        .collect::<Result<Vec<_>, _>>()
        .map_err(js_error)
}

/// Rebuild one persisted entry at the browser persistence boundary.  The
/// shared core constructor validates the countable `jabber:client` root and
/// keeps the original element for later replay.
fn resume_entry_from_persisted_js(
    entry: JsUnhandledOutboundEntry,
) -> Result<waddle_xmpp_client::UnhandledOutboundEntry, String> {
    let stanza = entry
        .xml
        .parse::<Element>()
        .map_err(|err| format!("invalid resume stanza XML: {err}"))?;
    let sent_at = chrono::DateTime::parse_from_rfc3339(&entry.sent_at)
        .map_err(|err| format!("invalid resume stanza timestamp: {err}"))?
        .with_timezone(&chrono::Utc);
    waddle_xmpp_client::UnhandledOutboundEntry::try_new(stanza, sent_at)
        .map_err(|err| format!("invalid persisted resume stanza: {err}"))
}

#[wasm_bindgen]
pub struct WaddleClient {
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
}

pub(crate) struct WaddleClientInner {
    pub(crate) config: StoredConfig,
    pub(crate) cmd_tx: Option<mpsc::Sender<WasmCommand>>,
    pub(crate) on_message: Option<Function>,
    pub(crate) on_presence: Option<Function>,
    pub(crate) on_connected: Option<Function>,
    pub(crate) on_session_lifecycle: Option<Function>,
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
    pub(crate) on_stream_management: Option<Function>,
    pub(crate) resume_state: Option<waddle_xmpp_client::SmResumeState>,
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
        id: waddle_xmpp_client::request::StanzaId,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    Disconnect {
        responder: oneshot::Sender<DriverResult<()>>,
    },
    RequestStreamManagementAck {
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

/// Result of one streamed XEP-0430 inbox query — the shared type
/// from the core crate, re-exported so driver/command plumbing keeps
/// its `crate::state::InboxPage` paths.
pub(crate) use waddle_xmpp_client::inbox::InboxPage;

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
    pub(crate) ws: WasmWebSocket,
    pub(crate) cmd_rx: mpsc::Receiver<WasmCommand>,
    pub(crate) event_tx: mpsc::Sender<DriverEvent>,
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
    pub(crate) pending_iqs:
        HashMap<waddle_xmpp_client::request::StanzaId, oneshot::Sender<DriverResult<Element>>>,
    pub(crate) pending_mam_queries: HashMap<String, PendingMamQuery>,
    pub(crate) pending_inbox_queries: HashMap<String, PendingInboxQuery>,
    pub(crate) deferred_commands: VecDeque<DeferredWasmCommand>,
    pub(crate) explicit_disconnect: bool,
    /// Browser-owned wakeup only: XEP-0198 timing and outcomes remain in the
    /// Rust runtime. These exist only while an acknowledgement deadline is
    /// pending, rather than for the whole lifetime of an idle client.
    pub(crate) sm_clock_timer: Option<i32>,
    pub(crate) sm_clock_callback: Option<Closure<dyn FnMut()>>,
    pub(crate) sm_clock_tx: mpsc::Sender<u64>,
    pub(crate) sm_clock_rx: mpsc::Receiver<u64>,
    pub(crate) sm_clock_schedule: super::driver::SmClockTimerSchedule,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persisted_entry(xml: &str, sent_at: &str) -> JsUnhandledOutboundEntry {
        JsUnhandledOutboundEntry {
            xml: xml.to_owned(),
            sent_at: sent_at.to_owned(),
        }
    }

    #[test]
    fn persisted_resume_entries_accept_countable_client_stanzas_in_order() {
        let sent_at = "2026-07-27T12:00:00.000Z";
        let entries = [
            persisted_entry("<message xmlns='jabber:client' id='m-1'><body>one</body></message>", sent_at),
            persisted_entry("<presence xmlns='jabber:client'><show>away</show></presence>", "2026-07-27T12:00:01.000Z"),
            persisted_entry("<iq xmlns='jabber:client' id='iq-1' type='get'><query xmlns='jabber:iq:version'/></iq>", "2026-07-27T12:00:02.000Z"),
        ];

        let restored = entries
            .into_iter()
            .map(resume_entry_from_persisted_js)
            .collect::<Result<Vec<_>, _>>()
            .expect("countable client stanzas restore");

        assert_eq!(
            restored
                .iter()
                .map(|entry| entry.stanza_for_persistence().name())
                .collect::<Vec<_>>(),
            vec!["message", "presence", "iq"],
        );
        assert_eq!(
            restored
                .iter()
                .map(|entry| entry
                    .sent_at()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .collect::<Vec<_>>(),
            vec![
                "2026-07-27T12:00:00.000Z",
                "2026-07-27T12:00:01.000Z",
                "2026-07-27T12:00:02.000Z",
            ],
        );
    }

    #[test]
    fn persisted_resume_entries_reject_controls_non_client_roots_and_malformed_xml() {
        for xml in [
            "<r xmlns='urn:xmpp:sm:3'/>",
            "<a xmlns='urn:xmpp:sm:3' h='1'/>",
            "<enable xmlns='urn:xmpp:sm:3'/>",
            "<resumed xmlns='urn:xmpp:sm:3' h='1' previd='old'/>",
            "<foo xmlns='jabber:client'/>",
            "<message xmlns='urn:example:other'/>",
            "<message",
        ] {
            assert!(
                resume_entry_from_persisted_js(persisted_entry(xml, "2026-07-27T12:00:00.000Z"))
                    .is_err(),
                "{xml} must not enter the XEP-0198 replay queue",
            );
        }
    }
}
