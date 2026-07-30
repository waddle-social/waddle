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
            waddle_xmpp_client::SmResumeState::new(
                waddle_xmpp_client::StreamId::new(previd),
                inbound_h,
                outbound_h,
            )
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
            waddle_xmpp_client::SmResumeState::new(
                waddle_xmpp_client::StreamId::new(previd),
                inbound_h,
                outbound_h,
            )
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
        let entries = resume_entries_from_js(entries).map_err(|err| js_error(err.to_string()))?;
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
                waddle_xmpp_client::StreamId::new(previd),
                inbound_h,
                outbound_h,
                entries,
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
        let entries = resume_entries_from_js(entries).map_err(|err| js_error(err.to_string()))?;
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
                waddle_xmpp_client::StreamId::new(previd),
                inbound_h,
                outbound_h,
                entries,
            )
            .map(|state| state.with_max_resume_seconds(Some(max_resume_seconds)))
            .map_err(|err| js_error(err.to_string()))?,
        );
        Ok(())
    }

    /// Restore a durable message/presence retry tail after a previous stream
    /// explicitly declined XEP-0198. It must never issue `<resume/>`.
    pub fn with_fresh_stream_retry_state_entries(
        &mut self,
        previd: String,
        inbound_h: u32,
        outbound_h: u32,
        entries: JsValue,
    ) -> Result<(), JsValue> {
        let entries = resume_entries_from_js(entries).map_err(|err| js_error(err.to_string()))?;
        self.resume_state = Some(
            waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
                waddle_xmpp_client::StreamId::new(previd),
                inbound_h,
                outbound_h,
                entries,
            )
            .map(waddle_xmpp_client::SmResumeState::into_fresh_stream_retry_state)
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
    pub(crate) resumable: bool,
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
            previd: value.previd().as_str().to_string(),
            resumable: value.is_resumable(),
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum PersistedResumeEntryError {
    #[error("invalid resume stanza XML")]
    Xml,
    #[error("invalid resume stanza timestamp")]
    Timestamp,
    #[error("invalid persisted resume stanza")]
    UncountableStanza,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum PersistedResumeEntriesError {
    #[error("invalid resume entries")]
    InvalidEntries,
    #[error(transparent)]
    Entry(#[from] PersistedResumeEntryError),
}

fn resume_entries_from_js(
    entries: JsValue,
) -> Result<Vec<waddle_xmpp_client::UnhandledOutboundEntry>, PersistedResumeEntriesError> {
    let entries: Vec<JsUnhandledOutboundEntry> = serde_wasm_bindgen::from_value(entries)
        .map_err(|_| PersistedResumeEntriesError::InvalidEntries)?;
    entries
        .into_iter()
        .map(resume_entry_from_persisted_js)
        .collect::<Result<Vec<_>, _>>()
        .map_err(PersistedResumeEntriesError::from)
}

/// Rebuild one persisted entry at the browser persistence boundary.  The
/// shared core constructor validates the countable `jabber:client` root and
/// keeps the original element for later replay.
fn resume_entry_from_persisted_js(
    entry: JsUnhandledOutboundEntry,
) -> Result<waddle_xmpp_client::UnhandledOutboundEntry, PersistedResumeEntryError> {
    let stanza = entry
        .xml
        .parse::<Element>()
        .map_err(|_| PersistedResumeEntryError::Xml)?;
    let sent_at = chrono::DateTime::parse_from_rfc3339(&entry.sent_at)
        .map_err(|_| PersistedResumeEntryError::Timestamp)?
        .with_timezone(&chrono::Utc);
    waddle_xmpp_client::UnhandledOutboundEntry::try_new(stanza, sent_at)
        .map_err(|_| PersistedResumeEntryError::UncountableStanza)
}

#[wasm_bindgen]
pub struct WaddleClient {
    pub(crate) inner: Rc<RefCell<WaddleClientInner>>,
}

pub(crate) struct WaddleClientInner {
    pub(crate) config: StoredConfig,
    /// The sole admission lane for commands submitted by browser callers.
    ///
    /// Unlike an async channel receiver, the lane is also visible to the
    /// synchronous pagehide path. That makes an accepted command's order
    /// observable to both the async driver and the last-chance `<r/>` write.
    pub(crate) command_lane: Option<Rc<RefCell<WasmCommandLane>>>,
    /// The one browser-owner core. The async driver and synchronous pagehide
    /// path must both borrow this exact runtime/socket pair; neither may
    /// manufacture a second transport owner.
    pub(crate) driver_core: Option<Rc<RefCell<WasmDriverCore>>>,
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

/// State that is exclusively owned by the browser's active XMPP connection.
/// The task keeps transport event subscriptions separately, while all typed
/// runtime decisions and physical writes pass through this core.
pub(crate) struct WasmDriverCore {
    pub(crate) runtime: XmppRuntime,
    pub(crate) web_socket: web_sys::WebSocket,
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

/// Browser command admission is bounded exactly like the former `mpsc(64)`,
/// but the ready FIFO is shared with pagehide. Senders beyond capacity wait
/// *outside* the admitted FIFO; a cancelled waiter is never promoted, so a
/// dropped Promise cannot manufacture an outbound stanza.
pub(crate) struct WasmCommandLane {
    ready: VecDeque<WasmCommand>,
    waiting: VecDeque<WaitingWasmCommand>,
    pagehide_completions: VecDeque<PagehideCommandCompletion>,
    wake_tx: mpsc::Sender<()>,
    closed: bool,
}

struct WaitingWasmCommand {
    command: WasmCommand,
    admitted: oneshot::Sender<()>,
}

pub(crate) const WASM_COMMAND_CAPACITY: usize = 64;

impl WasmCommandLane {
    pub(crate) fn new(wake_tx: mpsc::Sender<()>) -> Self {
        Self {
            ready: VecDeque::with_capacity(WASM_COMMAND_CAPACITY),
            waiting: VecDeque::new(),
            pagehide_completions: VecDeque::new(),
            wake_tx,
            closed: false,
        }
    }

    /// Admit immediately when there is no older waiter; otherwise preserve
    /// FIFO by waiting until the driver (or pagehide) frees capacity.
    pub(crate) fn enqueue(
        &mut self,
        command: WasmCommand,
    ) -> Result<Option<oneshot::Receiver<()>>, ()> {
        if self.closed {
            drop(command);
            return Err(());
        }
        if self.waiting.is_empty() && self.ready.len() < WASM_COMMAND_CAPACITY {
            self.ready.push_back(command);
            self.wake();
            return Ok(None);
        }

        let (admitted, receiver) = oneshot::channel();
        self.waiting
            .push_back(WaitingWasmCommand { command, admitted });
        Ok(Some(receiver))
    }

    pub(crate) fn pop_ready(&mut self) -> Option<WasmCommand> {
        let command = self.ready.pop_front();
        if command.is_some() {
            self.promote_waiters();
        }
        command
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn push_pagehide_completion(&mut self, completion: PagehideCommandCompletion) {
        self.pagehide_completions.push_back(completion);
        self.wake();
    }

    pub(crate) fn pop_pagehide_completion(&mut self) -> Option<PagehideCommandCompletion> {
        self.pagehide_completions.pop_front()
    }

    pub(crate) fn close(&mut self) -> Vec<WasmCommand> {
        self.closed = true;
        let mut commands = std::mem::take(&mut self.ready);
        while let Some(waiting) = self.waiting.pop_front() {
            // A waiter has not been admitted, so it must not be executed.
            drop(waiting.command);
        }
        commands.shrink_to_fit();
        commands.into_iter().collect()
    }

    fn promote_waiters(&mut self) {
        while self.ready.len() < WASM_COMMAND_CAPACITY {
            let Some(waiting) = self.waiting.pop_front() else {
                break;
            };
            // Promotion is the linearization point. If the browser-side
            // Promise was cancelled while waiting, retain neither its command
            // nor a phantom queue slot.
            if waiting.admitted.send(()).is_ok() {
                self.ready.push_back(waiting.command);
            }
        }
        if !self.ready.is_empty() {
            self.wake();
        }
    }

    fn wake(&mut self) {
        // A full one-slot wake channel already means the driver will inspect
        // the shared FIFO. Dropping this duplicate wake cannot drop a command.
        let _ = self.wake_tx.try_send(());
    }
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

/// A pagehide write is completed synchronously, but the browser's JS Promise
/// callbacks and query routing stay on the regular async driver turn. Keeping
/// this explicit record prevents a raw write from being replayed by that turn.
pub(crate) enum PagehideCommandCompletion {
    Stanza {
        responder: oneshot::Sender<DriverResult<()>>,
        result: DriverResult<()>,
    },
    Iq {
        stanza: Element,
        responder: oneshot::Sender<DriverResult<Element>>,
        result: DriverResult<()>,
    },
    MamQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<waddle_xmpp_client::MamPage>>,
        result: DriverResult<()>,
    },
    InboxQuery {
        stanza: Element,
        query_id: String,
        responder: oneshot::Sender<DriverResult<InboxPage>>,
        result: DriverResult<()>,
    },
    Deferred(DeferredWasmCommand),
    CancelIq {
        id: waddle_xmpp_client::request::StanzaId,
        responder: oneshot::Sender<DriverResult<()>>,
    },
    Disconnect {
        responder: oneshot::Sender<DriverResult<()>>,
        result: DriverResult<()>,
    },
    StreamManagementAck {
        responder: oneshot::Sender<DriverResult<()>>,
        result: DriverResult<()>,
    },
    Event(ClientEvent),
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
mod command_lane_tests {
    use super::*;
    use futures::executor::block_on;

    fn command(id: &str) -> WasmCommand {
        let (responder, _receiver) = oneshot::channel();
        WasmCommand::CancelIq {
            id: waddle_xmpp_client::request::StanzaId::new(id).expect("test stanza id"),
            responder,
        }
    }

    fn command_id(command: WasmCommand) -> String {
        match command {
            WasmCommand::CancelIq { id, .. } => id.to_string(),
            _ => panic!("test lane contains only cancellation commands"),
        }
    }

    #[test]
    fn command_lane_keeps_all_admitted_commands_fifo_and_promotes_waiters() {
        let (wake_tx, _wake_rx) = mpsc::channel(1);
        let mut lane = WasmCommandLane::new(wake_tx);
        for id in 0..WASM_COMMAND_CAPACITY {
            assert!(matches!(lane.enqueue(command(&id.to_string())), Ok(None)));
        }
        let waiter = match lane.enqueue(command("after-capacity")) {
            Ok(Some(waiter)) => waiter,
            Ok(None) => panic!("the 65th command must wait"),
            Err(_) => panic!("live lane must accept a waiter"),
        };

        assert_eq!(command_id(lane.pop_ready().expect("first command")), "0");
        block_on(waiter).expect("the promoted command was admitted");
        for id in 1..WASM_COMMAND_CAPACITY {
            assert_eq!(
                command_id(lane.pop_ready().expect("queued command")),
                id.to_string()
            );
        }
        assert_eq!(
            command_id(lane.pop_ready().expect("promoted command")),
            "after-capacity"
        );
    }

    #[test]
    fn command_lane_never_promotes_a_cancelled_waiter_or_loses_the_next_waiter() {
        let (wake_tx, _wake_rx) = mpsc::channel(1);
        let mut lane = WasmCommandLane::new(wake_tx);
        for id in 0..WASM_COMMAND_CAPACITY {
            assert!(matches!(lane.enqueue(command(&id.to_string())), Ok(None)));
        }
        let cancelled = match lane.enqueue(command("cancelled")) {
            Ok(Some(waiter)) => waiter,
            Ok(None) => panic!("capacity must make this command wait"),
            Err(_) => panic!("live lane must accept a waiter"),
        };
        let admitted_after_cancelled = match lane.enqueue(command("after-cancelled")) {
            Ok(Some(waiter)) => waiter,
            Ok(None) => panic!("an older waiter must retain FIFO admission"),
            Err(_) => panic!("live lane must accept a waiter"),
        };
        drop(cancelled);
        assert_eq!(command_id(lane.pop_ready().expect("first command")), "0");
        block_on(admitted_after_cancelled).expect("the later live waiter was admitted");
        for id in 1..WASM_COMMAND_CAPACITY {
            assert_eq!(
                command_id(lane.pop_ready().expect("admitted command")),
                id.to_string()
            );
        }
        assert_eq!(
            command_id(lane.pop_ready().expect("later waiter")),
            "after-cancelled",
            "a cancelled waiter must neither execute nor consume the next admitted command"
        );
        assert!(lane.pop_ready().is_none());

        drop(lane.close());
        assert!(lane.enqueue(command("closed")).is_err());
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
    pub(crate) core: Rc<RefCell<WasmDriverCore>>,
    pub(crate) ws: WasmWebSocket,
    pub(crate) command_lane: Rc<RefCell<WasmCommandLane>>,
    pub(crate) command_wake_rx: mpsc::Receiver<()>,
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
        ] {
            assert_eq!(
                resume_entry_from_persisted_js(persisted_entry(xml, "2026-07-27T12:00:00.000Z"))
                    .expect_err("controls and non-client roots must not replay"),
                PersistedResumeEntryError::UncountableStanza,
                "{xml} must not enter the XEP-0198 replay queue",
            );
        }

        assert_eq!(
            resume_entry_from_persisted_js(persisted_entry("<message", "2026-07-27T12:00:00.000Z"))
                .expect_err("malformed XML must not replay"),
            PersistedResumeEntryError::Xml,
        );
        assert_eq!(
            resume_entry_from_persisted_js(persisted_entry(
                "<message xmlns='jabber:client'/>",
                "not-a-timestamp",
            ))
            .expect_err("invalid timestamps must not replay"),
            PersistedResumeEntryError::Timestamp,
        );
    }
}
