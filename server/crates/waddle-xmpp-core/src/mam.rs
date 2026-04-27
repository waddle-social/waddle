//! Shared Message Archive Management (MAM) primitives and helpers.
//!
//! These types and builders are safe to share across server and client code.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use minidom::Element;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType, Thread};

use crate::xep::xep0359::{build_origin_id_element, build_stanza_id_element, OriginId};
use crate::{CoreError, CoreResult};

/// MAM XML namespace (XEP-0313 v2).
pub const MAM_NS: &str = "urn:xmpp:mam:2";

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
    /// leak-prone fields (`thread_id`, `reply_to_id`, mentions, ...)
    /// are cleared when this variant is set.
    Tombstone(ArchivedTombstone),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArchivedRichMessage {
    pub payload: Option<ArchivedRichPayload>,
    pub reply: Option<ArchivedReply>,
    pub references: Vec<ArchivedReference>,
    pub mentions: Vec<ArchivedMention>,
}

/// Archive-assigned unique identifier for a stored message row.
///
/// Per XEP-0313 §5.1.1 the archive emits this as the `id=` attribute of a
/// `<stanza-id by='archive-jid' id='archive-id'/>` element. Per
/// XEP-0359 §4 the value is an opaque non-empty string — Waddle's storage
/// generator emits UUID-v7 today, but the type only encodes the
/// XEP-required non-emptiness.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveId(String);

impl ArchiveId {
    /// Wrap a raw archive id, rejecting empty/whitespace strings.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ArchiveId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Archived message row at the storage boundary.
///
/// Protocol fields are typed per the project's typed-payloads rule. The two
/// serialization-boundary fields — `stanza_xml` and `rich` — retain string /
/// JSON shapes since they are themselves serialized blobs at the SQL layer.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    /// Archive-assigned unique identifier.
    pub id: ArchiveId,
    /// Timestamp when the message was archived.
    pub timestamp: DateTime<Utc>,
    /// Sender JID. Bare for direct, full (room+nick) for groupchat.
    pub from: Jid,
    /// Recipient/archive JID — always bare (room JID for MUC, contact bare
    /// for 1:1 personal archive).
    pub to: BareJid,
    /// Message body. `None` when absent on the wire (RFC 6121 §5.2.2 makes
    /// `<body/>` optional); always non-empty when present.
    pub body: Option<RichText>,
    /// RFC 6121 `<message id='...'>` attribute as supplied by the
    /// originating client (the wire stanza identifier). Distinct from
    /// XEP-0359 stanza-ids: the latter are server-assigned values that
    /// flow through the archive primary key (`id`) and are reconstructed
    /// in replay via `build_stanza_id_element(archived.id, archived.to)`.
    pub message_id: Option<RichMessageId>,
    /// RFC 6121 / XEP-0201 thread identifier. The optional `parent`
    /// attribute on `<thread/>` is currently dropped (tracked in #250).
    pub thread: Option<Thread>,
    /// XEP-0359 origin-id supplied by the originating client.
    pub origin_id: Option<OriginId>,
    /// XMPP message type (Chat / Groupchat / Normal / etc).
    pub message_type: MessageType,
    /// Preserved full stanza XML for faithful replay of archived timeline
    /// events. SQL byte-blob equivalent to wire bytes.
    pub stanza_xml: Option<String>,
    /// Typed rich-message payload and annotations used to reconstruct
    /// XMPP payloads. Reply target lives here (canonical), not in flat
    /// fields. Persisted as JSON in a single SQL column.
    pub rich: Option<ArchivedRichMessage>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's MUC nickname
    /// at archive-write time. Only set for `groupchat` rows; `None`
    /// otherwise. Used to disallow corrections across leave/rejoin
    /// cycles — the correction handler refuses if the room's current
    /// generation for the same nickname has advanced.
    pub nickname_generation: Option<u64>,
}

/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter.
    pub start: Option<DateTime<Utc>>,
    /// End time filter.
    pub end: Option<DateTime<Utc>>,
    /// XEP-0313 `<with/>` filter — JID (bare or full) of the counterpart.
    pub with: Option<Jid>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// RSM pagination: return rows whose archive id < this id, newest first.
    pub before_id: Option<ArchiveId>,
    /// RSM pagination: return rows whose archive id > this id.
    pub after_id: Option<ArchiveId>,
    /// XEP-0059 §2.5: empty `<before/>` element requests the last page of
    /// results. Set when the wire query contains a `<before/>` element with
    /// no text content; mutually informative with `before_id`.
    pub last_page: bool,
}

