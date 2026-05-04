//! Shared Message Archive Management (MAM) primitives and helpers.
//!
//! These types and builders are safe to share across server and client code.

use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use minidom::Element;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType};

use crate::xep0201::ThreadInfo;
use crate::xep0359::{OriginId, StanzaId};
use crate::{CoreError, CoreResult};

/// MAM XML namespace (XEP-0313 v2).
pub const MAM_NS: &str = "urn:xmpp:mam:2";

/// Waddle MAM thread filter namespace.
///
/// XEP-0313 permits extension data form fields, but `{urn:xmpp:mam:2}thread`
/// is not a standard MAM field. Keep Waddle-specific filtering in a Waddle
/// namespace so official MAM semantics stay conformant.
pub const WADDLE_MAM_THREAD_NS: &str = "urn:waddle:mam-thread:0";
pub const WADDLE_MAM_THREAD_FIELD: &str = "{urn:waddle:mam-thread:0}thread";

/// Full Text Search in MAM namespace (XEP-0431).
pub const FULLTEXT_MAM_NS: &str = "urn:xmpp:fulltext:0";
pub const FULLTEXT_MAM_FIELD: &str = "{urn:xmpp:fulltext:0}fulltext";

/// Result Set Management namespace (XEP-0059).
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";

/// Data Forms namespace.
pub const DATA_FORMS_NS: &str = "jabber:x:data";

/// Stanza ID namespace (XEP-0359).
pub const STANZA_ID_NS: &str = "urn:xmpp:sid:0";

/// Forward namespace (XEP-0297).
pub const FORWARD_NS: &str = "urn:xmpp:forward:0";

/// Delay namespace (XEP-0203).
pub const DELAY_NS: &str = "urn:xmpp:delay";

const CLIENT_NS: &str = "jabber:client";
const REPLY_NS: &str = "urn:xmpp:reply:0";
const MESSAGE_CORRECT_NS: &str = "urn:xmpp:message-correct:0";
const MESSAGE_RETRACT_NS: &str = "urn:xmpp:message-retract:1";
const MESSAGE_MODERATE_NS: &str = "urn:xmpp:message-moderate:1";
const REACTIONS_NS: &str = "urn:xmpp:reactions:0";
const REFERENCE_NS: &str = "urn:xmpp:reference:0";
const MENTIONS_NS: &str = "urn:xmpp:mentions:0";
const XDATA_VALIDATE_NS: &str = "http://jabber.org/protocol/xdata-validate";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichMessageId(pub String);

impl RichMessageId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichText(pub String);

