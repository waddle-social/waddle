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
    /// leak-prone fields (`thread_id`, `parent_thread_id`,
    /// `reply_to_id`, `reply_to_jid`, `stanza_xml`, mentions, ...)
    /// are cleared when this variant is set, per XEP-0424 §Tombstones
    /// / XEP-0425 §Tombstones: "any related elements which might leak
    /// information about the original message" must be replaced.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMessage {
    /// Unique message ID.
    pub id: String,
    /// Timestamp when the message was received.
    pub timestamp: DateTime<Utc>,
    /// Sender JID.
    pub from: String,
    /// Recipient JID (room JID for MUC, or contact bare JID for 1:1).
    pub to: String,
    /// Message body.
    pub body: String,
    /// Original stanza ID (if present).
    pub stanza_id: Option<String>,
    /// RFC 6121 thread identifier for this message.
    pub thread_id: Option<String>,
    /// XEP-0201 nested-thread parent (the `parent` attribute on
    /// `<thread/>`). Only meaningful when `thread_id` is `Some`.
    /// Cleared on XEP-0424 / XEP-0425 tombstones — see the
    /// [`ArchivedRichPayload::Tombstone`] doc comment for the full
    /// list of leak-prone fields.
    ///
    /// Typed as `ThreadId` (the existing newtype) per the
    /// typed-payloads hard rule for new struct fields. The companion
    /// `thread_id: Option<String>` field stays untyped here for now;
    /// #228 will collapse both into a single `thread:
    /// Option<xep0201::ThreadInfo>` field when the broader
    /// retyping of `ArchivedMessage` lands.
    #[serde(default)]
    pub parent_thread_id: Option<ThreadId>,
    /// XEP-0461 reply target message ID.
    pub reply_to_id: Option<String>,
    /// XEP-0461 optional original sender JID.
    pub reply_to_jid: Option<String>,
    /// XEP-0359 origin-id supplied by client.
    pub origin_id: Option<String>,
    /// Message type ("chat", "groupchat", "normal", etc.).
    #[serde(default = "default_message_type")]
    pub message_type: String,
    /// Preserved full stanza XML for faithful replay of archived timeline events.
    pub stanza_xml: Option<String>,
    /// Typed rich-message payload and annotations used to reconstruct XMPP payloads.
    pub rich: Option<ArchivedRichMessage>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's MUC nickname
    /// at archive-write time. Only set for `groupchat` rows; `None`
    /// otherwise. Used to disallow corrections across leave/rejoin
    /// cycles — the correction handler refuses if the room's current
    /// generation for the same nickname has advanced.
    #[serde(default)]
    pub nickname_generation: Option<u64>,
}

fn default_message_type() -> String {
    "chat".to_string()
}