/// MAM query result.
#[derive(Debug, Clone)]
pub struct MamResult {
    /// Retrieved messages.
    pub messages: Vec<ArchivedMessage>,
    /// Whether there are more messages available.
    pub complete: bool,
    /// First archive id in the result set.
    pub first_id: Option<ArchiveId>,
    /// Last archive id in the result set.
    pub last_id: Option<ArchiveId>,
    /// Total count (if available).
    pub count: Option<u32>,
}

/// Parse a MAM query from an IQ stanza.
pub fn parse_mam_query(iq: &Iq) -> CoreResult<(String, MamQuery)> {
    let query_elem = match &iq.payload {
        IqType::Set(elem) | IqType::Get(elem) if elem.name() == "query" && elem.ns() == MAM_NS => {
            elem
        }
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

/// Check if an IQ is a MAM query.
pub fn is_mam_query(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Set(elem) | IqType::Get(elem)
            if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Build MAM result messages for each archived message.
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
        let value = field
            .children()
            .find(|c| c.name() == "value")
            .map(|value| value.text());

        match var {
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
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.with = Some(Jid::from_str(&value).map_err(|error| {
                        CoreError::bad_request(Some(format!("Invalid <with/> JID: {error}")))
                    })?);
                }
            }
            _ => {}
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
                if let Some(id) = ArchiveId::new(value) {
                    query.after_id = Some(id);
                }
            }
            "before" => {
                // XEP-0059 §2.5: a `<before/>` element with no text requests
                // the last page; with text it's a backward-from-cursor page.
                let value = child.text();
                if let Some(id) = ArchiveId::new(value) {
                    query.before_id = Some(id);
                } else {
                    query.last_page = true;
                }
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
        .attr("id", archived.id.as_str())
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

    let rich_default;
    let rich = match archived.rich.as_ref() {
        Some(rich) => rich,
        None => {
            rich_default = ArchivedRichMessage::default();
            &rich_default
        }
    };
    build_typed_inner_message(archived, rich)
}

fn normalize_archived_inner_message(element: Element, archived: &ArchivedMessage) -> Element {
    if archived.message_type != MessageType::Groupchat {
        return element;
    }

    if let Ok(mut message) = Message::try_from(element.clone()) {
        message.to = None;
        return Element::from(message);
    }

    if element.attr("to").is_none() {
        return element;
    }

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
}

fn message_type_attr(message_type: &MessageType) -> &'static str {
    match message_type {
        MessageType::Chat => "chat",
        MessageType::Error => "error",
        MessageType::Groupchat => "groupchat",
        MessageType::Headline => "headline",
        MessageType::Normal => "normal",
    }
}