impl RichText {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReply {
    pub id: RichMessageId,
    pub to: Option<Jid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReference {
    pub ref_type: RichText,
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub uri: Option<RichText>,
    pub anchor: Option<RichText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedMention {
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub jid: Option<BareJid>,
    pub occupant_id: Option<RichText>,
    pub mentions: Option<RichText>,
    pub uri: Option<RichText>,
    pub active: bool,
    pub noping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReactionSet {
    pub target_id: RichMessageId,
    pub emojis: Vec<RichText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedRetraction {
    pub target_id: RichMessageId,
    pub stamp: Option<DateTime<Utc>>,
    pub retraction_id: Option<RichMessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedModeration {
    pub target_id: RichMessageId,
    pub moderated_by: Jid,
    pub stamp: Option<DateTime<Utc>>,
    pub reason: Option<RichText>,
}

/// A XEP-0424 tombstone replacing a retracted message in the archive.
///
/// When the optional `moderation` is `Some`, this is a XEP-0425
/// moderation tombstone whose `<retracted/>` element wraps a
/// `<moderated by/>` annotation; otherwise it is a plain XEP-0424
/// sender-initiated retraction tombstone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedTombstone {
    /// Stanza-id of the retraction message that produced the
    /// tombstone (for clients to correlate the tombstone to the
    /// retraction event that caused it). May be `None` for
    /// IQ-driven moderation tombstones whose request is not itself
    /// archived as a separate row.
    pub retraction_id: Option<RichMessageId>,
    /// XEP-0424 §"`<retracted/>` SHOULD include a 'stamp' attribute
    /// indicating the time at which the retraction took place."
    pub stamp: DateTime<Utc>,
    /// When set, this tombstone is the result of XEP-0425 moderation
    /// rather than a sender-initiated XEP-0424 retraction.
    pub moderation: Option<ArchivedModeration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchivedRichPayload {
    Correction {
        replaces_id: RichMessageId,
    },
    Retraction(ArchivedRetraction),
    Moderation(ArchivedModeration),
    Reactions(ArchivedReactionSet),
    /// In-place tombstone produced by XEP-0424 retraction or
    /// XEP-0425 moderation. The original row's `body` and
    /// leak-prone fields (`thread`, `reply`, `stanza_xml`,
    /// mentions, ...) are cleared when this variant is set, per
    /// XEP-0424 §Tombstones / XEP-0425 §Tombstones: "any related
    /// elements which might leak information about the original
    /// message" must be replaced.
    Tombstone(ArchivedTombstone),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArchivedRichMessage {
    pub payload: Option<ArchivedRichPayload>,
    pub reply: Option<ArchivedReply>,
    pub references: Vec<ArchivedReference>,
    pub mentions: Vec<ArchivedMention>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadId(String);

impl ThreadId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Archived message metadata.
///
/// `Serialize`/`Deserialize` are deliberately not derived: the
/// canonical archive row format is the SQL columns + the
/// JSON-encoded `rich_payload` column ([`ArchivedRichMessage`]),
/// not the whole struct. Embedded typed values
/// ([`ThreadInfo`], [`StanzaId`], [`OriginId`], [`ArchivedReply`])
/// keep their derives because they appear inside the JSON-encoded
/// payload column. Keeping `ArchivedMessage` itself non-serde keeps
/// the boundary between "row shape that touches storage" and
/// "rich payload blob that is JSON in the row" explicit.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    /// Unique message ID.
    pub id: String,
    /// Timestamp when the message was received.
    pub timestamp: DateTime<Utc>,
    /// Sender JID.
    ///
    /// Typed as `jid::Jid` (not `String`) per the typed-payloads hard
    /// rule. Carries either a bare or full JID — the projection
    /// preserves the wire form (full JID for groupchat occupants,
    /// bare/full as appropriate for 1:1) so replay round-trips
    /// resource and domain components without lossy reparse.
    pub from: Jid,
    /// Recipient JID (room JID for MUC, or contact bare JID for 1:1).
    ///
    /// Typed as `jid::Jid` for the same reason as [`Self::from`]. The
    /// archive write site supplies the canonical addressing tuple
    /// upstream of this struct.
    pub to: Jid,
    /// Message body, preserving the wire-fidelity distinction between
    /// "no `<body>` element on the wire" (`None`) and "an empty
    /// `<body></body>` element" (`Some("")`).
    ///
    /// RFC 6121 §5.2.3 makes `<body>` optional: subject-only,
    /// reaction-only, and other annotation-only messages may omit it
    /// entirely. Earlier denormalizations collapsed both cases to the
    /// empty string, so consumers reading this field directly (rather
    /// than re-parsing `stanza_xml`) saw a misleading "empty body" for
    /// stanzas that had no `<body>` element at all. Preserving the
    /// distinction restores XEP-0313 §3 archive fidelity for the
    /// denormalized projection.
    pub body: Option<String>,
    /// XEP-0359 stanza id captured for the archived row.
    ///
    /// Typed as [`xep0359::StanzaId`] (`{ id, by }`) so the
    /// `by` attribute (REQUIRED per XEP-0359 §3) is structurally
    /// inseparable from the id value. The `by` JID is reconstructed
    /// from the storage row's archive JID at decode time per the
    /// locked Q4 design — there is no separate `by` column in the
    /// SQL schema.
    ///
    /// [`xep0359::StanzaId`]: crate::xep0359::StanzaId
    pub stanza_id: Option<StanzaId>,
    /// XEP-0201 thread reference (RFC 6121 `<thread/>` id plus
    /// optional nested-thread `parent` attribute).
    ///
    /// Collapsed from the previous `thread_id: Option<String>` and
    /// `parent_thread_id: Option<ThreadId>` pair into a single typed
    /// field: `thread.id` is the RFC 6121 thread id and
    /// `thread.parent` is the optional XEP-0201 nested-thread parent.
    /// Modelling them together makes the "parent without id" invalid
    /// state unrepresentable (you cannot construct
    /// [`ThreadInfo`] with a parent and no id), aligning with the
    /// typed-payloads hard rule.
    ///
    /// Cleared on XEP-0424 / XEP-0425 tombstones — see the
    /// [`ArchivedRichPayload::Tombstone`] doc comment for the full
    /// list of leak-prone fields.
    ///
    /// Storage layout is unchanged: the SQL schema still has two
    /// columns (`thread_id` and `parent_thread_id`); encode splits
    /// this struct into the two columns and decode combines them.
    pub thread: Option<ThreadInfo>,
    /// XEP-0461 reply reference: the id of the replied-to message and
    /// the optional original sender JID (`<reply id='X' to='Y'/>`).
    ///
    /// Collapsed from the previous `reply_to_id: Option<String>` and
    /// `reply_to_jid: Option<String>` pair into a single typed
    /// [`ArchivedReply`] field. Modelling them together makes the
    /// "reply target sender without reply target id" invalid state
    /// unrepresentable (you cannot construct [`ArchivedReply`] with a
    /// `to` JID and no id), aligning with the typed-payloads hard rule
    /// and matching the canonical typed shape already used inside
    /// [`ArchivedRichMessage::reply`].
    ///
    /// Cleared on XEP-0424 / XEP-0425 tombstones — see the
    /// [`ArchivedRichPayload::Tombstone`] doc comment for the full
    /// list of leak-prone fields.
    ///
    /// Storage layout is unchanged: the SQL schema still has two
    /// columns (`reply_to_id` and `reply_to_jid`) plus the
    /// `idx_mam_room_reply_to` index; encode splits this struct into
    /// the two columns and decode combines them.
    pub reply: Option<ArchivedReply>,
    /// XEP-0359 origin-id supplied by client.
    ///
    /// Typed as [`xep0359::OriginId`] for symmetry with
    /// [`Self::stanza_id`]. The newtype carries only the id value
    /// (XEP-0359 origin-ids have no `by` attribute).
    ///
    /// [`xep0359::OriginId`]: crate::xep0359::OriginId
    pub origin_id: Option<OriginId>,
    /// Wire `<message type='…'/>` attribute, typed exactly per
    /// RFC 6121 §5.2.2 (the closed set: `chat`, `error`, `groupchat`,
    /// `headline`, `normal`).
    ///
    /// Pre-#228 this was `String` with a `default_message_type() =
    /// "chat"` serde default. The string-typed shape papered over two
    /// problems: the lossy `mam_message_type(&MessageType) -> String`
    /// stringifier in the projection round-tripped the wire-typed
    /// value through a string back into a string-only column, and the
    /// `"chat"` serde default contradicted RFC 6121 §5.2.2 ("If
    /// absent, the message is implicitly of type `normal`."). Typing
    /// this field as [`MessageType`] propagates the wire-parsed value
    /// directly and makes [`MessageType::default()`] == [`Normal`]
    /// the source of the absent-type semantics — see the test
    /// [`tests::message_type_default_matches_rfc6121_5_2_2`].
    ///
    /// [`Normal`]: xmpp_parsers::message::MessageType::Normal
    pub message_type: MessageType,
    /// Preserved full stanza XML for faithful replay of archived timeline events.
    pub stanza_xml: Option<String>,
    /// Typed rich-message payload and annotations used to reconstruct XMPP payloads.
    pub rich: Option<ArchivedRichMessage>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's MUC nickname
    /// at archive-write time. Only set for `groupchat` rows; `None`
    /// otherwise. Used to disallow corrections across leave/rejoin
    /// cycles — the correction handler refuses if the room's current
    /// generation for the same nickname has advanced.
    pub nickname_generation: Option<u64>,
}

impl ArchivedMessage {
    /// Test-only constructor that stamps `timestamp = Utc::now()` and
    /// fills every other optional field with its no-op default. Pulled
    /// in to replace `..Default::default()` ergonomics in test fixtures
    /// after the `Default` impl was dropped (no sensible default for
    /// [`Jid`]).
    ///
    /// `from` and `to` are mandatory because they have no safe
    /// fallback — the previous `Default::default()` returned an empty
    /// `String` which silently bypassed JID validity. Callers must
    /// supply real typed JIDs.
    ///
    /// Exported (`pub`, `#[doc(hidden)]`) so test fixtures in dependent
    /// crates (e.g. `waddle-xmpp`, `waddle-server`) can use it. Gated
    /// behind `cfg(test)` for in-crate tests and the `test-utils`
    /// Cargo feature for cross-crate consumers — production builds do
    /// not pull in this fixture constructor.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn for_test(from: Jid, to: Jid) -> Self {
        Self {
            id: String::new(),
            timestamp: Utc::now(),
            from,
            to,
            body: None,
            stanza_id: None,
            thread: None,
            reply: None,
            origin_id: None,
            // RFC 6121 §5.2.2: absent type defaults to `normal`.
            // Identical to `MessageType::default()` — written
            // explicitly here to make the conformance contract
            // visible at the test-fixture site.
            message_type: MessageType::Normal,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        }
    }
}

/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter.
    pub start: Option<DateTime<Utc>>,
    /// End time filter.
    pub end: Option<DateTime<Utc>>,
    /// Filter by sender or recipient JID per XEP-0313 §4.1.5 `with`
    /// field. Typed as `jid::Jid` (not `String`) per the typed-payloads
    /// hard rule; parsing happens once at the IQ-form parse boundary
    /// inside [`parse_data_form`] and a malformed value is rejected as
    /// `bad-request` rather than silently substituted.
    pub with: Option<Jid>,
    /// Filter by Waddle thread root id.
    pub thread_id: Option<ThreadId>,
    /// XEP-0431 full-text search terms.
    pub fulltext: Option<RichText>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Extended MAM filter: only messages before this archive ID.
    pub filter_before_id: Option<String>,
    /// Extended MAM filter: only messages after this archive ID.
    pub filter_after_id: Option<String>,
    /// Extended MAM filter: only these archive IDs.
    pub ids: Vec<String>,
    /// RSM pagination cursor: before this ID.
    pub before_id: Option<String>,
    /// RSM pagination cursor: after this ID.
    pub after_id: Option<String>,
}

/// MAM query result.
#[derive(Debug, Clone)]
pub struct MamResult {
    /// Retrieved messages.
    pub messages: Vec<ArchivedMessage>,
    /// Whether there are more messages available.
    pub complete: bool,
    /// First message ID in the result set.
    pub first_id: Option<String>,
    /// Last message ID in the result set.
    pub last_id: Option<String>,
    /// Total count (if available).
    pub count: Option<u32>,
}

/// Parse a MAM query from an IQ stanza.
pub fn parse_mam_query(iq: &Iq) -> CoreResult<(String, MamQuery)> {
    let query_elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "query" && elem.ns() == MAM_NS => elem,
        IqType::Set(_) | IqType::Get(_) => {
            return Err(CoreError::bad_request(Some(
                "Missing MAM query element".to_string(),
            )));
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "Invalid IQ type for MAM query".to_string(),
            )));
        }
    };

