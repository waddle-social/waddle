//! Inbound/outbound XMPP messaging, MUC, and presence operations.
//!
//! Exposes a [`parse`] function for runtime dispatch and typed outbound stanza
//! builders. With the `native` feature enabled, also exposes a convenience
//! trait for sending those stanzas through the native client handle.

use chrono::{DateTime, Utc};
use minidom::Element;
use uuid::Uuid;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;
use crate::xep::{reply as xep_reply, thread as xep_thread};

// ─── Namespace constants ───────────────────────────────────────────────────

const NS_DELAY: &str = "urn:xmpp:delay";
const NS_STANZA_ID: &str = "urn:xmpp:sid:0";
const NS_ORIGIN_ID: &str = "urn:xmpp:origin-id:0";
pub const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
const NS_MARKUP: &str = "urn:xmpp:markup:0";
const NS_WADDLE_MARKUP: &str = "urn:waddle:markup:0";
pub const NS_CHAT_STATES: &str = "http://jabber.org/protocol/chatstates";
pub const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";
const NS_REFERENCES: &str = "urn:xmpp:reference:0";
pub const NS_MESSAGE_RETRACT: &str = "urn:xmpp:message-retract:0";
pub const NS_MESSAGE_MODERATE: &str = "urn:xmpp:message-moderate:0";
pub const NS_MESSAGE_CORRECT: &str = "urn:xmpp:message-correct:0";
const NS_FASTEN: &str = "urn:xmpp:fasten:0";
const NS_HINTS: &str = "urn:xmpp:hints";
const NS_HATS: &str = "urn:xmpp:hats:0";
const NS_SIMS: &str = "urn:xmpp:sims:1";
const NS_SFS: &str = "urn:xmpp:sfs:0";
const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";
const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";
const NS_CLIENT: &str = "jabber:client";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_STICKERS: &str = "urn:xmpp:stickers:0";
const NS_VCARD_UPDATE: &str = "vcard-temp:x:update";