fn build_typed_inner_message(archived: &ArchivedMessage, rich: &ArchivedRichMessage) -> Element {
    let msg_type = message_type_attr(&archived.message_type);

    let mut builder = Element::builder("message", CLIENT_NS)
        .attr("from", archived.from.to_string())
        .attr("type", msg_type);
    if archived.message_type != MessageType::Groupchat {
        builder = builder.attr("to", archived.to.to_string());
    }

    if let Some(message_id) = archived.message_id.as_ref() {
        builder = builder.attr("id", message_id.as_str());
    }
    if let Some(body) = archived.body.as_ref() {
        builder = builder.append(
            Element::builder("body", CLIENT_NS)
                .append(body.as_str())
                .build(),
        );
    }
    if let Some(thread) = archived.thread.as_ref() {
        builder = builder.append(
            Element::builder("thread", CLIENT_NS)
                .append(thread.0.as_str())
                .build(),
        );
    }
    if let Some(origin_id) = archived.origin_id.as_ref() {
        builder = builder.append(build_origin_id_element(origin_id.as_str()));
    }
    if archived.message_type == MessageType::Groupchat {
        builder = builder.append(build_stanza_id_element(archived.id.as_str(), &archived.to));
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

fn build_rsm_response_element(result: &MamResult) -> Element {
    let mut builder = Element::builder("set", RSM_NS);

    if let Some(first) = result.first_id.as_ref() {
        builder = builder.append(
            Element::builder("first", RSM_NS)
                .append(first.as_str())
                .build(),
        );
    }
    if let Some(last) = result.last_id.as_ref() {
        builder = builder.append(
            Element::builder("last", RSM_NS)
                .append(last.as_str())
                .build(),
        );
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

    fn archive_id(s: &str) -> ArchiveId {
        ArchiveId::new(s).expect("non-empty archive id")
    }

    fn rich_text(s: &str) -> RichText {
        RichText::new(s).expect("non-empty rich text")
    }

    fn jid(s: &str) -> Jid {
        Jid::from_str(s).expect("valid jid")
    }

    fn bare_jid(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
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
        assert_eq!(
            query.after_id.as_ref().map(ArchiveId::as_str),
            Some("msg-9"),
        );
        assert_eq!(
            query.with.as_ref().map(Jid::to_string).as_deref(),
            Some("juliet@example.com"),
        );
        let start = query.start.expect("start filter");
        assert_eq!(start.year(), 2024);
        assert_eq!(start.month(), 1);
        assert_eq!(start.day(), 15);
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

        assert_eq!(query.before_id, None);
        assert!(query.last_page);
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
    fn builds_result_message_from_typed_fields() {
        let archived = ArchivedMessage {
            id: archive_id("msg-123"),
            timestamp: Utc::now(),
            from: jid("user@example.com/nick"),
            to: bare_jid("room@conference.example.com"),
            body: Some(rich_text("Hello, world!")),
            message_id: None,
            thread: Some(Thread("thread-1".to_owned())),
            origin_id: OriginId::new("origin-1"),
            message_type: MessageType::Chat,
            stanza_xml: None,
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: Some(ArchivedReply {
                    id: RichMessageId("parent-1".to_string()),
                    to: Some(jid("alice@example.com")),
                }),
                references: vec![],
                mentions: vec![],
            }),
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-1", &to_jid, &[archived]);
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

    #[test]
    fn preserves_archived_stanza_payload() {
        let archived = ArchivedMessage {
            id: archive_id("msg-124"),
            timestamp: Utc::now(),
            from: jid("room@conference.example.com/alice"),
            to: bare_jid("room@conference.example.com"),
            body: None,
            message_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='reaction-1'><reactions xmlns='urn:xmpp:reactions:0' id='msg-1'><reaction>👍</reaction></reactions></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-2", &to_jid, &[archived]);
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
            first_id: Some(archive_id("msg-1")),
            last_id: Some(archive_id("msg-2")),
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
        use crate::xep::xep0359::add_stanza_id;

        let mut message = Message::new(Some(jid("juliet@example.com")));
        let by = bare_jid("room@example.com");

        add_stanza_id(&mut message, "archive-1", &by);

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
            id: archive_id("msg-priority"),
            timestamp: Utc::now(),
            from: jid("room@conference.example.com/alice"),
            to: bare_jid("room@conference.example.com"),
            body: Some(rich_text("typed body")),
            message_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' type='groupchat' id='live-1'><body>live body with embeds</body><github xmlns='urn:waddle:github:0'><repo url='https://github.com/example/project'><owner>example</owner><name>project</name></repo></github></message>".to_string(),
            ),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: None,
                references: vec![],
                mentions: vec![],
            }),
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-priority", &to_jid, &[archived]);
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
        assert_eq!(body.text(), "live body with embeds");

        let github = inner_msg
            .children()
            .find(|c| c.name() == "github" && c.ns() == "urn:waddle:github:0")
            .expect("github embed payload");
        assert!(github.children().any(|c| c.name() == "repo"));
    }

    #[test]
    fn stanza_xml_preserves_reply_fallback() {
        let archived = ArchivedMessage {
            id: archive_id("msg-fallback"),
            timestamp: Utc::now(),
            from: jid("room@conference.example.com/bob"),
            to: bare_jid("room@conference.example.com"),
            body: Some(rich_text("> Alice wrote:\n> Hello!\nI agree!")),
            message_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/bob' type='groupchat' id='reply-1'><body>&gt; Alice wrote:\n&gt; Hello!\nI agree!</body><reply xmlns='urn:xmpp:reply:0' id='orig-1' to='room@conference.example.com/alice'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='22'/></fallback></message>".to_string(),
            ),
            rich: Some(ArchivedRichMessage {
                payload: None,
                reply: Some(ArchivedReply {
                    id: RichMessageId("orig-1".to_string()),
                    to: Some(jid("room@conference.example.com/alice")),
                }),
                references: vec![],
                mentions: vec![],
            }),
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-fallback", &to_jid, &[archived]);
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
            id: archive_id("msg-strip-to"),
            timestamp: Utc::now(),
            from: jid("room@conference.example.com/alice"),
            to: bare_jid("room@conference.example.com"),
            body: Some(rich_text("Hello!")),
            message_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(
                "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='msg-1'><body>Hello!</body></message>".to_string(),
            ),
            rich: None,
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-strip-to", &to_jid, &[archived]);
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
            id: archive_id("msg-strip-raw"),
            timestamp: Utc::now(),
            from: jid("room@conf.example.com/alice"),
            to: bare_jid("room@conf.example.com"),
            body: Some(rich_text("test")),
            message_id: None,
            thread: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: Some(stanza_xml.to_string()),
            rich: None,
            nickname_generation: None,
        };

        let to_jid = jid("user@example.com");
        let msg = build_result_messages("query-strip-raw", &to_jid, &[archived]);
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