    let query_id = query_elem
        .attr("queryid")
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    let mut mam_query = MamQuery::default();

    for child in query_elem.children() {
        if child.name() == "x" && child.ns() == DATA_FORMS_NS {
            parse_data_form(child, &mut mam_query)?;
        } else if child.name() == "set" && child.ns() == RSM_NS {
            parse_rsm(child, &mut mam_query)?;
        }
    }

    debug!(query_id = %query_id, query = ?mam_query, "Parsed MAM query");

    Ok((query_id, mam_query))
}

/// Check if an IQ asks for the XEP-0313 supported query fields form.
pub fn is_mam_query_form_request(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Get(elem) if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Check if an IQ is a MAM query.
pub fn is_mam_query(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Set(elem) | IqType::Get(elem)
            if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Build the XEP-0313 supported query fields response.
pub fn build_query_form_iq(original_iq: &Iq) -> Iq {
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "form")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(MAM_NS)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "with")
                .attr("type", "jid-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "start")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "end")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "before-id")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "after-id")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "ids")
                .attr("type", "list-multi")
                .append(
                    Element::builder("validate", XDATA_VALIDATE_NS)
                        .attr("datatype", "xs:string")
                        .append(Element::builder("open", XDATA_VALIDATE_NS).build())
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", WADDLE_MAM_THREAD_FIELD)
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", FULLTEXT_MAM_FIELD)
                .attr("type", "text-single")
                .build(),
        )
        .build();
    let query = Element::builder("query", MAM_NS).append(form).build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(query)),
    }
}

/// Build MAM result messages for each archived message.
///
/// `to_jid` is typed as `&jid::Jid` so the recipient address is a
/// validated value flowing in from the IQ wire-parse boundary; the
/// previous string-typed parameter forced an internal `parse()` whose
/// only failure mode was the `parse_message_jid` "unknown@invalid"
/// fallback (a hot-path data-loss bug).
pub fn build_result_messages(
    query_id: &str,
    to_jid: &Jid,
    messages: &[ArchivedMessage],
) -> Vec<Message> {
    messages
        .iter()
        .map(|archived| build_result_message(query_id, to_jid, archived))
        .collect()
}

/// Build the MAM fin (completion) IQ response.
pub fn build_fin_iq(original_iq: &Iq, result: &MamResult) -> Iq {
    let fin = Element::builder("fin", MAM_NS)
        .attr("complete", if result.complete { "true" } else { "false" })
        .append(build_rsm_response_element(result))
        .build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(fin)),
    }
}

fn parse_data_form(form: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for field in form.children() {
        if field.name() != "field" {
            continue;
        }

        let var = field.attr("var").unwrap_or("");
        let values = field
            .children()
            .filter(|child| child.name() == "value")
            .map(Element::text)
            .collect::<Vec<_>>();
        let value = values.iter().find(|value| !value.is_empty()).cloned();

        match var {
            "" | "FORM_TYPE" => {}
            "start" => {
                if let Some(value) = value {
                    query.start = Some(parse_datetime(&value)?);
                }
            }
            "end" => {
                if let Some(value) = value {
                    query.end = Some(parse_datetime(&value)?);
                }
            }
            "with" => {
                if let Some(value) = value {
                    let parsed = value.parse::<Jid>().map_err(|error| {
                        CoreError::bad_request(Some(format!(
                            "Invalid `with` JID in MAM query: {error}"
                        )))
                    })?;
                    query.with = Some(parsed);
                }
            }
            "before-id" => {
                query.filter_before_id = value;
            }
            "after-id" => {
                query.filter_after_id = value;
            }
            "ids" => {
                query.ids = values
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect();
            }
            WADDLE_MAM_THREAD_FIELD => {
                query.thread_id = value.and_then(ThreadId::new);
            }
            FULLTEXT_MAM_FIELD => {
                query.fulltext = value.and_then(RichText::new);
            }
            _ => {
                return Err(CoreError::NotImplemented);
            }
        }
    }

    Ok(())
}

