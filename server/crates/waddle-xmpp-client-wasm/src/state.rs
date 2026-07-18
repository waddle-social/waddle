use super::*;

#[wasm_bindgen(typescript_custom_section)]
const RESUME_STATE_TYPESCRIPT: &str = r#"
export type WaddleResumeStanzaKind = "message" | "presence" | "iq";
export interface WaddleResumeXmlName {
    readonly namespace: string;
    readonly localName: string;
}
export interface WaddleResumeXmlAttribute {
    readonly name: WaddleResumeXmlName;
    readonly value: string;
}
export type WaddleResumeXmlToken =
    | {
        readonly kind: "start";
        readonly name: WaddleResumeXmlName;
        readonly attributes: WaddleResumeXmlAttribute[];
    }
    | { readonly kind: "text"; readonly value: string }
    | { readonly kind: "end" };
export interface WaddleResumeStanzaSnapshot {
    readonly stanzaKind: WaddleResumeStanzaKind;
    readonly tokens: WaddleResumeXmlToken[];
}
export interface WaddleResumeEntrySnapshot {
    readonly stanza: WaddleResumeStanzaSnapshot;
    readonly sentAtEpochMs: number;
}
export interface WaddleResumeStateSnapshot {
    readonly previd: string;
    readonly inboundH: number;
    readonly outboundH: number;
    readonly unhandledOutboundEntries: WaddleResumeEntrySnapshot[];
    readonly maxResumeSeconds?: number;
}
"#;

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

    pub fn with_resume_state(&mut self, state: JsValue) -> Result<(), JsValue> {
        let state = serde_wasm_bindgen::from_value::<JsResumeStateInput>(state)
            .map_err(ResumeWasmBoundaryError::from)
            .and_then(resume_state_from_input)
            .map_err(|error| js_error(error.to_string()))?;
        self.resume_state = Some(state);
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JsResumeStateInput {
    previd: String,
    inbound_h: u32,
    outbound_h: u32,
    unhandled_outbound_entries: Vec<JsResumeEntryInput>,
    max_resume_seconds: Option<u32>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JsResumeEntryInput {
    stanza: waddle_xmpp_client::ResumeStanzaSnapshot,
    sent_at_epoch_ms: f64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResumeWasmBoundaryError {
    #[error("invalid resume entries: {0}")]
    Deserialize(#[from] serde_wasm_bindgen::Error),
    #[error("resume sentAtEpochMs must be a finite safe integer")]
    InvalidTimestamp,
    #[error("resume sentAtEpochMs is outside the supported range")]
    TimestampOutOfRange,
    #[error("resume maxResumeSeconds must be greater than zero")]
    InvalidMaxResumeSeconds,
    #[error(transparent)]
    ResumeStanza(#[from] waddle_xmpp_client::ResumeStanzaError),
    #[error(transparent)]
    Client(#[from] waddle_xmpp_client::ClientError),
}

fn resume_state_from_input(
    state: JsResumeStateInput,
) -> Result<waddle_xmpp_client::SmResumeState, ResumeWasmBoundaryError> {
    if state.max_resume_seconds == Some(0) {
        return Err(ResumeWasmBoundaryError::InvalidMaxResumeSeconds);
    }
    let mut sent_at = Vec::with_capacity(state.unhandled_outbound_entries.len());
    let snapshots = state
        .unhandled_outbound_entries
        .into_iter()
        .map(|entry| {
            sent_at.push(resume_timestamp(entry.sent_at_epoch_ms)?);
            Ok(entry.stanza)
        })
        .collect::<Result<Vec<_>, ResumeWasmBoundaryError>>()?;
    let stanzas = waddle_xmpp_client::ResumeStanza::from_snapshot_batch(snapshots)?;
    let entries = stanzas
        .into_iter()
        .zip(sent_at)
        .map(|(stanza, sent_at)| waddle_xmpp_client::SmResumeEntry::new(stanza, sent_at));
    Ok(
        waddle_xmpp_client::SmResumeState::from_unhandled_outbound_entries(
            state.previd,
            state.inbound_h,
            state.outbound_h,
            entries,
        )?
        .with_max_resume_seconds(state.max_resume_seconds),
    )
}

fn resume_timestamp(
    sent_at_epoch_ms: f64,
) -> Result<chrono::DateTime<chrono::Utc>, ResumeWasmBoundaryError> {
    const JS_DATE_LIMIT_MS: f64 = 8_640_000_000_000_000.0;
    if !sent_at_epoch_ms.is_finite()
        || sent_at_epoch_ms.fract() != 0.0
        || sent_at_epoch_ms < 0.0
        || sent_at_epoch_ms > JS_DATE_LIMIT_MS
    {
        return Err(ResumeWasmBoundaryError::InvalidTimestamp);
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(sent_at_epoch_ms as i64)
        .ok_or(ResumeWasmBoundaryError::TimestampOutOfRange)
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
    pub(crate) unhandled_outbound_entries: Vec<JsResumeEntry>,
    pub(crate) max_resume_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsResumeEntry {
    pub(crate) stanza: waddle_xmpp_client::ResumeStanzaSnapshot,
    pub(crate) sent_at_epoch_ms: f64,
}

impl TryFrom<waddle_xmpp_client::SmResumeState> for JsResumeState {
    type Error = ResumeWasmBoundaryError;

    fn try_from(value: waddle_xmpp_client::SmResumeState) -> Result<Self, Self::Error> {
        let unhandled_outbound_entries = value
            .unhandled_outbound_entries()
            .map(|entry| {
                let sent_at_epoch_ms = entry.sent_at().timestamp_millis();
                if !(0..=8_640_000_000_000_000_i64).contains(&sent_at_epoch_ms) {
                    return Err(ResumeWasmBoundaryError::TimestampOutOfRange);
                }
                Ok(JsResumeEntry {
                    stanza: entry.stanza().snapshot()?,
                    sent_at_epoch_ms: sent_at_epoch_ms as f64,
                })
            })
            .collect::<Result<Vec<_>, ResumeWasmBoundaryError>>()?;
        Ok(Self {
            previd: value.previd().to_string(),
            inbound_h: value.inbound_h(),
            outbound_h: value.outbound_h(),
            unhandled_outbound_entries,
            max_resume_seconds: value.max_resume_seconds(),
        })
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

    fn resume_state_input(
        unhandled_outbound_entries: Vec<JsResumeEntryInput>,
    ) -> JsResumeStateInput {
        JsResumeStateInput {
            previd: "wasm-prev".to_owned(),
            inbound_h: 2,
            outbound_h: 3,
            unhandled_outbound_entries,
            max_resume_seconds: Some(300),
        }
    }

    fn resume_state_from_entries(
        entries: Vec<JsResumeEntryInput>,
    ) -> Result<waddle_xmpp_client::SmResumeState, ResumeWasmBoundaryError> {
        resume_state_from_input(resume_state_input(entries))
    }

    fn message_snapshot(
        attributes: Vec<waddle_xmpp_client::ResumeXmlAttribute>,
        text_values: impl IntoIterator<Item = String>,
    ) -> waddle_xmpp_client::ResumeStanzaSnapshot {
        let mut tokens = vec![waddle_xmpp_client::ResumeXmlToken::Start {
            name: waddle_xmpp_client::ResumeXmlName {
                namespace: waddle_xmpp_client::ResumeXmlNamespace::new("jabber:client"),
                local_name: waddle_xmpp_client::ResumeXmlLocalName::new("message")
                    .expect("valid root"),
            },
            attributes,
        }];
        tokens.extend(text_values.into_iter().map(|value| {
            waddle_xmpp_client::ResumeXmlToken::Text {
                value: waddle_xmpp_client::ResumeXmlValue::new(value),
            }
        }));
        tokens.push(waddle_xmpp_client::ResumeXmlToken::End);
        waddle_xmpp_client::ResumeStanzaSnapshot {
            stanza_kind: waddle_xmpp_client::ResumeStanzaKind::Message,
            tokens,
        }
    }

    fn resume_entry(
        stanza: waddle_xmpp_client::ResumeStanzaSnapshot,
        sent_at_epoch_ms: f64,
    ) -> JsResumeEntryInput {
        JsResumeEntryInput {
            stanza,
            sent_at_epoch_ms,
        }
    }

    #[test]
    fn wasm_resume_entry_round_trips_structured_stanza_and_millisecond_time() {
        let timestamp_ms = 1_748_779_140_123_i64;
        let input: JsResumeEntryInput =
            serde_json::from_value(resume_entry_json(timestamp_ms)).expect("typed input");
        let state = resume_state_from_entries(vec![input]).expect("entry converts");
        let output =
            serde_json::to_value(JsResumeState::try_from(state).expect("resume state converts"))
                .expect("output serializes");

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
        assert!(matches!(
            resume_state_from_entries(vec![fractional]),
            Err(ResumeWasmBoundaryError::InvalidTimestamp)
        ));
    }

    #[test]
    fn wasm_resume_entry_rejects_unbalanced_oversized_and_too_deep_tokens() {
        let base: JsResumeEntryInput =
            serde_json::from_value(resume_entry_json(0)).expect("typed input");

        let mut unbalanced = base.stanza.clone();
        unbalanced.tokens.pop();
        assert!(resume_state_from_entries(vec![JsResumeEntryInput {
            stanza: unbalanced,
            sent_at_epoch_ms: 0.0,
        }])
        .is_err());

        let mut oversized = base.stanza.clone();
        let root = oversized.tokens[0].clone();
        oversized.tokens = vec![root];
        oversized.tokens.extend(
            (0..16_383).map(|_| waddle_xmpp_client::ResumeXmlToken::Text {
                value: waddle_xmpp_client::ResumeXmlValue::new(""),
            }),
        );
        oversized
            .tokens
            .push(waddle_xmpp_client::ResumeXmlToken::End);
        assert!(resume_state_from_entries(vec![JsResumeEntryInput {
            stanza: oversized,
            sent_at_epoch_ms: 0.0,
        }])
        .expect_err("token limit rejected")
        .to_string()
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
        assert!(resume_state_from_entries(vec![JsResumeEntryInput {
            stanza: waddle_xmpp_client::ResumeStanzaSnapshot {
                stanza_kind: waddle_xmpp_client::ResumeStanzaKind::Message,
                tokens,
            },
            sent_at_epoch_ms: 0.0,
        }])
        .expect_err("depth limit rejected")
        .to_string()
        .contains("depth limit"));
    }

    #[test]
    fn wasm_resume_state_requires_positive_max_and_timestamp_intersection() {
        let mut zero_max = resume_state_input(Vec::new());
        zero_max.max_resume_seconds = Some(0);
        assert!(matches!(
            resume_state_from_input(zero_max),
            Err(ResumeWasmBoundaryError::InvalidMaxResumeSeconds)
        ));

        let snapshot = message_snapshot(Vec::new(), []);
        let chrono_max_ms = chrono::DateTime::<chrono::Utc>::MAX_UTC.timestamp_millis();
        for timestamp in [0.0, 1_748_779_140_123.0, chrono_max_ms as f64] {
            resume_state_from_entries(vec![resume_entry(snapshot.clone(), timestamp)])
                .expect("timestamp in JS Date and chrono intersection");
        }

        for timestamp in [-1.0, 1.5, f64::NAN, f64::INFINITY, 9_007_199_254_740_992.0] {
            assert!(matches!(
                resume_state_from_entries(vec![resume_entry(snapshot.clone(), timestamp)]),
                Err(ResumeWasmBoundaryError::InvalidTimestamp)
            ));
        }
        assert!(matches!(
            resume_state_from_entries(vec![resume_entry(snapshot, chrono_max_ms as f64 + 1.0,)]),
            Err(ResumeWasmBoundaryError::TimestampOutOfRange)
        ));
    }

    #[test]
    fn wasm_resume_state_accepts_exact_entry_limit_and_rejects_limit_plus_one() {
        let entry = resume_entry(message_snapshot(Vec::new(), []), 0.0);
        let exact = resume_state_from_entries(vec![entry.clone(); 4_096])
            .expect("exact WASM resume entry limit");
        assert_eq!(exact.unhandled_outbound_entries().count(), 4_096);

        assert!(matches!(
            resume_state_from_entries(vec![entry; 4_097]),
            Err(ResumeWasmBoundaryError::ResumeStanza(
                waddle_xmpp_client::ResumeStanzaError::EntryLimit
            ))
        ));
    }

    #[test]
    fn wasm_resume_state_shares_exact_aggregate_xml_budgets() {
        let first_token_count = 16_384 / 2;
        let second_token_count = 16_384 - first_token_count;
        let first = resume_entry(
            message_snapshot(
                Vec::new(),
                std::iter::repeat_n(String::new(), first_token_count - 2),
            ),
            0.0,
        );
        let second = resume_entry(
            message_snapshot(
                Vec::new(),
                std::iter::repeat_n(String::new(), second_token_count - 2),
            ),
            0.0,
        );
        resume_state_from_entries(vec![first.clone(), second.clone()])
            .expect("exact aggregate token budget");
        let mut token_overflow = second;
        token_overflow.stanza.tokens.insert(
            1,
            waddle_xmpp_client::ResumeXmlToken::Text {
                value: waddle_xmpp_client::ResumeXmlValue::new(""),
            },
        );
        assert!(matches!(
            resume_state_from_entries(vec![first, token_overflow]),
            Err(ResumeWasmBoundaryError::ResumeStanza(
                waddle_xmpp_client::ResumeStanzaError::TokenLimit
            ))
        ));

        let attributes = |count: usize| {
            (0..count)
                .map(|index| waddle_xmpp_client::ResumeXmlAttribute {
                    name: waddle_xmpp_client::ResumeXmlName {
                        namespace: waddle_xmpp_client::ResumeXmlNamespace::new(""),
                        local_name: waddle_xmpp_client::ResumeXmlLocalName::new(format!(
                            "a{index}"
                        ))
                        .expect("valid attribute name"),
                    },
                    value: waddle_xmpp_client::ResumeXmlValue::new(""),
                })
                .collect::<Vec<_>>()
        };
        let first = resume_entry(message_snapshot(attributes(8_192), []), 0.0);
        let second = resume_entry(message_snapshot(attributes(8_192), []), 0.0);
        resume_state_from_entries(vec![first.clone(), second.clone()])
            .expect("exact aggregate attribute budget");
        let mut attribute_overflow = second;
        let waddle_xmpp_client::ResumeXmlToken::Start { attributes, .. } =
            &mut attribute_overflow.stanza.tokens[0]
        else {
            panic!("message snapshot starts with root");
        };
        attributes.push(waddle_xmpp_client::ResumeXmlAttribute {
            name: waddle_xmpp_client::ResumeXmlName {
                namespace: waddle_xmpp_client::ResumeXmlNamespace::new(""),
                local_name: waddle_xmpp_client::ResumeXmlLocalName::new("overflow")
                    .expect("valid attribute name"),
            },
            value: waddle_xmpp_client::ResumeXmlValue::new(""),
        });
        assert!(matches!(
            resume_state_from_entries(vec![first, attribute_overflow]),
            Err(ResumeWasmBoundaryError::ResumeStanza(
                waddle_xmpp_client::ResumeStanzaError::AttributeLimit
            ))
        ));

        let root_bytes = 2 * ("jabber:client".len() + "message".len());
        let exact_text = "x".repeat(1024 * 1024 - root_bytes);
        let first = resume_entry(message_snapshot(Vec::new(), [exact_text]), 0.0);
        let second = resume_entry(message_snapshot(Vec::new(), []), 0.0);
        resume_state_from_entries(vec![first.clone(), second])
            .expect("exact aggregate UTF-8 budget");
        let byte_overflow = resume_entry(message_snapshot(Vec::new(), ["x".to_owned()]), 0.0);
        assert!(matches!(
            resume_state_from_entries(vec![first, byte_overflow]),
            Err(ResumeWasmBoundaryError::ResumeStanza(
                waddle_xmpp_client::ResumeStanzaError::ByteLimit
            ))
        ));
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
    Error {
        reason: DriverErrorReason,
        authentication_condition: Option<DriverAuthenticationCondition>,
    },
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DriverErrorReason {
    #[serde(rename = "core-error")]
    Core,
    InvalidTransportScheme,
    #[serde(rename = "missing-websocket-host")]
    MissingWebSocketHost,
    EmptyResource,
    EmptyStanzaId,
    RequestIdExhausted,
    DuplicateRequest,
    DuplicateStanzaCorrelation,
    UnknownRequest,
    UnknownStanzaCorrelation,
    InvalidPhaseTransition,
    InvalidStateTransition,
    MissingStreamFeature,
    InvalidStreamFeatures,
    InvalidSaslFailure,
    InvalidBindResponse,
    AuthenticationRejected,
    #[serde(rename = "websocket-connect-timeout")]
    WebSocketConnectTimeout,
    #[serde(rename = "websocket-write-timeout")]
    WebSocketWriteTimeout,
    IqTimeout,
    #[serde(rename = "websocket-transport-error")]
    WebSocketTransport,
    EmptyTransportFrame,
    TransportFrameTooLarge,
    InvalidTransportFrame,
    InvalidStreamOpenTo,
    InvalidStreamOpenFrom,
    UnsupportedStreamVersion,
    #[serde(rename = "unsupported-websocket-message")]
    UnsupportedWebSocketMessage,
    TransportClosed,
    RequestCancelled,
    Disconnected,
    InvalidResumeStanza,
    #[serde(rename = "push-registration-error")]
    PushRegistration,
    StanzaError,
}

impl DriverErrorReason {
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Core,
        Self::InvalidTransportScheme,
        Self::MissingWebSocketHost,
        Self::EmptyResource,
        Self::EmptyStanzaId,
        Self::RequestIdExhausted,
        Self::DuplicateRequest,
        Self::DuplicateStanzaCorrelation,
        Self::UnknownRequest,
        Self::UnknownStanzaCorrelation,
        Self::InvalidPhaseTransition,
        Self::InvalidStateTransition,
        Self::MissingStreamFeature,
        Self::InvalidStreamFeatures,
        Self::InvalidSaslFailure,
        Self::InvalidBindResponse,
        Self::AuthenticationRejected,
        Self::WebSocketConnectTimeout,
        Self::WebSocketWriteTimeout,
        Self::IqTimeout,
        Self::WebSocketTransport,
        Self::EmptyTransportFrame,
        Self::TransportFrameTooLarge,
        Self::InvalidTransportFrame,
        Self::InvalidStreamOpenTo,
        Self::InvalidStreamOpenFrom,
        Self::UnsupportedStreamVersion,
        Self::UnsupportedWebSocketMessage,
        Self::TransportClosed,
        Self::RequestCancelled,
        Self::Disconnected,
        Self::InvalidResumeStanza,
        Self::PushRegistration,
        Self::StanzaError,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DriverAuthenticationCondition {
    Aborted,
    AccountDisabled,
    CredentialsExpired,
    EncryptionRequired,
    IncorrectEncoding,
    InvalidAuthzid,
    InvalidMechanism,
    MalformedRequest,
    MechanismTooWeak,
    NotAuthorized,
    TemporaryAuthFailure,
    Unknown,
}

impl DriverAuthenticationCondition {
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Aborted,
        Self::AccountDisabled,
        Self::CredentialsExpired,
        Self::EncryptionRequired,
        Self::IncorrectEncoding,
        Self::InvalidAuthzid,
        Self::InvalidMechanism,
        Self::MalformedRequest,
        Self::MechanismTooWeak,
        Self::NotAuthorized,
        Self::TemporaryAuthFailure,
        Self::Unknown,
    ];
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