// ─── Inbound types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct MarkupSpan {
    pub span_type: MarkupSpanType,
    pub start: usize,
    pub end: usize,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkupSpanType {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Blockquote,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharedFileDisposition {
    Inline,
    #[default]
    Attachment,
}

impl SharedFileDisposition {
    pub fn from_text_or_infer(value: Option<&str>, media_type: Option<&str>) -> Self {
        match value {
            Some("inline") => Self::Inline,
            Some("attachment") => Self::Attachment,
            _ => Self::infer_from_media_type(media_type),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Attachment => "attachment",
        }
    }

    fn infer_from_media_type(media_type: Option<&str>) -> Self {
        if media_type.is_some_and(|m| {
            m.starts_with("image/")
                || m.starts_with("video/")
                || m.starts_with("audio/")
                || m == "application/pdf"
        }) {
            Self::Inline
        } else {
            Self::Attachment
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    pub url: String,
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub disposition: SharedFileDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatStatePayload {
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayedMarkerPayload {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionPayload {
    pub target_id: String,
    pub emojis: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionPayload {
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationPayload {
    pub target_id: String,
    pub moderated_by: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionPayload {
    pub replaces_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundMessage {
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub id: Option<String>,
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub thread: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub moderation_target_id: Option<String>,
    pub moderated_by: Option<String>,
    pub moderation_reason: Option<String>,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<String>,
    /// XEP-0428 fallback range, in Unicode scalar offsets, end exclusive.
    pub reply_fallback: Option<(u32, u32)>,
    pub markup_spans: Vec<MarkupSpan>,
    pub chat_state: Option<String>,
    pub displayed_marker_id: Option<String>,
    pub shared_files: Vec<SharedFile>,
    pub broadcast_mention: Option<String>,
    pub mention_uris: Vec<String>,
    /// XEP-0372 references attached to this message. Populated for *every*
    /// `<reference xmlns="urn:xmpp:reference:0"/>` child, regardless of type
    /// (`mention`, `data`, or any other), as long as `type` and `uri` are
    /// present (both required by XEP-0372).
    pub references: Vec<ReferenceData>,
    pub forum_post_kind: Option<String>,
    pub forum_title: Option<String>,
    pub thread_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub is_sticker: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceHat {
    pub uri: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucAffiliation {
    Owner,
    Admin,
    Member,
    Outcast,
    None,
}

impl MucAffiliation {
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            "outcast" => Some(Self::Outcast),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Outcast => "outcast",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucRole {
    Moderator,
    Participant,
    Visitor,
    None,
}

impl MucRole {
    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "moderator" => Some(Self::Moderator),
            "participant" => Some(Self::Participant),
            "visitor" => Some(Self::Visitor),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Moderator => "moderator",
            Self::Participant => "participant",
            Self::Visitor => "visitor",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundPresence {
    pub from: Option<String>,
    pub to: Option<String>,
    pub presence_type: Option<String>,
    pub status: Option<String>,
    pub show: Option<String>,
    pub hats: Vec<PresenceHat>,
    pub muc_affiliation: Option<MucAffiliation>,
    pub muc_role: Option<MucRole>,
    pub muc_jid: Option<String>,
    pub vcard_avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessagingEvent {
    Message(Box<InboundMessage>),
    Presence(InboundPresence),
}

// ─── Outbound options ────────────────────────────────────────────────────

/// Options attached to an outbound chat or groupchat message, carrying
/// typed XEP payloads. Protocol values flow through typed XEP structs —
/// never raw `String` blobs — per the typed-payloads hard rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendMessageOptions {
    /// Caller-supplied stanza id for optimistic UI / delivery correlation.
    /// Generated by the client when omitted.
    pub stanza_id: Option<StanzaId>,
    /// Optional RFC 6121 `<subject/>`, used by forum topic posts.
    pub subject: Option<String>,
    /// XEP-0461 reply marker identifying the quoted message.
    pub reply: Option<xep_reply::ReplyMarker>,
    /// XEP-0428 fallback range over the body identifying the quoted prefix.
    /// Offsets count Unicode scalar values and `end` is exclusive.
    pub fallback: Option<xep_reply::FallbackRange>,
    /// XEP-0201 thread reference (with optional parent for nested threads).
    pub thread: Option<xep_thread::ThreadRef>,
    /// XEP-0394 message styling spans.
    pub markup_spans: Vec<MarkupSpanData>,
    /// XEP-0372 references such as mentions.
    pub references: Vec<ReferenceData>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<SharedFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkupSpanData {
    pub span_type: String,
    pub start: u32,
    pub end: u32,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceData {
    pub ref_type: String,
    pub uri: String,
    pub begin: u32,
    pub end: u32,
    /// XEP-0372 optional `anchor` attribute. Carries the original, unresolved
    /// display text (or arbitrary anchor URI) when offsets cannot be relied on.
    pub anchor: Option<String>,
}

// ─── Parse entry point ────────────────────────────────────────────────────

/// Parse an XMPP element into a [`MessagingEvent`], or return `None` if the
/// element is not a `<message>` or `<presence>`.
pub fn parse(element: &Element) -> Option<MessagingEvent> {
    match element.name() {
        "message" => Some(MessagingEvent::Message(Box::new(parse_message(element)))),
        "presence" => Some(MessagingEvent::Presence(parse_presence(element))),
        _ => None,
    }
}

// ─── Message parsing ──────────────────────────────────────────────────────

fn parse_message(el: &Element) -> InboundMessage {
    let id = el.attr("id").map(String::from);
    let from = el.attr("from").map(String::from);
    let to = el.attr("to").map(String::from);
    let message_type = el.attr("type").unwrap_or("normal").to_string();

    let body = el.get_child("body", NS_CLIENT).map(|e| e.text());
    let subject = el.get_child("subject", NS_CLIENT).map(|e| e.text());
    let thread = el.get_child("thread", NS_CLIENT).map(|e| e.text());

    let timestamp = el
        .get_child("delay", NS_DELAY)
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let stanza_id = el
        .get_child("stanza-id", NS_STANZA_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let origin_id = el
        .get_child("origin-id", NS_ORIGIN_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let correction = parse_correction_payload(el);
    let replaces_id = correction
        .as_ref()
        .map(|payload| payload.replaces_id.clone());

    let moderation = parse_moderation_payload(el);
    let moderation_target_id = moderation.as_ref().map(|payload| payload.target_id.clone());
    let moderated_by = moderation
        .as_ref()
        .map(|payload| payload.moderated_by.clone());
    let moderation_reason = moderation
        .as_ref()
        .and_then(|payload| payload.reason.clone());

    let retraction = parse_retraction_payload(el);
    let retracts_id = moderation_target_id
        .clone()
        .or_else(|| retraction.as_ref().map(|payload| payload.target_id.clone()));

    let reaction = parse_reaction_payload(el);
    let reaction_target_id = reaction.as_ref().map(|payload| payload.target_id.clone());
    let reaction_emojis = reaction.map(|payload| payload.emojis).unwrap_or_default();

    let reply_marker = xep_reply::parse_reply(el);
    let reply_to_id = reply_marker.as_ref().map(|m| m.id.clone());
    let reply_to_sender = reply_marker.as_ref().map(|m| m.to.to_string());
    let reply_fallback = xep_reply::parse_fallback(el).map(|r| (r.start, r.end));

    let markup_spans = el
        .get_child("markup", NS_MARKUP)
        .map(parse_markup_spans)
        .unwrap_or_default();

    let chat_state = parse_chat_state_payload(el).map(|payload| payload.state);
    let displayed_marker_id = parse_displayed_marker_payload(el).map(|payload| payload.id);

    // XEP-0372: References (mentions and data)
    //
    // Every `<reference xmlns="urn:xmpp:reference:0"/>` child with the required
    // `type` and `uri` attributes is captured as a typed `ReferenceData` and
    // also fans out into the flat helper views (`mention_uris`,
    // `broadcast_mention`, `shared_files`) the rest of the codebase already
    // consumes. The flat views are derived projections; `references` is the
    // structural source of truth. Reading `begin`/`end` is the only string→u32
    // parse on the inbound path; per the typed-payloads hard rule, no other
    // boundary may stringify protocol values.
    let mut references: Vec<ReferenceData> = Vec::new();
    let mut mention_uris: Vec<String> = Vec::new();
    let mut broadcast_mention: Option<String> = None;
    let mut shared_files: Vec<SharedFile> = Vec::new();

    for child in el
        .children()
        .filter(|c| c.name() == "reference" && c.ns() == NS_REFERENCES)
    {
        let ref_type = match child.attr("type") {
            Some(t) => t,
            None => continue,
        };
        let uri = match child.attr("uri") {
            Some(u) => u,
            None => continue,
        };
        // begin/end are optional per XEP-0372, but they form an all-or-nothing
        // pair: a reference either points at a body substring (both present
        // and numeric) or it is anchor-only (both absent → represented as the
        // (0, 0) sentinel). A half-specified pair like `begin="3"` with no
        // `end` is meaningless — drop it. Same for malformed values like
        // `begin="abc"`, which would otherwise mis-position the span.
        let begin_attr = child.attr("begin");
        let end_attr = child.attr("end");
        let (begin, end) = match (begin_attr, end_attr) {
            (Some(b), Some(e)) => match (b.parse::<u32>(), e.parse::<u32>()) {
                (Ok(b), Ok(e)) if e >= b => (b, e),
                _ => continue,
            },
            (None, None) => (0, 0),
            _ => continue,
        };
        let anchor = child.attr("anchor").map(String::from);

        references.push(ReferenceData {
            ref_type: ref_type.to_string(),
            uri: uri.to_string(),
            begin,
            end,
            anchor: anchor.clone(),
        });

        match ref_type {
            "mention" => {
                let uri_str = uri.to_string();
                if uri_str.starts_with("xmpp:")
                    && (uri_str.contains("@everyone") || uri_str.contains("@here"))
                {
                    broadcast_mention = Some(uri_str.clone());
                }
                mention_uris.push(uri_str);
            }
            "data" => {
                if let Some(file) = parse_shared_file(child) {
                    shared_files.push(file);
                }
            }
            _ => {}
        }
    }

    for file_sharing_el in el
        .children()
        .filter(|c| c.name() == "file-sharing" && c.ns() == NS_SFS)
    {
        if let Some(file) = parse_file_sharing_element(file_sharing_el) {
            shared_files.push(file);
        }
    }

    // XEP-0447 / XEP-0363: also check <sims> children for file sharing
    for sims_el in el.children().filter(|c| c.ns() == NS_SIMS) {
        for source_el in sims_el.children() {
            for url_data_el in source_el.children() {
                let url = url_data_el.attr("url").map(String::from);
                if let Some(url) = url {
                    shared_files.push(SharedFile {
                        url,
                        name: sims_el
                            .get_child("name", sims_el.ns().as_str())
                            .map(|e| e.text()),
                        media_type: sims_el
                            .get_child("media-type", sims_el.ns().as_str())
                            .map(|e| e.text()),
                        size: sims_el
                            .get_child("size", sims_el.ns().as_str())
                            .and_then(|e| e.text().parse().ok()),
                        width: None,
                        height: None,
                        disposition: SharedFileDisposition::Attachment,
                    });
                }
            }
        }
    }

    // XEP-0201 thread + nested thread parent
    let thread_ref = xep_thread::parse_thread(el);
    let thread_id = thread_ref
        .as_ref()
        .map(|t| t.id.clone())
        .or_else(|| thread.clone());
    let parent_thread_id = thread_ref.as_ref().and_then(|t| t.parent.clone());
    let (forum_post_kind, forum_title) =
        if thread_id.is_some() && body.is_some() && subject.is_some() {
            (Some("topic".to_string()), subject.clone())
        } else if thread_id.is_some() && body.is_some() {
            (Some("reply".to_string()), None)
        } else {
            (None, None)
        };

    // XEP-0449: Stickers
    let is_sticker = el.get_child("sticker", NS_STICKERS).is_some();

    InboundMessage {
        from,
        to,
        message_type,
        id,
        stanza_id,
        origin_id,
        body,
        subject,
        thread,
        timestamp,
        replaces_id,
        retracts_id,
        moderation_target_id,
        moderated_by,
        moderation_reason,
        reaction_target_id,
        reaction_emojis,
        reply_to_id,
        reply_to_sender,
        reply_fallback,
        markup_spans,
        chat_state,
        displayed_marker_id,
        shared_files,
        broadcast_mention,
        mention_uris,
        references,
        forum_post_kind,
        forum_title,
        thread_id,
        parent_thread_id,
        is_sticker,
    }
}

fn validate_chat_state(state: &str) -> ClientResult<&str> {
    match state {
        "active" | "composing" | "paused" | "inactive" | "gone" => Ok(state),
        _ => Err(ClientError::Core(waddle_xmpp_core::CoreError::bad_request(
            Some(format!("invalid chat state `{state}`")),
        ))),
    }
}

pub fn parse_chat_state_payload(element: &Element) -> Option<ChatStatePayload> {
    element
        .children()
        .find(|child| child.ns() == NS_CHAT_STATES)
        .and_then(|child| validate_chat_state(child.name()).ok())
        .map(|state| ChatStatePayload {
            state: state.to_string(),
        })
}

pub fn parse_displayed_marker_payload(element: &Element) -> Option<DisplayedMarkerPayload> {
    element
        .get_child("displayed", NS_CHAT_MARKERS)
        .and_then(|child| child.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| DisplayedMarkerPayload { id: id.to_string() })
}

pub fn parse_reaction_payload(element: &Element) -> Option<ReactionPayload> {
    let reactions = element.get_child("reactions", NS_REACTIONS)?;
    let target_id = reactions.attr("id")?.trim();
    if target_id.is_empty() {
        return None;
    }
    let emojis = reactions
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == NS_REACTIONS)
        .map(|child| child.text())
        .filter(|emoji| !emoji.is_empty())
        .collect();
    Some(ReactionPayload {
        target_id: target_id.to_string(),
        emojis,
    })
}

pub fn parse_retraction_payload(element: &Element) -> Option<RetractionPayload> {
    element
        .get_child("retract", NS_MESSAGE_RETRACT)
        .and_then(|child| child.attr("id"))
        .or_else(|| {
            element
                .get_child("retracted", NS_MESSAGE_RETRACT)
                .and_then(|child| child.attr("id"))
        })
        .filter(|id| !id.is_empty())
        .map(|id| RetractionPayload {
            target_id: id.to_string(),
        })
}

pub fn parse_moderation_payload(element: &Element) -> Option<ModerationPayload> {
    let apply_to = element.get_child("apply-to", NS_FASTEN)?;
    let target_id = apply_to.attr("id")?.trim();
    if target_id.is_empty() {
        return None;
    }
    let moderated = apply_to.get_child("moderated", NS_MESSAGE_MODERATE)?;
    moderated.get_child("retract", NS_MESSAGE_RETRACT)?;
    let moderated_by = moderated.attr("by").unwrap_or_default().to_string();
    let reason = moderated
        .get_child("reason", NS_MESSAGE_MODERATE)
        .map(|child| child.text())
        .filter(|text| !text.trim().is_empty());
    Some(ModerationPayload {
        target_id: target_id.to_string(),
        moderated_by,
        reason,
    })
}

pub fn parse_correction_payload(element: &Element) -> Option<CorrectionPayload> {
    element
        .get_child("replace", NS_MESSAGE_CORRECT)
        .and_then(|child| child.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| CorrectionPayload {
            replaces_id: id.to_string(),
        })
}

fn parse_markup_spans(markups_el: &Element) -> Vec<MarkupSpan> {
    // XEP-0394: direct children of <markup> are:
    //   <span start="..." end="..."><emphasis/|<strong/>|<code/>|<deleted/></span>  — inline (NS_MARKUP)
    //   <span start="..." end="..." uri="..."/>                                     — link (NS_WADDLE_MARKUP)
    //   <bcode start="..." end="..."/>                                              — code block (NS_MARKUP)
    //   <bquote start="..." end="..."/>                                             — blockquote (NS_MARKUP)
    markups_el
        .children()
        .filter_map(|child| match child.name() {
            "span" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                // Link: <span uri="..."/> with no inline child element
                if let Some(uri) = child.attr("uri") {
                    return Some(MarkupSpan {
                        span_type: MarkupSpanType::Link,
                        start,
                        end,
                        uri: Some(uri.to_string()),
                    });
                }
                // Inline markup: inspect the single child element
                let span_type = child.children().find_map(|inner| match inner.name() {
                    "strong" => Some(MarkupSpanType::Bold),
                    "emphasis" => Some(MarkupSpanType::Italic),
                    "deleted" => Some(MarkupSpanType::Strikethrough),
                    "code" => Some(MarkupSpanType::Code),
                    _ => None,
                })?;
                Some(MarkupSpan {
                    span_type,
                    start,
                    end,
                    uri: None,
                })
            }
            "bcode" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                Some(MarkupSpan {
                    span_type: MarkupSpanType::CodeBlock,
                    start,
                    end,
                    uri: None,
                })
            }
            "bquote" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                Some(MarkupSpan {
                    span_type: MarkupSpanType::Blockquote,
                    start,
                    end,
                    uri: None,
                })
            }
            _ => None,
        })
        .collect()
}

fn parse_shared_file(reference_el: &Element) -> Option<SharedFile> {
    // Look for nested file metadata; structure varies by implementation.
    // Try XEP-0447 <sources> / <url-data> layout first.
    let mut url: Option<String> = None;
    let mut name: Option<String> = None;
    let mut media_type: Option<String> = None;
    let mut size: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut disposition: Option<String> = None;

    for child in reference_el.children() {
        match child.name() {
            "url-data" => {
                url = child.attr("target").map(String::from);
            }
            "file" => {
                name = child
                    .get_child("name", child.ns().as_str())
                    .map(|e| e.text());
                media_type = child
                    .get_child("media-type", child.ns().as_str())
                    .map(|e| e.text());
                size = child
                    .get_child("size", child.ns().as_str())
                    .and_then(|e| e.text().parse().ok());
                if let Some(thumb) = child.get_child("thumbnail", child.ns().as_str()) {
                    width = thumb.attr("width").and_then(|v| v.parse().ok());
                    height = thumb.attr("height").and_then(|v| v.parse().ok());
                }
                if let Some(disp) = child.get_child("disposition", child.ns().as_str()) {
                    disposition = Some(disp.text());
                }
            }
            // Simple <url> child fallback
            "url" => {
                url = Some(child.text());
            }
            _ => {}
        }
    }

    let disposition =
        SharedFileDisposition::from_text_or_infer(disposition.as_deref(), media_type.as_deref());
    url.map(|u| SharedFile {
        url: u,
        name,
        media_type,
        size,
        width,
        height,
        disposition,
    })
}

fn parse_file_sharing_element(file_sharing_el: &Element) -> Option<SharedFile> {
    let mut url: Option<String> = None;
    let mut name: Option<String> = None;
    let mut media_type: Option<String> = None;
    let mut size: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let disposition_attr = file_sharing_el.attr("disposition");

    if let Some(file_el) = file_sharing_el.get_child("file", NS_FILE_METADATA) {
        name = file_el
            .get_child("name", NS_FILE_METADATA)
            .map(|e| e.text());
        media_type = file_el
            .get_child("media-type", NS_FILE_METADATA)
            .map(|e| e.text());
        size = file_el
            .get_child("size", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
        width = file_el
            .get_child("width", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
        height = file_el
            .get_child("height", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
    }

    if let Some(sources_el) = file_sharing_el.get_child("sources", NS_SFS) {
        url = sources_el
            .get_child("url-data", NS_URL_DATA)
            .and_then(|e| e.attr("target"))
            .map(String::from);
    }

    let disposition =
        SharedFileDisposition::from_text_or_infer(disposition_attr, media_type.as_deref());
    url.map(|u| SharedFile {
        url: u,
        name,
        media_type,
        size,
        width,
        height,
        disposition,
    })
}

// ─── Presence parsing ─────────────────────────────────────────────────────

fn parse_presence(el: &Element) -> InboundPresence {
    let from = el.attr("from").map(String::from);
    let to = el.attr("to").map(String::from);
    let presence_type = el.attr("type").map(String::from);
    let status = el.get_child("status", NS_CLIENT).map(|e| e.text());
    let show = el.get_child("show", NS_CLIENT).map(|e| e.text());

    // XEP-0317: Hats
    let hats = el
        .get_child("hats", NS_HATS)
        .map(|hats_el| {
            hats_el
                .children()
                .filter(|c| c.name() == "hat")
                .filter_map(|hat| {
                    let uri = hat.attr("uri")?.to_string();
                    let title = hat.attr("title")?.to_string();
                    Some(PresenceHat { uri, title })
                })
                .collect()
        })
        .unwrap_or_default();
    let muc_item = el
        .get_child("x", NS_MUC_USER)
        .and_then(|x| x.get_child("item", NS_MUC_USER));
    let muc_affiliation = muc_item
        .and_then(|item| item.attr("affiliation"))
        .and_then(MucAffiliation::from_attr);
    let muc_role = muc_item
        .and_then(|item| item.attr("role"))
        .and_then(MucRole::from_attr);
    let muc_jid = muc_item
        .and_then(|item| item.attr("jid"))
        .map(str::to_string);
    let vcard_avatar = el
        .get_child("x", NS_VCARD_UPDATE)
        .and_then(|x| x.get_child("photo", NS_VCARD_UPDATE))
        .map(|photo| photo.text())
        .filter(|hash| !hash.is_empty());

    InboundPresence {
        from,
        to,
        presence_type,
        status,
        show,
        hats,
        muc_affiliation,
        muc_role,
        muc_jid,
        vcard_avatar,
    }
}

// ─── Outbound trait ───────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait MessagingExt {
    fn join_room<'a>(
        &'a self,
        room_jid: &'a str,
        nick: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn leave_room<'a>(
        &'a self,
        room_jid: &'a str,
        nick: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_groupchat_message<'a>(
        &'a self,
        room_jid: &'a str,
        body: &'a str,
        options: &'a SendMessageOptions,
    ) -> impl std::future::Future<Output = ClientResult<StanzaId>> + Send + 'a;
    fn send_chat_message<'a>(
        &'a self,
        peer_jid: &'a str,
        body: &'a str,
        options: &'a SendMessageOptions,
    ) -> impl std::future::Future<Output = ClientResult<StanzaId>> + Send + 'a;
    fn send_presence<'a>(
        &'a self,
        status: Option<&'a str>,
        show: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_chat_state<'a>(
        &'a self,
        jid: &'a str,
        state: &'a str,
        message_type: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn send_displayed_marker<'a>(
        &'a self,
        jid: &'a str,
        message_id: &'a str,
        message_type: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
    fn retract_message<'a>(
        &'a self,
        jid: &'a str,
        message_id: &'a str,
        message_type: &'a str,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl MessagingExt for ClientHandle {
    async fn join_room(&self, room_jid: &str, nick: &str) -> ClientResult<()> {
        let to = format!("{}/{}", room_jid, nick);
        let stanza = Element::builder("presence", NS_CLIENT)
            .attr("to", to.as_str())
            .append(Element::builder("x", NS_MUC).build())
            .build();
        self.send_stanza(stanza).await
    }

    async fn leave_room(&self, room_jid: &str, nick: &str) -> ClientResult<()> {
        let to = format!("{}/{}", room_jid, nick);
        let stanza = Element::builder("presence", NS_CLIENT)
            .attr("to", to.as_str())
            .attr("type", "unavailable")
            .build();
        self.send_stanza(stanza).await
    }

    async fn send_groupchat_message(
        &self,
        room_jid: &str,
        body: &str,
        options: &SendMessageOptions,
    ) -> ClientResult<StanzaId> {
        let (stanza_id, stanza) = build_outbound_message(room_jid, "groupchat", body, options)?;
        self.send_stanza(stanza).await?;
        Ok(stanza_id)
    }

    async fn send_chat_message(
        &self,
        peer_jid: &str,
        body: &str,
        options: &SendMessageOptions,
    ) -> ClientResult<StanzaId> {
        let (stanza_id, stanza) = build_outbound_message(peer_jid, "chat", body, options)?;
        self.send_stanza(stanza).await?;
        Ok(stanza_id)
    }

    async fn send_presence(&self, status: Option<&str>, show: Option<&str>) -> ClientResult<()> {
        let mut builder = Element::builder("presence", NS_CLIENT);
        if let Some(s) = status {
            builder = builder.append(Element::builder("status", NS_CLIENT).append(s).build());
        }
        if let Some(s) = show {
            builder = builder.append(Element::builder("show", NS_CLIENT).append(s).build());
        }
        self.send_stanza(builder.build()).await
    }

    async fn send_chat_state(
        &self,
        jid: &str,
        state: &str,
        message_type: &str,
    ) -> ClientResult<()> {
        let stanza = build_chat_state_message(jid, state, message_type)?;
        self.send_stanza(stanza).await
    }

    async fn send_displayed_marker(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
    ) -> ClientResult<()> {
        let stanza = build_displayed_message(jid, message_id, message_type);
        self.send_stanza(stanza).await
    }

    async fn retract_message(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
    ) -> ClientResult<()> {
        let stanza = build_retraction_message(jid, message_type, message_id);
        self.send_stanza(stanza).await
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

pub fn build_chat_state_message(
    to: &str,
    state: &str,
    message_type: &str,
) -> ClientResult<Element> {
    let state = validate_chat_state(state)?;
    Ok(Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .append(Element::builder(state, NS_CHAT_STATES).build())
        .build())
}

pub fn build_displayed_message(to: &str, message_id: &str, message_type: &str) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .append(
            Element::builder("displayed", NS_CHAT_MARKERS)
                .attr("id", message_id)
                .build(),
        )
        .build()
}

pub fn build_reaction_message(
    to: &str,
    message_type: &str,
    target_id: &str,
    emojis: &[String],
) -> Element {
    let mut reactions = Element::builder("reactions", NS_REACTIONS)
        .attr("id", target_id)
        .build();
    for emoji in emojis {
        reactions.append_child(
            Element::builder("reaction", NS_REACTIONS)
                .append(emoji.as_str())
                .build(),
        );
    }

    Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .attr("id", Uuid::new_v4().to_string())
        .append(reactions)
        .append(Element::builder("store", NS_HINTS).build())
        .build()
}

pub fn build_retraction_message(to: &str, message_type: &str, retracts_id: &str) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .attr("id", Uuid::new_v4().to_string())
        .append(
            Element::builder("retract", NS_MESSAGE_RETRACT)
                .attr("id", retracts_id)
                .build(),
        )
        .append(
            Element::builder("body", NS_CLIENT)
                .append("This person attempted to retract a previous message.")
                .build(),
        )
        .append(Element::builder("store", NS_HINTS).build())
        .build()
}

pub fn build_moderation_message(
    to: &str,
    message_type: &str,
    target_id: &str,
    reason: Option<&str>,
) -> Element {
    let mut moderated = Element::builder("moderated", NS_MESSAGE_MODERATE)
        .append(Element::builder("retract", NS_MESSAGE_RETRACT).build());
    if let Some(reason) = reason {
        moderated = moderated.append(
            Element::builder("reason", NS_MESSAGE_MODERATE)
                .append(reason)
                .build(),
        );
    }

    Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .attr("id", Uuid::new_v4().to_string())
        .append(
            Element::builder("apply-to", NS_FASTEN)
                .attr("id", target_id)
                .append(moderated.build())
                .build(),
        )
        .append(Element::builder("store", NS_HINTS).build())
        .build()
}

pub fn build_correction_message(
    to: &str,
    message_type: &str,
    body: &str,
    replaces_id: &str,
    options: &SendMessageOptions,
) -> ClientResult<(StanzaId, Element)> {
    let (stanza_id, mut stanza) = build_outbound_message(to, message_type, body, options)?;
    stanza.append_child(
        Element::builder("replace", NS_MESSAGE_CORRECT)
            .attr("id", replaces_id)
            .build(),
    );
    Ok((stanza_id, stanza))
}

/// Build a `<message/>` stanza carrying the body plus any XEP payloads from
/// `options`. All XML construction goes through typed `minidom::Element`
/// builders — never `format!` — per the project XML hard rule.
pub fn build_outbound_message(
    to: &str,
    message_type: &str,
    body: &str,
    options: &SendMessageOptions,
) -> ClientResult<(StanzaId, Element)> {
    let stanza_id = match options.stanza_id.clone() {
        Some(stanza_id) => stanza_id,
        None => StanzaId::new(Uuid::new_v4().to_string())?,
    };
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr("to", to)
        .attr("type", message_type)
        .attr("id", stanza_id.as_str())
        .append(Element::builder("body", NS_CLIENT).append(body).build());

    if let Some(subject) = options.subject.as_deref() {
        builder = builder.append(
            Element::builder("subject", NS_CLIENT)
                .append(subject)
                .build(),
        );
    }
    if let Some(marker) = options.reply.as_ref() {
        builder = builder.append(xep_reply::build_reply_element(marker));
    }
    if let Some(range) = options.fallback.as_ref() {
        builder = builder.append(xep_reply::build_fallback_element(range));
    }
    if let Some(thread) = options.thread.as_ref() {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    if !options.markup_spans.is_empty() {
        let mut markups = Element::builder("markup", NS_MARKUP).build();
        for span in &options.markup_spans {
            // XEP-0394: inline spans use <span start="..." end="..."><child/></span>;
            // block-level elements (bcode, bquote) are siblings of span, not wrapped in it;
            // links use <span start="..." end="..." uri="..."/> with no child element.
            let el = match span.span_type.as_str() {
                "bold" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr("start", span.start.to_string())
                        .attr("end", span.end.to_string())
                        .build();
                    el.append_child(Element::builder("strong", NS_MARKUP).build());
                    el
                }
                "italic" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr("start", span.start.to_string())
                        .attr("end", span.end.to_string())
                        .build();
                    el.append_child(Element::builder("emphasis", NS_MARKUP).build());
                    el
                }
                "strikethrough" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr("start", span.start.to_string())
                        .attr("end", span.end.to_string())
                        .build();
                    el.append_child(Element::builder("deleted", NS_MARKUP).build());
                    el
                }
                "code" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr("start", span.start.to_string())
                        .attr("end", span.end.to_string())
                        .build();
                    el.append_child(Element::builder("code", NS_MARKUP).build());
                    el
                }
                // Block-level: <bcode start="..." end="..."/> — sibling of <span>, never wrapped
                "code_block" => Element::builder("bcode", NS_MARKUP)
                    .attr("start", span.start.to_string())
                    .attr("end", span.end.to_string())
                    .build(),
                // Block-level: <bquote start="..." end="..."/> — sibling of <span>, never wrapped
                "blockquote" => Element::builder("bquote", NS_MARKUP)
                    .attr("start", span.start.to_string())
                    .attr("end", span.end.to_string())
                    .build(),
                // Link: <span start="..." end="..." uri="..."/> in urn:waddle:markup:0 —
                // XEP-0394 does not define a link span; use custom namespace to avoid
                // polluting the official markup namespace.
                "link" => {
                    let mut b = Element::builder("span", NS_WADDLE_MARKUP)
                        .attr("start", span.start.to_string())
                        .attr("end", span.end.to_string());
                    if let Some(uri) = &span.uri {
                        b = b.attr("uri", uri);
                    }
                    b.build()
                }
                _ => continue,
            };
            markups.append_child(el);
        }
        builder = builder.append(markups);
    }
    for reference in &options.references {
        // XEP-0372 §2.1: `begin` and `end` are OPTIONAL. They MUST be present
        // when the reference points at a substring of the body, and MUST be
        // absent when no body position applies (e.g. anchor-only references
        // to a previous message). Treat (0, 0) as the "no position" sentinel
        // so anchor-only / future use cases can travel cleanly on the wire
        // without emitting `begin="0" end="0"` (which a conformant receiver
        // would interpret as a 0-length annotation at body offset 0).
        let mut ref_builder = Element::builder("reference", NS_REFERENCES)
            .attr("type", reference.ref_type.as_str())
            .attr("uri", reference.uri.as_str());
        if reference.begin != 0 || reference.end != 0 {
            ref_builder = ref_builder
                .attr("begin", reference.begin.to_string())
                .attr("end", reference.end.to_string());
        }
        if let Some(anchor) = reference.anchor.as_deref() {
            ref_builder = ref_builder.attr("anchor", anchor);
        }
        builder = builder.append(ref_builder.build());
    }
    for file in &options.shared_files {
        builder = builder.append(build_file_sharing_element(file));
    }
    Ok((stanza_id, builder.build()))
}

pub fn build_file_sharing_element(file: &SharedFile) -> Element {
    let mut file_sharing = Element::builder("file-sharing", NS_SFS)
        .attr("disposition", file.disposition.as_str())
        .build();

    let mut metadata = Element::builder("file", NS_FILE_METADATA).build();
    if let Some(media_type) = file.media_type.as_deref() {
        metadata.append_child(
            Element::builder("media-type", NS_FILE_METADATA)
                .append(media_type)
                .build(),
        );
    }
    if let Some(name) = file.name.as_deref() {
        metadata.append_child(
            Element::builder("name", NS_FILE_METADATA)
                .append(name)
                .build(),
        );
    }
    if let Some(size) = file.size {
        metadata.append_child(
            Element::builder("size", NS_FILE_METADATA)
                .append(size.to_string())
                .build(),
        );
    }
    if let Some(width) = file.width {
        metadata.append_child(
            Element::builder("width", NS_FILE_METADATA)
                .append(width.to_string())
                .build(),
        );
    }
    if let Some(height) = file.height {
        metadata.append_child(
            Element::builder("height", NS_FILE_METADATA)
                .append(height.to_string())
                .build(),
        );
    }
    file_sharing.append_child(metadata);

    let mut sources = Element::builder("sources", NS_SFS).build();
    sources.append_child(
        Element::builder("url-data", NS_URL_DATA)
            .attr("target", file.url.as_str())
            .build(),
    );
    file_sharing.append_child(sources);
    file_sharing
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn el(xml: &str) -> Element {
        xml.parse().expect("invalid XML")
    }

    #[test]
    fn parse_message_with_body() {
        let e = el("<message xmlns='jabber:client' \
             from='room@conf.example/alice' \
             to='bob@example.com' \
             type='groupchat' \
             id='msg-1'>\
             <body>Hello world</body>\
             </message>");
        let ev = parse(&e).unwrap();
        let MessagingEvent::Message(msg) = ev else {
            panic!("expected Message");
        };
        assert_eq!(msg.id.as_deref(), Some("msg-1"));
        assert_eq!(msg.from.as_deref(), Some("room@conf.example/alice"));
        assert_eq!(msg.body.as_deref(), Some("Hello world"));
        assert_eq!(msg.message_type, "groupchat");
    }

    #[test]
    fn parse_message_with_delay() {
        use chrono::Datelike;
        let e = el("<message xmlns='jabber:client' type='groupchat' id='m2'>\
             <body>delayed</body>\
             <delay xmlns='urn:xmpp:delay' stamp='2024-01-15T10:00:00Z'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        let ts = msg.timestamp.unwrap();
        assert_eq!(ts.year(), 2024);
        assert_eq!(ts.month(), 1);
        assert_eq!(ts.day(), 15);
    }

    #[test]
    fn parse_message_with_stanza_id() {
        let e = el("<message xmlns='jabber:client' type='groupchat' id='m3'>\
             <body>hi</body>\
             <stanza-id xmlns='urn:xmpp:sid:0' id='server-sid-42' by='room@conf.example'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.stanza_id.as_deref(), Some("server-sid-42"));
    }

    #[test]
    fn parse_message_with_chat_state() {
        let e = el(
            "<message xmlns='jabber:client' type='chat' to='alice@example.com'>\
             <composing xmlns='http://jabber.org/protocol/chatstates'/>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.chat_state.as_deref(), Some("composing"));
    }

    #[test]
    fn parse_message_with_displayed_marker() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <displayed xmlns='urn:xmpp:chat-markers:0' id='orig-msg-id'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.displayed_marker_id.as_deref(), Some("orig-msg-id"));
    }

    #[test]
    fn parse_message_with_reply() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <body>&gt; [quoted]\nresponse</body>\
             <reply xmlns='urn:xmpp:reply:0' id='target-id' to='sender@example.com'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.reply_to_id.as_deref(), Some("target-id"));
        assert_eq!(msg.reply_to_sender.as_deref(), Some("sender@example.com"));
        assert!(msg.reply_fallback.is_none());
    }

    #[test]
    fn parse_message_with_reply_and_fallback() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <body>&gt; alice wrote:\n&gt; original message\n\nmy response</body>\
             <reply xmlns='urn:xmpp:reply:0' id='orig-id' to='alice@example.com'/>\
             <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                 <body start='0' end='37'/>\
             </fallback>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.reply_to_id.as_deref(), Some("orig-id"));
        assert_eq!(msg.reply_to_sender.as_deref(), Some("alice@example.com"));
        assert_eq!(msg.reply_fallback, Some((0, 37)));
    }

    #[test]
    fn parse_message_with_nested_thread() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <body>nested reply</body>\
             <thread parent='root-abc'>child-xyz</thread>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.thread_id.as_deref(), Some("child-xyz"));
        assert_eq!(msg.parent_thread_id.as_deref(), Some("root-abc"));
    }

    #[test]
    fn parse_message_with_retract() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <retract xmlns='urn:xmpp:message-retract:0' id='old-msg-id'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.retracts_id.as_deref(), Some("old-msg-id"));
    }

    #[test]
    fn parse_message_with_correction() {
        let e = el("<message xmlns='jabber:client' type='chat'>\
             <body>fixed</body>\
             <replace xmlns='urn:xmpp:message-correct:0' id='old-msg-id'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.replaces_id.as_deref(), Some("old-msg-id"));
    }

    #[test]
    fn parse_message_with_moderation() {
        let e = el(
            "<message xmlns='jabber:client' type='groupchat' from='room@muc.example'>\
             <apply-to xmlns='urn:xmpp:fasten:0' id='old-msg-id'>\
                 <moderated xmlns='urn:xmpp:message-moderate:0'>\
                     <retract xmlns='urn:xmpp:message-retract:0'/>\
                     <reason>cleanup</reason>\
                 </moderated>\
             </apply-to>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.retracts_id.as_deref(), Some("old-msg-id"));
        assert_eq!(msg.moderation_target_id.as_deref(), Some("old-msg-id"));
        assert_eq!(msg.moderated_by.as_deref(), Some(""));
        assert_eq!(msg.moderation_reason.as_deref(), Some("cleanup"));
    }

    #[test]
    fn parse_message_with_reactions() {
        let e = el("<message xmlns='jabber:client' type='groupchat'>\
             <reactions xmlns='urn:xmpp:reactions:0' id='target-msg'>\
             <reaction>👍</reaction>\
             <reaction>❤️</reaction>\
             </reactions>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.reaction_target_id.as_deref(), Some("target-msg"));
        assert_eq!(msg.reaction_emojis, vec!["👍", "❤️"]);
    }

    #[test]
    fn parse_message_extracts_references_for_data_type() {
        let e = el(
            "<message xmlns='jabber:client' type='groupchat' id='m-data'>\
               <body>see https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com' begin='4' end='23' \
                  anchor='https://example.com'/>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(
            msg.references,
            vec![ReferenceData {
                ref_type: "data".to_string(),
                uri: "https://example.com".to_string(),
                begin: 4,
                end: 23,
                anchor: Some("https://example.com".to_string()),
            }]
        );
        // `type=data` references that don't carry XEP-0446/0447 file metadata
        // must not pollute the shared_files projection.
        assert!(msg.shared_files.is_empty());
        // `type=data` references must not appear as mentions.
        assert!(msg.mention_uris.is_empty());
    }

    #[test]
    fn parse_message_extracts_references_for_mention_type() {
        let e = el(
            "<message xmlns='jabber:client' type='groupchat' id='m-mention'>\
               <body>hi @bob</body>\
               <reference xmlns='urn:xmpp:reference:0' type='mention' \
                  uri='xmpp:bob@example.com' begin='3' end='7'/>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(
            msg.references,
            vec![ReferenceData {
                ref_type: "mention".to_string(),
                uri: "xmpp:bob@example.com".to_string(),
                begin: 3,
                end: 7,
                anchor: None,
            }]
        );
        // Mentions still also flow through the flat helper view.
        assert_eq!(msg.mention_uris, vec!["xmpp:bob@example.com".to_string()]);
    }

    #[test]
    fn parse_message_extracts_references_for_unknown_type() {
        let e = el(
            "<message xmlns='jabber:client' type='groupchat' id='m-unknown'>\
               <body>see https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='quote' \
                  uri='xmpp:room@conf.example?message;id=abc' begin='0' end='3'/>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.references.len(), 1);
        assert_eq!(msg.references[0].ref_type, "quote");
        assert!(msg.mention_uris.is_empty());
        assert!(msg.shared_files.is_empty());
    }

    #[test]
    fn parse_message_reference_handles_missing_optional_attrs() {
        let e = el("<message xmlns='jabber:client' type='chat' id='m-min'>\
               <body>https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(
            msg.references,
            vec![ReferenceData {
                ref_type: "data".to_string(),
                uri: "https://example.com".to_string(),
                begin: 0,
                end: 0,
                anchor: None,
            }]
        );
    }

    #[test]
    fn parse_message_reference_skipped_when_begin_or_end_unparseable() {
        // XEP-0372 says begin/end are optional, but if present they MUST be
        // numeric. Silently coercing "abc" to 0 would mis-position the
        // highlighted span — drop the reference instead.
        let e = el(
            "<message xmlns='jabber:client' type='chat' id='m-bad-offsets'>\
               <body>https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com' begin='abc' end='5'/>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://other.example' begin='0' end='not-a-number'/>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert!(msg.references.is_empty());
    }

    #[test]
    fn parse_message_reference_skipped_when_only_one_of_begin_end_is_present() {
        // begin/end form an all-or-nothing pair under XEP-0372: either both
        // are present (the reference points at a body substring) or both are
        // absent (anchor-only). A half-specified `begin="3"` with no `end` is
        // meaningless and would silently coerce `end` to 0, putting `end`
        // before `begin` — drop the reference instead.
        let e = el("<message xmlns='jabber:client' type='chat' id='m-half'>\
               <body>see https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com' begin='4'/>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://other.example' end='10'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert!(msg.references.is_empty());
    }

    #[test]
    fn parse_message_reference_skipped_when_required_attr_missing() {
        // XEP-0372 requires both `type` and `uri`. References missing either
        // must be silently dropped — never returned with placeholder values.
        let e = el("<message xmlns='jabber:client' type='chat' id='m-bad'>\
               <body>https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data'/>\
               <reference xmlns='urn:xmpp:reference:0' uri='https://example.com'/>\
             </message>");
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert!(msg.references.is_empty());
    }

    #[test]
    fn build_outbound_message_omits_begin_end_when_no_position() {
        // XEP-0372 says begin/end are optional. Anchor-only references that
        // do not point at a body substring must NOT carry begin="0" end="0"
        // — that would be interpreted as a real 0-length annotation at
        // offset 0 by a conformant receiver.
        let options = SendMessageOptions {
            references: vec![ReferenceData {
                ref_type: "data".to_string(),
                uri: "xmpp:room@conf.example?message;id=earlier-msg".to_string(),
                begin: 0,
                end: 0,
                anchor: Some("xmpp:alice@example.com".to_string()),
            }],
            ..Default::default()
        };

        let (_, stanza) =
            build_outbound_message("room@muc.example", "groupchat", "see above", &options).unwrap();

        let reference = stanza
            .get_child("reference", NS_REFERENCES)
            .expect("reference child");
        assert_eq!(reference.attr("type"), Some("data"));
        assert_eq!(
            reference.attr("uri"),
            Some("xmpp:room@conf.example?message;id=earlier-msg")
        );
        assert_eq!(reference.attr("anchor"), Some("xmpp:alice@example.com"));
        assert_eq!(reference.attr("begin"), None);
        assert_eq!(reference.attr("end"), None);
    }

    #[test]
    fn build_outbound_message_emits_anchor_attribute_when_present() {
        let options = SendMessageOptions {
            references: vec![ReferenceData {
                ref_type: "data".to_string(),
                uri: "https://example.com".to_string(),
                begin: 4,
                end: 23,
                anchor: Some("example.com".to_string()),
            }],
            ..Default::default()
        };

        let (_, stanza) = build_outbound_message(
            "room@muc.example",
            "groupchat",
            "see https://example.com",
            &options,
        )
        .unwrap();

        let reference = stanza
            .get_child("reference", NS_REFERENCES)
            .expect("reference child");
        assert_eq!(reference.attr("anchor"), Some("example.com"));
        assert_eq!(reference.attr("type"), Some("data"));
        assert_eq!(reference.attr("uri"), Some("https://example.com"));
    }

    #[test]
    fn parse_message_with_direct_file_sharing() {
        let e = el(
            "<message xmlns='jabber:client' type='chat'>\
               <body>https://files.example.com/report.pdf</body>\
               <file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>\
                 <file xmlns='urn:xmpp:file:metadata:0'>\
                   <media-type>application/pdf</media-type>\
                   <name>report.pdf</name>\
                   <size>4096</size>\
                 </file>\
                 <sources>\
                   <url-data xmlns='http://jabber.org/protocol/url-data' target='https://files.example.com/report.pdf'/>\
                 </sources>\
               </file-sharing>\
             </message>",
        );
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.shared_files.len(), 1);
        assert_eq!(
            msg.shared_files[0].url,
            "https://files.example.com/report.pdf"
        );
        assert_eq!(msg.shared_files[0].name.as_deref(), Some("report.pdf"));
        assert_eq!(
            msg.shared_files[0].media_type.as_deref(),
            Some("application/pdf")
        );
        assert_eq!(msg.shared_files[0].size, Some(4096));
        assert_eq!(
            msg.shared_files[0].disposition,
            SharedFileDisposition::Inline
        );
    }

    #[test]
    fn parse_presence_available() {
        let e = el("<presence xmlns='jabber:client' from='alice@example.com'>\
             <show>away</show>\
             <status>Out for lunch</status>\
             </presence>");
        let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
            panic!("expected Presence");
        };
        assert_eq!(p.show.as_deref(), Some("away"));
        assert_eq!(p.status.as_deref(), Some("Out for lunch"));
        assert!(p.presence_type.is_none());
    }

    #[test]
    fn parse_presence_unavailable() {
        let e = el("<presence xmlns='jabber:client' \
             from='alice@example.com' \
             type='unavailable'/>");
        let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
            panic!("expected Presence");
        };
        assert_eq!(p.presence_type.as_deref(), Some("unavailable"));
    }

    #[test]
    fn parse_presence_with_hats() {
        let e = el("<presence xmlns='jabber:client' from='admin@room.example'>\
             <hats xmlns='urn:xmpp:hats:0'>\
             <hat uri='urn:example:hats:admin' title='Administrator'/>\
             <hat uri='urn:example:hats:mod' title='Moderator'/>\
             </hats>\
             </presence>");
        let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
            panic!("expected Presence");
        };
        assert_eq!(p.hats.len(), 2);
        assert_eq!(p.hats[0].uri, "urn:example:hats:admin");
        assert_eq!(p.hats[0].title, "Administrator");
        assert_eq!(p.hats[1].title, "Moderator");
    }

    #[test]
    fn parse_presence_with_muc_affiliation_and_role() {
        let e = el(
            "<presence xmlns='jabber:client' from='room@muc.example/alice'>\
             <x xmlns='http://jabber.org/protocol/muc#user'>\
             <item affiliation='owner' role='moderator' jid='alice@example.com/phone'/>\
             </x>\
             <x xmlns='vcard-temp:x:update'>\
             <photo>room-avatar-hash</photo>\
             </x>\
             </presence>",
        );
        let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
            panic!("expected Presence");
        };
        assert_eq!(p.muc_affiliation, Some(MucAffiliation::Owner));
        assert_eq!(p.muc_role, Some(MucRole::Moderator));
        assert_eq!(p.muc_jid.as_deref(), Some("alice@example.com/phone"));
        assert_eq!(p.vcard_avatar.as_deref(), Some("room-avatar-hash"));
    }

    #[test]
    fn build_outbound_message_with_shared_files() {
        let options = SendMessageOptions {
            shared_files: vec![SharedFile {
                url: "https://files.example.com/song.ogg".to_string(),
                name: Some("song.ogg".to_string()),
                media_type: Some("audio/ogg".to_string()),
                size: Some(1234),
                width: None,
                height: None,
                disposition: SharedFileDisposition::Inline,
            }],
            ..Default::default()
        };
        let (stanza_id, stanza) =
            build_outbound_message("alice@example.com", "chat", "listen", &options).unwrap();
        assert_eq!(stanza.attr("id"), Some(stanza_id.as_str()));
        let file_sharing = stanza
            .get_child("file-sharing", NS_SFS)
            .expect("file-sharing child");
        assert_eq!(file_sharing.attr("disposition"), Some("inline"));
        let file = file_sharing
            .get_child("file", NS_FILE_METADATA)
            .expect("metadata child");
        assert_eq!(
            file.get_child("media-type", NS_FILE_METADATA)
                .map(|e| e.text()),
            Some("audio/ogg".to_string())
        );
        assert_eq!(
            file_sharing
                .get_child("sources", NS_SFS)
                .and_then(|sources| sources.get_child("url-data", NS_URL_DATA))
                .and_then(|url_data| url_data.attr("target")),
            Some("https://files.example.com/song.ogg")
        );
    }

    #[test]
    fn build_outbound_message_uses_caller_stanza_id() {
        let stanza_id = StanzaId::new("client-visible-1").unwrap();
        let options = SendMessageOptions {
            stanza_id: Some(stanza_id.clone()),
            ..Default::default()
        };

        let (returned_id, stanza) =
            build_outbound_message("room@muc.example", "groupchat", "hello", &options).unwrap();

        assert_eq!(returned_id, stanza_id);
        assert_eq!(stanza.attr("id"), Some("client-visible-1"));
    }

    #[test]
    fn build_outbound_message_with_markup_spans_and_references() {
        let options = SendMessageOptions {
            markup_spans: vec![
                MarkupSpanData {
                    span_type: "bold".to_string(),
                    start: 0,
                    end: 5,
                    uri: None,
                },
                // XEP-0394: links use <span uri="..."/> attribute, no child element
                MarkupSpanData {
                    span_type: "link".to_string(),
                    start: 6,
                    end: 10,
                    uri: Some("xmpp:bob@example.com".to_string()),
                },
            ],
            references: vec![ReferenceData {
                ref_type: "mention".to_string(),
                uri: "xmpp:bob@example.com".to_string(),
                begin: 6,
                end: 10,
                anchor: None,
            }],
            ..Default::default()
        };

        let (_, stanza) =
            build_outbound_message("room@muc.example", "groupchat", "hello @bob", &options)
                .unwrap();

        let markups = stanza
            .get_child("markup", NS_MARKUP)
            .expect("markups child");
        let spans = markups
            .children()
            .filter(|child| child.name() == "span")
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 2);
        // Bold: <span start="0" end="5"><strong/></span>
        assert!(spans[0].get_child("strong", NS_MARKUP).is_some());
        // Link: <span start="6" end="10" uri="xmpp:bob@example.com"/> in urn:waddle:markup:0
        assert_eq!(spans[1].ns(), NS_WADDLE_MARKUP);
        assert_eq!(spans[1].attr("uri"), Some("xmpp:bob@example.com"));
        assert!(spans[1].get_child("link", NS_MARKUP).is_none());
        let reference = stanza
            .get_child("reference", NS_REFERENCES)
            .expect("reference child");
        assert_eq!(reference.attr("type"), Some("mention"));
        assert_eq!(reference.attr("begin"), Some("6"));
        assert_eq!(reference.attr("end"), Some("10"));
        assert_eq!(reference.attr("uri"), Some("xmpp:bob@example.com"));
    }

    #[test]
    fn build_outbound_message_block_markup_uses_xep0394_elements() {
        // XEP-0394: code blocks use <bcode/> and blockquotes use <bquote/> as
        // siblings of <span>, not wrapped inside a <span> element.
        let options = SendMessageOptions {
            markup_spans: vec![
                MarkupSpanData {
                    span_type: "code_block".to_string(),
                    start: 0,
                    end: 20,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "blockquote".to_string(),
                    start: 21,
                    end: 45,
                    uri: None,
                },
            ],
            ..Default::default()
        };

        let (_, stanza) = build_outbound_message(
            "room@muc.example",
            "groupchat",
            "```code``` > quote",
            &options,
        )
        .unwrap();

        let markups = stanza
            .get_child("markup", NS_MARKUP)
            .expect("markups child");
        // Block elements must NOT be wrapped in <span>
        assert_eq!(
            markups.children().filter(|c| c.name() == "span").count(),
            0,
            "code_block and blockquote must not be wrapped in <span>"
        );
        // <bcode start="0" end="20"/>
        let bcode = markups
            .get_child("bcode", NS_MARKUP)
            .expect("bcode element");
        assert_eq!(bcode.attr("start"), Some("0"));
        assert_eq!(bcode.attr("end"), Some("20"));
        // <bquote start="21" end="45"/>
        let bquote = markups
            .get_child("bquote", NS_MARKUP)
            .expect("bquote element");
        assert_eq!(bquote.attr("start"), Some("21"));
        assert_eq!(bquote.attr("end"), Some("45"));
    }

    #[test]
    fn xep0394_markup_roundtrip_parse_and_build() {
        // Build a message with all markup types, then parse the resulting stanza
        // and verify the roundtrip produces the same MarkupSpan values.
        let options = SendMessageOptions {
            markup_spans: vec![
                MarkupSpanData {
                    span_type: "bold".to_string(),
                    start: 0,
                    end: 4,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "italic".to_string(),
                    start: 5,
                    end: 9,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "strikethrough".to_string(),
                    start: 10,
                    end: 14,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "code".to_string(),
                    start: 15,
                    end: 19,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "code_block".to_string(),
                    start: 20,
                    end: 39,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "blockquote".to_string(),
                    start: 40,
                    end: 59,
                    uri: None,
                },
                MarkupSpanData {
                    span_type: "link".to_string(),
                    start: 60,
                    end: 64,
                    uri: Some("https://example.com".to_string()),
                },
            ],
            ..Default::default()
        };

        let (_, stanza) =
            build_outbound_message("alice@example.com", "chat", "hello world", &options).unwrap();

        // Parse back using parse_markup_spans
        let markups_el = stanza.get_child("markup", NS_MARKUP).expect("markups");
        let parsed = parse_markup_spans(markups_el);

        assert_eq!(parsed.len(), 7);
        assert!(matches!(parsed[0].span_type, MarkupSpanType::Bold));
        assert_eq!((parsed[0].start, parsed[0].end), (0, 4));
        assert!(matches!(parsed[1].span_type, MarkupSpanType::Italic));
        assert_eq!((parsed[1].start, parsed[1].end), (5, 9));
        assert!(matches!(parsed[2].span_type, MarkupSpanType::Strikethrough));
        assert_eq!((parsed[2].start, parsed[2].end), (10, 14));
        assert!(matches!(parsed[3].span_type, MarkupSpanType::Code));
        assert_eq!((parsed[3].start, parsed[3].end), (15, 19));
        assert!(matches!(parsed[4].span_type, MarkupSpanType::CodeBlock));
        assert_eq!((parsed[4].start, parsed[4].end), (20, 39));
        assert!(matches!(parsed[5].span_type, MarkupSpanType::Blockquote));
        assert_eq!((parsed[5].start, parsed[5].end), (40, 59));
        assert!(matches!(parsed[6].span_type, MarkupSpanType::Link));
        assert_eq!(parsed[6].uri.as_deref(), Some("https://example.com"));
        assert_eq!((parsed[6].start, parsed[6].end), (60, 64));
    }

    #[test]
    fn build_chat_state_message_validates_state() {
        let stanza = build_chat_state_message("alice@example.com", "composing", "chat")
            .expect("valid chat state");
        assert_eq!(stanza.attr("to"), Some("alice@example.com"));
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert!(stanza.get_child("composing", NS_CHAT_STATES).is_some());
        assert!(build_chat_state_message("alice@example.com", "typing", "chat").is_err());
    }

    #[test]
    fn build_displayed_message_has_expected_shape() {
        let stanza = build_displayed_message("room@muc.example", "msg-1", "groupchat");
        assert_eq!(stanza.attr("to"), Some("room@muc.example"));
        assert!(stanza.get_child("displayed", NS_CHAT_MARKERS).is_some());
        assert_eq!(
            stanza
                .get_child("displayed", NS_CHAT_MARKERS)
                .and_then(|child| child.attr("id")),
            Some("msg-1")
        );
    }

    #[test]
    fn build_correction_message_with_markup_spans() {
        let options = SendMessageOptions {
            markup_spans: vec![MarkupSpanData {
                span_type: "bold".to_string(),
                start: 0,
                end: 5,
                uri: None,
            }],
            references: vec![ReferenceData {
                ref_type: "mention".to_string(),
                uri: "xmpp:bob@example.com".to_string(),
                begin: 6,
                end: 10,
                anchor: None,
            }],
            ..Default::default()
        };

        let (_, stanza) = build_correction_message(
            "room@muc.example",
            "groupchat",
            "hello @bob",
            "orig-1",
            &options,
        )
        .expect("correction stanza");

        assert!(stanza.get_child("replace", NS_MESSAGE_CORRECT).is_some());
        assert!(stanza.get_child("markup", NS_MARKUP).is_some());
        assert!(stanza.get_child("reference", NS_REFERENCES).is_some());
    }

    #[test]
    fn build_reaction_message_has_expected_shape() {
        let emojis = vec!["👍".to_string(), "❤️".to_string()];
        let stanza = build_reaction_message("room@muc.example", "groupchat", "msg-1", &emojis);
        let reactions = stanza
            .get_child("reactions", NS_REACTIONS)
            .expect("reactions child");
        assert_eq!(reactions.attr("id"), Some("msg-1"));
        let values: Vec<String> = reactions
            .children()
            .filter(|child| child.name() == "reaction" && child.ns() == NS_REACTIONS)
            .map(|child| child.text())
            .collect();
        assert_eq!(values, vec!["👍", "❤️"]);
        assert!(stanza.get_child("store", NS_HINTS).is_some());
    }

    #[test]
    fn build_retraction_message_has_expected_shape() {
        let stanza = build_retraction_message("room@muc.example", "groupchat", "msg-1");
        assert_eq!(
            stanza
                .get_child("retract", NS_MESSAGE_RETRACT)
                .and_then(|child| child.attr("id")),
            Some("msg-1")
        );
        assert_eq!(
            stanza
                .get_child("body", NS_CLIENT)
                .map(|child| child.text()),
            Some("This person attempted to retract a previous message.".to_string())
        );
        assert!(stanza.get_child("store", NS_HINTS).is_some());
    }

    #[test]
    fn build_moderation_message_has_expected_shape() {
        let stanza =
            build_moderation_message("room@muc.example", "groupchat", "msg-1", Some("cleanup"));
        assert_eq!(stanza.attr("to"), Some("room@muc.example"));
        let apply_to = stanza
            .get_child("apply-to", NS_FASTEN)
            .expect("apply-to child");
        assert_eq!(apply_to.attr("id"), Some("msg-1"));
        let moderated = apply_to
            .get_child("moderated", NS_MESSAGE_MODERATE)
            .expect("moderated child");
        assert!(moderated.get_child("retract", NS_MESSAGE_RETRACT).is_some());
        assert_eq!(
            moderated
                .get_child("reason", NS_MESSAGE_MODERATE)
                .map(|child| child.text()),
            Some("cleanup".to_string())
        );
        assert!(stanza.get_child("store", NS_HINTS).is_some());
    }

    #[test]
    fn build_correction_message_has_expected_shape() {
        let (message_id, stanza) = build_correction_message(
            "alice@example.com",
            "chat",
            "fixed",
            "msg-1",
            &SendMessageOptions::default(),
        )
        .expect("correction stanza");
        assert_eq!(stanza.attr("to"), Some("alice@example.com"));
        assert_eq!(stanza.attr("id"), Some(message_id.as_str()));
        assert_eq!(
            stanza
                .get_child("body", NS_CLIENT)
                .map(minidom::Element::text),
            Some("fixed".to_string())
        );
        assert_eq!(
            stanza
                .get_child("replace", NS_MESSAGE_CORRECT)
                .and_then(|child: &minidom::Element| child.attr("id")),
            Some("msg-1")
        );
    }

    #[test]
    fn parse_ignores_iq() {
        let e = el("<iq xmlns='jabber:client' type='get' id='iq-1'/>");
        assert!(parse(&e).is_none());
    }
}