fn parse_rsm(rsm: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for child in rsm.children() {
        match child.name() {
            "max" => {
                let value = child.text();
                if !value.is_empty() {
                    query.max = Some(value.parse().map_err(|_| {
                        CoreError::bad_request(Some(format!("Invalid RSM max value: {}", value)))
                    })?);
                }
            }
            "after" => {
                let value = child.text();
                if !value.is_empty() {
                    query.after_id = Some(value);
                }
            }
            "before" => {
                query.before_id = Some(child.text());
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_datetime(value: &str) -> CoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|error| CoreError::bad_request(Some(format!("Invalid datetime: {}", error))))
}

fn build_result_message(query_id: &str, to_jid: &Jid, archived: &ArchivedMessage) -> Message {
    let inner_msg = archived_inner_message(archived);
    let delay = Element::builder("delay", DELAY_NS)
        .attr("stamp", archived.timestamp.to_rfc3339())
        .build();
    let forwarded = Element::builder("forwarded", FORWARD_NS)
        .append(delay)
        .append(inner_msg)
        .build();
    let result = Element::builder("result", MAM_NS)
        .attr("queryid", query_id)
        .attr("id", &archived.id)
        .append(forwarded)
        .build();

    let mut msg = Message::new(Some(to_jid.clone()));
    msg.id = Some(Uuid::now_v7().to_string());
    msg.type_ = MessageType::Normal;
    msg.payloads.push(result);
    msg
}

fn archived_inner_message(archived: &ArchivedMessage) -> Element {
    if let Some(stanza_xml) = archived.stanza_xml.as_deref() {
        match stanza_xml.parse::<Element>() {
            Ok(element) => return normalize_archived_inner_message(element, archived),
            Err(error) => {
                warn!(
                    archive_id = %archived.id,
                    error = %error,
                    "Failed to parse archived stanza XML, falling back to typed reconstruction"
                );
            }
        }
    }

    if let Some(rich) = archived.rich.as_ref() {
        return build_typed_inner_message(archived, rich);
    }

    build_legacy_inner_message(archived)
}

/// Wire-form name for a [`MessageType`] (the closed RFC 6121 §5.2.2
/// set: `chat`, `error`, `groupchat`, `headline`, `normal`).
///
/// `xmpp_parsers::message::MessageType` is generated by the
/// `generate_attribute! { …, Default = Normal }` macro, which
/// implements `IntoAttributeValue` (returns `None` for the default
/// to omit the attribute on serialize) and `xso::AsXmlText`, but
/// **not** `Display` — `to_string()` on the enum does not exist.
///
/// We need an unconditional wire-form string for both replay
/// (`<message type='…'/>` is always emitted with an explicit type
/// for archived rows, including the default) and SQL bind. This
/// helper provides exactly that: a total mapping from the typed
/// enum back to the same wire literal `MessageType::from_str`
/// would parse.
pub fn message_type_wire_str(message_type: &MessageType) -> &'static str {
    match message_type {
        MessageType::Chat => "chat",
        MessageType::Error => "error",
        MessageType::Groupchat => "groupchat",
        MessageType::Headline => "headline",
        MessageType::Normal => "normal",
    }
}

fn normalize_archived_inner_message(element: Element, archived: &ArchivedMessage) -> Element {
    if archived.message_type != MessageType::Groupchat {
        return element;
    }

    let mut normalized = if element.attr("to").is_none() {
        element
    } else {
        let ns = element.ns().to_string();
        let name = element.name().to_string();
        let mut builder = Element::builder(name, &ns);
        for (key, value) in element.attrs() {
            if key != "to" {
                builder = builder.attr(key, value);
            }
        }
        for child in element.children().cloned() {
            builder = builder.append(child);
        }
        builder.build()
    };

    if let Some(info) = archived.thread.as_ref() {
        crate::xep0201::install_thread_element(&mut normalized, info);
    }

    normalized
}

fn build_typed_inner_message(archived: &ArchivedMessage, rich: &ArchivedRichMessage) -> Element {
    let msg_type = archived.message_type.clone();

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", archived.from.to_string())
        .attr("type", message_type_wire_str(&msg_type));
    if msg_type != MessageType::Groupchat {
        builder = builder.attr("to", archived.to.to_string());
    }

    if let Some(sid) = archived.stanza_id.as_ref() {
        builder = builder.attr("id", sid.id.as_str());
    }
    // RFC 6121 §5.2.3 / XEP-0313 §3: emit `<body/>` exactly when the
    // archived row recorded one on the wire. `Some("")` is a real
    // empty `<body></body>` element and MUST round-trip as such;
    // `None` MUST emit no `<body/>` element at all (subject-only,
    // reaction-only, and other annotation-only stanzas).
    if let Some(body) = archived.body.as_deref() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(body.to_owned())
                .build(),
        );
    }
    // XEP-0201: emit `<thread parent='X'>id</thread>` via the canonical
    // typed builder so the optional parent attribute round-trips on
    // replay. The "parent without id ⇒ no `<thread/>` element" rule is
    // enforced at the type level by collapsing the previous two flat
    // fields into `Option<ThreadInfo>`: you cannot construct
    // [`ThreadInfo`] with a parent and no id, so a row that was
    // ever decoded into `archived.thread` already carries a coherent
    // shape. A row with no thread metadata at all is `None` and emits
    // nothing.
    if let Some(info) = archived.thread.as_ref() {
        builder = builder.append(crate::xep0201::build_thread_element(info, CLIENT_NS));
    }
    if let Some(oid) = archived.origin_id.as_ref() {
        builder = builder.append(
            Element::builder("origin-id", STANZA_ID_NS)
                .attr("id", oid.id.as_str())
                .build(),
        );
    }
    if msg_type == MessageType::Groupchat && !archived.id.is_empty() {
        builder = builder.append(
            Element::builder("stanza-id", STANZA_ID_NS)
                .attr("id", &archived.id)
                .attr("by", archived.to.to_string())
                .build(),
        );
    }
    if let Some(reply) = rich.reply.as_ref() {
        let mut reply_builder = Element::builder("reply", REPLY_NS).attr("id", reply.id.as_str());
        if let Some(to) = reply.to.as_ref() {
            reply_builder = reply_builder.attr("to", to.to_string());
        }
        builder = builder.append(reply_builder.build());
    }
    for reference in &rich.references {
        let mut reference_builder =
            Element::builder("reference", REFERENCE_NS).attr("type", reference.ref_type.as_str());
        if let Some(begin) = reference.begin {
            reference_builder = reference_builder.attr("begin", begin.to_string());
        }
        if let Some(end) = reference.end {
            reference_builder = reference_builder.attr("end", end.to_string());
        }
        if let Some(uri) = reference.uri.as_ref() {
            reference_builder = reference_builder.attr("uri", uri.as_str());
        }
        if let Some(anchor) = reference.anchor.as_ref() {
            reference_builder = reference_builder.attr("anchor", anchor.as_str());
        }
        builder = builder.append(reference_builder.build());
    }
    for mention in &rich.mentions {
        let mut mention_elem = Element::builder("mention", MENTIONS_NS).build();
        if let Some(begin) = mention.begin {
            mention_elem.set_attr("begin", begin.to_string());
        }
        if let Some(end) = mention.end {
            mention_elem.set_attr("end", end.to_string());
        }
        if let Some(jid) = mention.jid.as_ref() {
            mention_elem.set_attr("jid", jid.to_string());
        }
        if let Some(occupant_id) = mention.occupant_id.as_ref() {
            mention_elem.set_attr("occupantid", occupant_id.as_str());
        }
        if let Some(mentions) = mention.mentions.as_ref() {
            mention_elem.set_attr("mentions", mentions.as_str());
        }
        if let Some(uri) = mention.uri.as_ref() {
            mention_elem.set_attr("uri", uri.as_str());
        }
        if mention.active {
            mention_elem.append_child(Element::builder("active", MENTIONS_NS).build());
        }
        if mention.noping {
            mention_elem.append_child(Element::builder("noping", MENTIONS_NS).build());
        }
        builder = builder.append(mention_elem);
    }

    match rich.payload.as_ref() {
        Some(ArchivedRichPayload::Correction { replaces_id }) => {
            builder = builder.append(
                Element::builder("replace", MESSAGE_CORRECT_NS)
                    .attr("id", replaces_id.as_str())
                    .build(),
            );
        }
        Some(ArchivedRichPayload::Retraction(retraction)) => {
            let retract_builder = Element::builder("retract", MESSAGE_RETRACT_NS)
                .attr("id", retraction.target_id.as_str());
            builder = builder.append(retract_builder.build());
        }
        Some(ArchivedRichPayload::Moderation(moderation)) => {
            let moderated = Element::builder("moderated", MESSAGE_MODERATE_NS)
                .attr("by", moderation.moderated_by.to_string())
                .build();
            let mut retract = Element::builder("retract", MESSAGE_RETRACT_NS)
                .attr("id", moderation.target_id.as_str())
                .append(moderated);
            if let Some(reason) = moderation.reason.as_ref() {
                retract = retract.append(
                    Element::builder("reason", MESSAGE_RETRACT_NS)
                        .append(reason.as_str())
                        .build(),
                );
            }
            builder = builder.append(retract.build());
        }
        Some(ArchivedRichPayload::Reactions(reactions)) => {
            let mut reactions_elem = Element::builder("reactions", REACTIONS_NS)
                .attr("id", reactions.target_id.as_str())
                .build();
            for emoji in &reactions.emojis {
                reactions_elem.append_child(
                    Element::builder("reaction", REACTIONS_NS)
                        .append(emoji.as_str())
                        .build(),
                );
            }
            builder = builder.append(reactions_elem);
        }
        Some(ArchivedRichPayload::Tombstone(tombstone)) => {
            let mut retracted = Element::builder("retracted", MESSAGE_RETRACT_NS)
                .attr("stamp", tombstone.stamp.to_rfc3339());
            if let Some(retraction_id) = tombstone.retraction_id.as_ref() {
                retracted = retracted.attr("id", retraction_id.as_str());
            }
            if let Some(moderation) = tombstone.moderation.as_ref() {
                let moderated = Element::builder("moderated", MESSAGE_MODERATE_NS)
                    .attr("by", moderation.moderated_by.to_string())
                    .build();
                retracted = retracted.append(moderated);
                if let Some(reason) = moderation.reason.as_ref() {
                    retracted = retracted.append(
                        Element::builder("reason", MESSAGE_RETRACT_NS)
                            .append(reason.as_str())
                            .build(),
                    );
                }
            }
            builder = builder.append(retracted.build());
        }
        None => {}
    }

    builder.build()
}