impl Default for ArchivedMessage {
    fn default() -> Self {
        Self {
            id: String::new(),
            timestamp: Utc::now(),
            from: String::new(),
            to: String::new(),
            body: String::new(),
            stanza_id: None,
            thread_id: None,
            parent_thread_id: None,
            reply_to_id: None,
            reply_to_jid: None,
            origin_id: None,
            message_type: default_message_type(),
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
    /// Filter by sender.
    pub with: Option<String>,
    /// Filter by Waddle thread root id.
    pub thread_id: Option<ThreadId>,
    /// XEP-0431 full-text search terms.
    pub fulltext: Option<RichText>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Pagination: before this ID.
    pub before_id: Option<String>,
    /// Pagination: after this ID.
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
                .attr("var", WADDLE_MAM_THREAD_FIELD)
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "fulltext")
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
pub fn build_result_messages(
    query_id: &str,
    to_jid: &str,
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

/// Add a stanza-id extension to a message for MAM compliance.
pub fn add_stanza_id(message: &mut Message, archive_id: &str, by: &str) {
    let stanza_id = Element::builder("stanza-id", STANZA_ID_NS)
        .attr("id", archive_id)
        .attr("by", by)
        .build();
    message.payloads.push(stanza_id);
}

fn parse_data_form(form: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for field in form.children() {
        if field.name() != "field" {
            continue;
        }

        let var = field.attr("var").unwrap_or("");
        let value = field
            .children()
            .find(|c| c.name() == "value")
            .map(|value| value.text());

        match var {
            "" | "FORM_TYPE" => {}
            "start" => {
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.start = Some(parse_datetime(&value)?);
                }
            }
            "end" => {
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.end = Some(parse_datetime(&value)?);
                }
            }
            "with" => {
                query.with = value.filter(|value| !value.is_empty());
            }
            WADDLE_MAM_THREAD_FIELD => {
                query.thread_id = value.and_then(ThreadId::new);
            }
            "fulltext" => {
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

fn build_result_message(query_id: &str, to_jid: &str, archived: &ArchivedMessage) -> Message {
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

    let mut msg = Message::new(Some(parse_message_jid(to_jid)));
    msg.id = Some(Uuid::now_v7().to_string());
    msg.type_ = MessageType::Normal;
    msg.payloads.push(result);
    msg
}

fn parse_message_jid(to_jid: &str) -> Jid {
    to_jid
        .parse()
        .unwrap_or_else(|_| Jid::from(BareJid::new("unknown").expect("valid fallback JID")))
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

fn normalize_archived_inner_message(element: Element, archived: &ArchivedMessage) -> Element {
    if archived.message_type != "groupchat" {
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

    if let Some(thread_id) = archived.thread_id.as_deref() {
        let info = crate::xep0201::ThreadInfo {
            id: thread_id.to_owned(),
            parent: archived
                .parent_thread_id
                .as_ref()
                .map(|parent| parent.as_str().to_owned()),
        };
        crate::xep0201::install_thread_element(&mut normalized, &info);
    }

    normalized
}

fn build_typed_inner_message(archived: &ArchivedMessage, rich: &ArchivedRichMessage) -> Element {
    let msg_type = if archived.message_type.is_empty() {
        "chat"
    } else {
        archived.message_type.as_str()
    };

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", &archived.from)
        .attr("type", msg_type);
    if msg_type != "groupchat" {
        builder = builder.attr("to", &archived.to);
    }

    if let Some(stanza_id) = archived.stanza_id.as_deref() {
        builder = builder.attr("id", stanza_id);
    }
    if !archived.body.is_empty() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(archived.body.clone())
                .build(),
        );
    }
    // XEP-0201: emit `<thread parent='X'>id</thread>` via the canonical
    // typed builder so the optional parent attribute round-trips on
    // replay. The "parent without id ⇒ no `<thread/>` element" rule is
    // enforced by gating the entire emission on `thread_id.is_some()`:
    // a row with `parent_thread_id == Some(_)` and `thread_id == None`
    // is incoherent (RFC 6121 §5.2.5 — `parent` is meaningful only as a
    // back-reference from a thread that has its own id) and produces
    // no `<thread/>` element at all.
    if let Some(thread_id) = archived.thread_id.as_deref() {
        let info = crate::xep0201::ThreadInfo {
            id: thread_id.to_owned(),
            parent: archived
                .parent_thread_id
                .as_ref()
                .map(|t| t.as_str().to_owned()),
        };
        builder = builder.append(crate::xep0201::build_thread_element(&info, CLIENT_NS));
    }
    if let Some(origin_id) = archived.origin_id.as_deref() {
        builder = builder.append(
            Element::builder("origin-id", STANZA_ID_NS)
                .attr("id", origin_id)
                .build(),
        );
    }
    if msg_type == "groupchat" && !archived.id.is_empty() && !archived.to.is_empty() {
        builder = builder.append(
            Element::builder("stanza-id", STANZA_ID_NS)
                .attr("id", &archived.id)
                .attr("by", &archived.to)
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
    let msg_type = if archived.message_type.is_empty() {
        "chat"
    } else {
        archived.message_type.as_str()
    };

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", &archived.from)
        .attr("type", msg_type);
    if msg_type != "groupchat" {
        builder = builder.attr("to", &archived.to);
    }

    if let Some(stanza_id) = archived.stanza_id.as_deref() {
        builder = builder.attr("id", stanza_id);
    }
    if !archived.body.is_empty() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(archived.body.clone())
                .build(),
        );
    }
    // XEP-0201: same emission rule as `build_typed_inner_message`. Use
    // the canonical typed builder so parent round-trips, and gate on
    // `thread_id.is_some()` so a parent-only row never emits a stray
    // `<thread/>` element (RFC 6121 §5.2.5 incoherence guard).
    if let Some(thread_id) = archived.thread_id.as_deref() {
        let info = crate::xep0201::ThreadInfo {
            id: thread_id.to_owned(),
            parent: archived
                .parent_thread_id
                .as_ref()
                .map(|t| t.as_str().to_owned()),
        };
        builder = builder.append(crate::xep0201::build_thread_element(&info, CLIENT_NS));
    }
    if let Some(reply_to_id) = archived.reply_to_id.as_deref() {
        let mut reply = Element::builder("reply", REPLY_NS).attr("id", reply_to_id);
        if let Some(reply_to_jid) = archived.reply_to_jid.as_deref() {
            reply = reply.attr("to", reply_to_jid);
        }
        builder = builder.append(reply.build());
    }
    if let Some(origin_id) = archived.origin_id.as_deref() {
        builder = builder.append(
            Element::builder("origin-id", STANZA_ID_NS)
                .attr("id", origin_id)
                .build(),
        );
    }
    if msg_type == "groupchat" && !archived.id.is_empty() && !archived.to.is_empty() {
        builder = builder.append(
            Element::builder("stanza-id", STANZA_ID_NS)
                .attr("id", &archived.id)
                .attr("by", &archived.to)
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
        assert_eq!(query.with.as_deref(), Some("juliet@example.com"));
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
        assert!(fields.contains(&WADDLE_MAM_THREAD_FIELD));
        assert!(fields.contains(&"fulltext"));
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
            timestamp: Utc::now(),
            from: "user@example.com/nick".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello, world!".to_string(),
            thread_id: Some("thread-1".to_string()),
            reply_to_id: Some("parent-1".to_string()),
            reply_to_jid: Some("alice@example.com".to_string()),
            origin_id: Some("origin-1".to_string()),
            ..Default::default()
        };

        let msg = build_result_messages("query-1", "user@example.com", &[archived]);
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
            timestamp: Utc::now(),
            from: "alice@example.com/web".to_string(),
            to: "bob@example.com".to_string(),
            body: "nested reply".to_string(),
            stanza_id: Some("wire-id-1".to_string()),
            thread_id: Some("child-thread".to_string()),
            parent_thread_id: ThreadId::new("root-thread"),
            origin_id: None,
            message_type: "chat".to_string(),
            stanza_xml,
            ..Default::default()
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
        let msgs = build_result_messages("q1", "user@example.com", &[archived]);
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
        let msgs = build_result_messages("q2", "user@example.com", &[archived]);
        let thread = replay_inner_thread(&msgs[0]);
        assert_eq!(thread.text().trim(), "child-thread");
        assert_eq!(thread.attr("parent"), Some("root-thread"));
    }

    #[test]
    fn xep_0201_groupchat_stanza_xml_replay_reinstalls_thread_and_strips_to() {
        let archived = ArchivedMessage {
            id: "archive-threaded-reply".to_string(),
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "threaded reply".to_string(),
            stanza_id: Some("wire-id-2".to_string()),
            thread_id: Some("root-thread".to_string()),
            parent_thread_id: ThreadId::new("parent-thread"),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='bob@example.com/web' type='groupchat' id='wire-id-2'><body>threaded reply</body><thread>stale-thread</thread><reply xmlns='urn:xmpp:reply:0' id='root-thread'/></message>"
                    .to_string(),
            ),
            ..Default::default()
        };

        let msgs = build_result_messages("q-stanza-xml", "bob@example.com/web", &[archived]);
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
            timestamp: Utc::now(),
            from: "alice@example.com/web".to_string(),
            to: "bob@example.com".to_string(),
            body: "body".to_string(),
            thread_id: None,
            parent_thread_id: ThreadId::new("root-thread"),
            message_type: "chat".to_string(),
            stanza_xml: None,
            rich: None,
            ..Default::default()
        };
        let msgs = build_result_messages("q3", "user@example.com", &[archived]);
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
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: String::new(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='reaction-1'><reactions xmlns='urn:xmpp:reactions:0' id='msg-1'><reaction>👍</reaction></reactions></message>".to_string(),
            ),
            ..Default::default()
        };

        let msg = build_result_messages("query-2", "user@example.com", &[archived]);
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
            from: Some(parse_message_jid("romeo@example.com/orchard")),
            to: Some(parse_message_jid("juliet@example.com/balcony")),
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
    fn adds_stanza_id_payload() {
        let mut message = Message::new(Some(parse_message_jid("juliet@example.com")));

        add_stanza_id(&mut message, "archive-1", "room@example.com");

        let stanza_id = message
            .payloads
            .iter()
            .find(|payload| payload.name() == "stanza-id" && payload.ns() == STANZA_ID_NS)
            .expect("stanza-id payload");
        assert_eq!(stanza_id.attr("id"), Some("archive-1"));
        assert_eq!(stanza_id.attr("by"), Some("room@example.com"));
    }

    #[test]
    fn stanza_xml_preferred_over_rich_payload() {
        let archived = ArchivedMessage {
            id: "msg-priority".to_string(),
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "typed body".to_string(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' type='groupchat' id='live-1'><body>live body with extension payload</body><item xmlns='urn:example:task-widget:1' id='task-123' status='open'/></message>".to_string(),
            ),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: None,
                references: vec![],
                mentions: vec![],
            }),
            ..Default::default()
        };

        let msg = build_result_messages("query-priority", "user@example.com", &[archived]);
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
            timestamp: Utc::now(),
            from: "room@conference.example.com/bob".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "> Alice wrote:\n> Hello!\nI agree!".to_string(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/bob' type='groupchat' id='reply-1'><body>&gt; Alice wrote:\n&gt; Hello!\nI agree!</body><reply xmlns='urn:xmpp:reply:0' id='orig-1' to='room@conference.example.com/alice'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='22'/></fallback></message>".to_string(),
            ),
            reply_to_id: Some("orig-1".to_string()),
            reply_to_jid: Some("room@conference.example.com/alice".to_string()),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: Some(ArchivedReply {
                    id: RichMessageId("orig-1".to_string()),
                    to: Some("room@conference.example.com/alice".parse().unwrap()),
                }),
                references: vec![],
                mentions: vec![],
            }),
            ..Default::default()
        };

        let msg = build_result_messages("query-fallback", "user@example.com", &[archived]);
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
            timestamp: Utc::now(),
            from: "room@conference.example.com/alice".to_string(),
            to: "room@conference.example.com".to_string(),
            body: "Hello!".to_string(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='msg-1'><body>Hello!</body></message>".to_string(),
            ),
            ..Default::default()
        };

        let msg = build_result_messages("query-strip-to", "user@example.com", &[archived]);
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
            timestamp: Utc::now(),
            from: "room@conf.example.com/alice".to_string(),
            to: "room@conf.example.com".to_string(),
            body: "test".to_string(),
            message_type: "groupchat".to_string(),
            stanza_xml: Some(stanza_xml.to_string()),
            ..Default::default()
        };

        let msg = build_result_messages("query-strip-raw", "user@example.com", &[archived]);
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