fn build_legacy_inner_message(archived: &ArchivedMessage) -> Element {
    let msg_type = archived.message_type.clone();

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", archived.from.to_string())
        .attr("type", message_type_wire_str(&msg_type));
    if msg_type != MessageType::Groupchat {
        builder = builder.attr("to", archived.to.to_string());
    }

    if let Some(sid) = archived.stanza_id.as_ref() {
        builder = builder.attr("id", sid.id.as_str());
    }
    // RFC 6121 §5.2.3 / XEP-0313 §3: see `build_typed_inner_message`
    // — `Some("")` round-trips as `<body></body>`, `None` omits the
    // element entirely.
    if let Some(body) = archived.body.as_deref() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(body.to_owned())
                .build(),
        );
    }
    // XEP-0201: same emission rule as `build_typed_inner_message`. Use
    // the canonical typed builder so parent round-trips. The "parent
    // without id" incoherence (RFC 6121 §5.2.5) is unrepresentable in
    // [`ThreadInfo`], so emission is simply gated on the entire
    // `archived.thread` being `Some`.
    if let Some(info) = archived.thread.as_ref() {
        builder = builder.append(crate::xep0201::build_thread_element(info, CLIENT_NS));
    }
    if let Some(reply) = archived.reply.as_ref() {
        let mut reply_builder = Element::builder("reply", REPLY_NS).attr("id", reply.id.as_str());
        if let Some(to) = reply.to.as_ref() {
            reply_builder = reply_builder.attr("to", to.to_string());
        }
        builder = builder.append(reply_builder.build());
    }
    if let Some(oid) = archived.origin_id.as_ref() {
        builder = builder.append(
            Element::builder("origin-id", STANZA_ID_NS)
                .attr("id", oid.id.as_str())
                .build(),
        );
    }
    if msg_type == MessageType::Groupchat && !archived.id.is_empty() {
        builder = builder.append(
            Element::builder("stanza-id", STANZA_ID_NS)
                .attr("id", &archived.id)
                .attr("by", archived.to.to_string())
                .build(),
        );
    }

    builder.build()
}

fn build_rsm_response_element(result: &MamResult) -> Element {
    let mut builder = Element::builder("set", RSM_NS);

    if let Some(first) = result.first_id.as_deref() {
        builder = builder.append(Element::builder("first", RSM_NS).append(first).build());
    }
    if let Some(last) = result.last_id.as_deref() {
        builder = builder.append(Element::builder("last", RSM_NS).append(last).build());
    }
    if let Some(count) = result.count {
        builder = builder.append(
            Element::builder("count", RSM_NS)
                .append(count.to_string())
                .build(),
        );
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn jid(value: &str) -> Jid {
        value.parse::<Jid>().expect("valid jid literal")
    }

    /// RFC 6121 §5.2.2 ("Type Attribute"): "If absent, the message is
    /// implicitly of type `normal`."
    ///
    /// `xmpp_parsers::message::MessageType::default()` correctly
    /// returns [`MessageType::Normal`] (verified by reading
    /// `generate_attribute!` `Default = Normal` in
    /// `xmpp-parsers-0.21.0/src/message.rs`). This test pins that
    /// contract: a future bump of `xmpp-parsers` that changes the
    /// default would silently shift the absent-type semantics of every
    /// archived row that hits the typed-decode fallback path.
    ///
    /// Pre-#228 commit 8 the `default_message_type() = "chat"` helper
    /// in this module was a latent conformance bug: archived rows that
    /// went through serde with an absent `message_type` field would
    /// hydrate to `"chat"`, violating the RFC. Deleting that helper
    /// and typing the field as [`MessageType`] anchors the absent-type
    /// semantics to [`MessageType::default()`] (== [`Normal`]), which
    /// matches the RFC.
    ///
    /// [`Normal`]: xmpp_parsers::message::MessageType::Normal
    #[test]
    fn message_type_default_matches_rfc6121_5_2_2() {
        assert_eq!(MessageType::default(), MessageType::Normal);
    }

    #[test]
    fn parses_mam_query_with_form_and_rsm() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-1".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .attr("queryid", "query-1")
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "submit")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "start")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("2024-01-15T10:30:00Z")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "with")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("juliet@example.com")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "before-id")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("msg-20")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "after-id")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("msg-2")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "ids")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("msg-5")
                                            .build(),
                                    )
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("msg-7")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", FULLTEXT_MAM_FIELD)
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("release notes")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("set", RSM_NS)
                            .append(Element::builder("max", RSM_NS).append("10").build())
                            .append(Element::builder("after", RSM_NS).append("msg-9").build())
                            .build(),
                    )
                    .build(),
            ),
        };

        let (query_id, query) = parse_mam_query(&iq).expect("valid MAM query");

        assert_eq!(query_id, "query-1");
        assert_eq!(query.max, Some(10));
        assert_eq!(query.after_id.as_deref(), Some("msg-9"));
        assert_eq!(query.filter_before_id.as_deref(), Some("msg-20"));
        assert_eq!(query.filter_after_id.as_deref(), Some("msg-2"));
        assert_eq!(query.ids, vec!["msg-5", "msg-7"]);
        assert_eq!(
            query.with.as_ref().map(Jid::to_string).as_deref(),
            Some("juliet@example.com")
        );
        assert_eq!(
            query.fulltext.as_ref().map(RichText::as_str),
            Some("release notes")
        );
        let start = query.start.expect("start filter");
        assert_eq!(start.year(), 2024);
        assert_eq!(start.month(), 1);
        assert_eq!(start.day(), 15);
    }

    #[test]
    fn parses_waddle_mam_thread_field() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-thread".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "submit")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", WADDLE_MAM_THREAD_FIELD)
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("thread-root")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "FORM_TYPE")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append(MAM_NS)
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        };

        let (_, query) = parse_mam_query(&iq).expect("valid MAM query");

        assert_eq!(
            query.thread_id.as_ref().map(ThreadId::as_str),
            Some("thread-root")
        );
    }

    #[test]
    fn rejects_unsupported_mam_form_fields() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-thread".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "submit")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "{urn:xmpp:mam:2}thread")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("wrong-thread")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        };

        let err = parse_mam_query(&iq).expect_err("unsupported MAM field");
        assert!(matches!(err, CoreError::NotImplemented));
    }

    #[test]
    fn rejects_legacy_bare_fulltext_mam_form_field() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-legacy-fulltext".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .attr("type", "submit")
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "fulltext")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("release notes")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        };

        let err = parse_mam_query(&iq).expect_err("unsupported MAM field");
        assert!(matches!(err, CoreError::NotImplemented));
    }

    #[test]
    fn builds_mam_query_form_with_waddle_thread_field() {
        let iq = Iq {
            from: Some("juliet@example.com/chamber".parse().expect("from jid")),
            to: Some("room@muc.example.com".parse().expect("to jid")),
            id: "mam-form".to_string(),
            payload: IqType::Get(Element::builder("query", MAM_NS).build()),
        };

        assert!(is_mam_query_form_request(&iq));
        let result = build_query_form_iq(&iq);
        let IqType::Result(Some(query)) = result.payload else {
            panic!("expected query form result");
        };
        let form = query.get_child("x", DATA_FORMS_NS).expect("form");
        let fields = form
            .children()
            .filter_map(|field| field.attr("var"))
            .collect::<Vec<_>>();

        assert!(fields.contains(&"FORM_TYPE"));
        assert!(fields.contains(&"with"));
        assert!(fields.contains(&"start"));
        assert!(fields.contains(&"end"));
        assert!(fields.contains(&"before-id"));
        assert!(fields.contains(&"after-id"));
        assert!(fields.contains(&"ids"));
        assert!(fields.contains(&WADDLE_MAM_THREAD_FIELD));
        assert!(fields.contains(&FULLTEXT_MAM_FIELD));
        assert!(!fields.contains(&"fulltext"));

        let ids_field = form
            .children()
            .find(|field| field.attr("var") == Some("ids"))
            .expect("ids field");
        let validate = ids_field
            .get_child("validate", XDATA_VALIDATE_NS)
            .expect("ids field validate element");
        assert_eq!(validate.attr("datatype"), Some("xs:string"));
        assert!(validate.get_child("open", XDATA_VALIDATE_NS).is_some());
    }

    #[test]
    fn parses_last_page_rsm_before() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-2".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("set", RSM_NS)
                            .append(Element::builder("before", RSM_NS).build())
                            .build(),
                    )
                    .build(),
            ),
        };

        let (_, query) = parse_mam_query(&iq).expect("valid MAM query");

        assert_eq!(query.before_id, Some(String::new()));
    }

    #[test]
    fn rejects_invalid_datetime() {
        let iq = Iq {
            from: None,
            to: None,
            id: "mam-3".to_string(),
            payload: IqType::Set(
                Element::builder("query", MAM_NS)
                    .append(
                        Element::builder("x", DATA_FORMS_NS)
                            .append(
                                Element::builder("field", DATA_FORMS_NS)
                                    .attr("var", "start")
                                    .append(
                                        Element::builder("value", DATA_FORMS_NS)
                                            .append("not-a-date")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        };

        let err = parse_mam_query(&iq).expect_err("invalid MAM query");
        assert!(matches!(err, CoreError::BadRequest(_)));
    }

    #[test]
    fn builds_result_message_from_legacy_fields() {
        let archived = ArchivedMessage {
            id: "msg-123".to_string(),
            thread: Some(ThreadInfo::root(
                ThreadId::new("thread-1").expect("non-empty thread id"),
            )),
            reply: Some(ArchivedReply {
                id: RichMessageId::new("parent-1").expect("non-empty reply id"),
                to: Some("alice@example.com".parse::<Jid>().expect("valid jid")),
            }),
            origin_id: Some(OriginId::new("origin-1")),
            body: Some("Hello, world!".to_string()),
            ..ArchivedMessage::for_test(
                jid("user@example.com/nick"),
                jid("room@conference.example.com"),
            )
        };

        let msg = build_result_messages("query-1", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        assert!(inner_msg.children().any(|c| c.name() == "thread"));
        assert!(inner_msg
            .children()
            .any(|c| c.name() == "reply" && c.ns() == REPLY_NS));
        assert!(inner_msg
            .children()
            .any(|c| c.name() == "origin-id" && c.ns() == STANZA_ID_NS));
    }

    fn nested_thread_archived_for_replay(stanza_xml: Option<String>) -> ArchivedMessage {
        ArchivedMessage {
            id: "msg-thread-nested".to_string(),
            body: Some("nested reply".to_string()),
            stanza_id: Some(StanzaId::new(
                "wire-id-1",
                "bob@example.com".parse::<Jid>().expect("valid jid"),
            )),
            thread: Some(ThreadInfo::child(
                ThreadId::new("child-thread").expect("non-empty thread id"),
                ThreadId::new("root-thread").expect("non-empty parent id"),
            )),
            message_type: MessageType::Chat,
            stanza_xml,
            ..ArchivedMessage::for_test(jid("alice@example.com/web"), jid("bob@example.com"))
        }
    }

    fn replay_inner_thread(msg: &Message) -> &Element {
        let result = msg
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");
        inner_msg
            .children()
            .find(|c| c.name() == "thread")
            .expect("thread child on replay")
    }

    #[test]
    fn xep_0201_typed_replay_emits_thread_parent() {
        // Typed reconstruction path: stanza_xml is None and rich is None
        // so the projection falls through to `build_typed_inner_message`
        // (with `rich` defaulted) — actually this falls into the legacy
        // path since `rich.is_none()`. Use a row that exercises the
        // typed path by setting a rich payload that doesn't carry its
        // own `<thread/>`.
        let archived = ArchivedMessage {
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: None,
                references: vec![],
                mentions: vec![],
            }),
            ..nested_thread_archived_for_replay(None)
        };
        let msgs = build_result_messages("q1", &jid("user@example.com"), &[archived]);
        let thread = replay_inner_thread(&msgs[0]);
        assert_eq!(thread.text().trim(), "child-thread");
        assert_eq!(thread.attr("parent"), Some("root-thread"));
    }

    #[test]
    fn xep_0201_legacy_replay_emits_thread_parent() {
        // Legacy reconstruction path: stanza_xml is None AND rich is
        // None — `build_legacy_inner_message` rebuilds purely from
        // scalar columns.
        let archived = nested_thread_archived_for_replay(None);
        let msgs = build_result_messages("q2", &jid("user@example.com"), &[archived]);
        let thread = replay_inner_thread(&msgs[0]);
        assert_eq!(thread.text().trim(), "child-thread");
        assert_eq!(thread.attr("parent"), Some("root-thread"));
    }

    #[test]
    fn xep_0201_groupchat_stanza_xml_replay_reinstalls_thread_and_strips_to() {
        let archived = ArchivedMessage {
            id: "archive-threaded-reply".to_string(),
            body: Some("threaded reply".to_string()),
            stanza_id: Some(StanzaId::new(
                "wire-id-2",
                "room@conference.example.com"
                    .parse::<Jid>()
                    .expect("valid jid"),
            )),
            thread: Some(ThreadInfo::child(
                ThreadId::new("root-thread").expect("non-empty thread id"),
                ThreadId::new("parent-thread").expect("non-empty parent id"),
            )),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='bob@example.com/web' type='groupchat' id='wire-id-2'><body>threaded reply</body><thread>stale-thread</thread><reply xmlns='urn:xmpp:reply:0' id='root-thread'/></message>"
                    .to_string(),
            ),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let msgs = build_result_messages("q-stanza-xml", &jid("bob@example.com/web"), &[archived]);
        let result = msgs[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");
        let thread = inner_msg
            .children()
            .find(|c| c.name() == "thread")
            .expect("thread child on replay");

        assert_eq!(inner_msg.attr("to"), None);
        assert_eq!(thread.text().trim(), "root-thread");
        assert_eq!(thread.attr("parent"), Some("parent-thread"));
        assert!(inner_msg
            .children()
            .any(|c| c.name() == "reply" && c.ns() == REPLY_NS));
    }

    #[test]
    fn xep_0201_replay_omits_thread_when_id_missing_even_with_parent() {
        // RFC 6121 §5.2.5 incoherence guard: parent without id MUST NOT
        // emit a `<thread/>` element on replay. This locks the rule
        // against future regressions where parent-only state could be
        // smuggled past the projection.
        let archived = ArchivedMessage {
            id: "msg-incoherent".to_string(),
            body: Some("body".to_string()),
            // The collapsed `thread: Option<ThreadInfo>` field makes
            // "parent without id" unrepresentable; the closest legal
            // analog of the previous parent-only state is `None`,
            // which (still) emits no `<thread/>` element. This test
            // continues to pin that emission rule for the no-thread
            // case.
            thread: None,
            message_type: MessageType::Chat,
            stanza_xml: None,
            rich: None,
            ..ArchivedMessage::for_test(jid("alice@example.com/web"), jid("bob@example.com"))
        };
        let msgs = build_result_messages("q3", &jid("user@example.com"), &[archived]);
        let result = msgs[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");
        assert!(
            !inner_msg.children().any(|c| c.name() == "thread"),
            "no thread element should be emitted when thread_id is None"
        );
    }

    #[test]
    fn preserves_archived_stanza_payload() {
        let archived = ArchivedMessage {
            id: "msg-124".to_string(),
            body: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='reaction-1'><reactions xmlns='urn:xmpp:reactions:0' id='msg-1'><reaction>👍</reaction></reactions></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let msg = build_result_messages("query-2", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");
        let reactions = inner_msg
            .children()
            .find(|c| c.name() == "reactions" && c.ns() == "urn:xmpp:reactions:0")
            .expect("reactions payload");

        assert_eq!(inner_msg.attr("id"), Some("reaction-1"));
        assert_eq!(reactions.attr("id"), Some("msg-1"));
    }

    #[test]
    fn builds_fin_iq_with_rsm_metadata() {
        let original = Iq {
            from: Some(jid("romeo@example.com/orchard")),
            to: Some(jid("juliet@example.com/balcony")),
            id: "iq-1".to_string(),
            payload: IqType::Get(Element::builder("query", MAM_NS).build()),
        };
        let result = MamResult {
            messages: Vec::new(),
            complete: true,
            first_id: Some("msg-1".to_string()),
            last_id: Some("msg-2".to_string()),
            count: Some(2),
        };

        let fin = build_fin_iq(&original, &result);
        let payload = match fin.payload {
            IqType::Result(Some(payload)) => payload,
            other => panic!("unexpected fin payload: {:?}", other),
        };
        let set = payload
            .children()
            .find(|child| child.name() == "set" && child.ns() == RSM_NS)
            .expect("rsm set");

        assert_eq!(payload.attr("complete"), Some("true"));
        assert_eq!(
            set.get_child("first", RSM_NS).map(|child| child.text()),
            Some("msg-1".to_string())
        );
        assert_eq!(
            set.get_child("last", RSM_NS).map(|child| child.text()),
            Some("msg-2".to_string())
        );
        assert_eq!(
            set.get_child("count", RSM_NS).map(|child| child.text()),
            Some("2".to_string())
        );
    }

    #[test]
    fn stanza_xml_preferred_over_rich_payload() {
        let archived = ArchivedMessage {
            id: "msg-priority".to_string(),
            body: Some("typed body".to_string()),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' type='groupchat' id='live-1'><body>live body with extension payload</body><item xmlns='urn:example:task-widget:1' id='task-123' status='open'/></message>".to_string(),
            ),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: None,
                references: vec![],
                mentions: vec![],
            }),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let msg = build_result_messages("query-priority", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        let body = inner_msg
            .get_child("body", CLIENT_NS)
            .expect("body element");
        assert_eq!(body.text(), "live body with extension payload");

        let extension_payload = inner_msg
            .children()
            .find(|c| c.name() == "item" && c.ns() == "urn:example:task-widget:1")
            .expect("extension payload");
        assert_eq!(extension_payload.attr("id"), Some("task-123"));
    }

    #[test]
    fn stanza_xml_preserves_reply_fallback() {
        let archived = ArchivedMessage {
            id: "msg-fallback".to_string(),
            body: Some("> Alice wrote:\n> Hello!\nI agree!".to_string()),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/bob' type='groupchat' id='reply-1'><body>&gt; Alice wrote:\n&gt; Hello!\nI agree!</body><reply xmlns='urn:xmpp:reply:0' id='orig-1' to='room@conference.example.com/alice'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='22'/></fallback></message>".to_string(),
            ),
            reply: Some(ArchivedReply {
                id: RichMessageId::new("orig-1").expect("non-empty reply id"),
                to: Some(jid("room@conference.example.com/alice")),
            }),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: Some(ArchivedReply {
                    id: RichMessageId("orig-1".to_string()),
                    to: Some(jid("room@conference.example.com/alice")),
                }),
                references: vec![],
                mentions: vec![],
            }),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/bob"),
                jid("room@conference.example.com"),
            )
        };

        let msg = build_result_messages("query-fallback", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        let reply = inner_msg
            .children()
            .find(|c| c.name() == "reply" && c.ns() == "urn:xmpp:reply:0")
            .expect("reply element");
        assert_eq!(reply.attr("id"), Some("orig-1"));

        let fallback = inner_msg
            .children()
            .find(|c| c.name() == "fallback" && c.ns() == "urn:xmpp:fallback:0")
            .expect("fallback element");
        assert_eq!(fallback.attr("for"), Some("urn:xmpp:reply:0"));
        let fallback_body = fallback
            .get_child("body", "urn:xmpp:fallback:0")
            .expect("fallback body element");
        assert_eq!(fallback_body.attr("start"), Some("0"));
        assert_eq!(fallback_body.attr("end"), Some("22"));
    }

    #[test]
    fn stanza_xml_strips_to_for_groupchat() {
        let archived = ArchivedMessage {
            id: "msg-strip-to".to_string(),
            body: Some("Hello!".to_string()),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='msg-1'><body>Hello!</body></message>".to_string(),
            ),
            ..ArchivedMessage::for_test(
                jid("room@conference.example.com/alice"),
                jid("room@conference.example.com"),
            )
        };

        let msg = build_result_messages("query-strip-to", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        assert!(
            inner_msg.attr("to").is_none(),
            "groupchat MAM replay should not have a 'to' attribute, got: {:?}",
            inner_msg.attr("to")
        );
    }

    #[test]
    fn stanza_xml_strips_to_for_groupchat_on_raw_element_fallback() {
        let stanza_xml = "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='room@conf.example.com' type='groupchat' id='msg-x'><body>test</body><custom xmlns='urn:example:unknown'/></message>";
        let archived = ArchivedMessage {
            id: "msg-strip-raw".to_string(),
            body: Some("test".to_string()),
            message_type: MessageType::Groupchat,
            stanza_xml: Some(stanza_xml.to_string()),
            ..ArchivedMessage::for_test(
                jid("room@conf.example.com/alice"),
                jid("room@conf.example.com"),
            )
        };

        let msg = build_result_messages("query-strip-raw", &jid("user@example.com"), &[archived]);
        let result = msg[0]
            .payloads
            .iter()
            .find(|p| p.name() == "result" && p.ns() == MAM_NS)
            .expect("result payload");
        let forwarded = result
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
            .expect("forwarded element");
        let inner_msg = forwarded
            .children()
            .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
            .expect("inner message");

        assert!(
            inner_msg.attr("to").is_none(),
            "groupchat MAM replay via raw element fallback should not have a 'to' attribute, got: {:?}",
            inner_msg.attr("to")
        );
    }
}
