//! Inbound/outbound XMPP messaging, MUC, and presence operations.
//!
//! Exposes a [`parse`] function for runtime dispatch and a [`MessagingExt`]
//! trait implemented on [`ClientHandle`] for all outbound operations.

use chrono::{DateTime, Utc};
use minidom::Element;
use uuid::Uuid;

use crate::client::ClientHandle;
use crate::error::ClientResult;
use crate::request::StanzaId;
use crate::xep::{reply as xep_reply, thread as xep_thread};

// ─── Namespace constants ───────────────────────────────────────────────────

const NS_DELAY: &str = "urn:xmpp:delay";
const NS_STANZA_ID: &str = "urn:xmpp:sid:0";
const NS_ORIGIN_ID: &str = "urn:xmpp:origin-id:0";
const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
const NS_MARKUP: &str = "urn:xmpp:markup:0";
const NS_CHAT_STATE: &str = "http://jabber.org/protocol/chatstates";
const NS_MARKERS: &str = "urn:xmpp:chat-markers:0";
const NS_REFERENCES: &str = "urn:xmpp:reference:0";
const NS_RETRACT: &str = "urn:xmpp:message-retract:0";
const NS_REPLACE: &str = "urn:xmpp:message-correct:0";
const NS_HATS: &str = "urn:xmpp:hats:0";
const NS_SIMS: &str = "urn:xmpp:sims:1";
const NS_SFS: &str = "urn:xmpp:sfs:0";
const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";
const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";
const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_STICKERS: &str = "urn:xmpp:stickers:0";

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
    /// XEP-0461 reply marker identifying the quoted message.
    pub reply: Option<xep_reply::ReplyMarker>,
    /// XEP-0428 fallback range over the body identifying the quoted prefix.
    /// Offsets count Unicode scalar values and `end` is exclusive.
    pub fallback: Option<xep_reply::FallbackRange>,
    /// XEP-0201 thread reference (with optional parent for nested threads).
    pub thread: Option<xep_thread::ThreadRef>,
    /// XEP-0446 / XEP-0447 shared files attached to the message.
    pub shared_files: Vec<SharedFile>,
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

    // XEP-0203: Delayed Delivery
    let timestamp = el
        .get_child("delay", NS_DELAY)
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // XEP-0359: Unique and Stable Stanza IDs
    let stanza_id = el
        .get_child("stanza-id", NS_STANZA_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let origin_id = el
        .get_child("origin-id", NS_ORIGIN_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    // XEP-0308: Last Message Correction
    let replaces_id = el
        .get_child("replace", NS_REPLACE)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    // XEP-0424: Message Retraction
    let retracts_id = el
        .get_child("retract", NS_RETRACT)
        .and_then(|e| e.attr("id"))
        .map(String::from)
        .or_else(|| {
            // server-side retraction uses <retracted>
            el.get_child("retracted", NS_RETRACT)
                .and_then(|e| e.attr("id"))
                .map(String::from)
        });

    // XEP-0444: Message Reactions
    let reactions_el = el.get_child("reactions", NS_REACTIONS);
    let reaction_target_id = reactions_el.and_then(|e| e.attr("id")).map(String::from);
    let reaction_emojis = reactions_el
        .map(|e| {
            e.children()
                .filter(|c| c.name() == "reaction")
                .map(|c| c.text())
                .collect()
        })
        .unwrap_or_default();

    // XEP-0461: Message Replies + XEP-0428: Fallback Indication
    let reply_marker = xep_reply::parse_reply(el);
    let reply_to_id = reply_marker.as_ref().map(|m| m.id.clone());
    let reply_to_sender = reply_marker.as_ref().map(|m| m.to.to_string());
    let reply_fallback = xep_reply::parse_fallback(el).map(|r| (r.start, r.end));

    // XEP-0394: Message Markup
    let markup_spans = el
        .get_child("markups", NS_MARKUP)
        .map(parse_markup_spans)
        .unwrap_or_default();

    // XEP-0085: Chat State Notifications
    let chat_state = el
        .children()
        .find(|c| c.ns() == NS_CHAT_STATE)
        .map(|c| c.name().to_string());

    // XEP-0333: Displayed Markers
    let displayed_marker_id = el
        .get_child("displayed", NS_MARKERS)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    // XEP-0372: References (mentions and data)
    let mut mention_uris: Vec<String> = Vec::new();
    let mut broadcast_mention: Option<String> = None;
    let mut shared_files: Vec<SharedFile> = Vec::new();

    for child in el
        .children()
        .filter(|c| c.name() == "reference" && c.ns() == NS_REFERENCES)
    {
        match child.attr("type") {
            Some("mention") => {
                if let Some(uri) = child.attr("uri") {
                    let uri_str = uri.to_string();
                    if uri_str.starts_with("xmpp:")
                        && (uri_str.contains("@everyone") || uri_str.contains("@here"))
                    {
                        broadcast_mention = Some(uri_str.clone());
                    }
                    mention_uris.push(uri_str);
                }
            }
            Some("data") => {
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
        forum_post_kind,
        forum_title,
        thread_id,
        parent_thread_id,
        is_sticker,
    }
}

fn parse_markup_spans(markups_el: &Element) -> Vec<MarkupSpan> {
    markups_el
        .children()
        .filter_map(|child| {
            let span_type = match child.name() {
                "strong" => MarkupSpanType::Bold,
                "emphasis" => MarkupSpanType::Italic,
                "strike" => MarkupSpanType::Strikethrough,
                "code" => MarkupSpanType::Code,
                "codex" => MarkupSpanType::CodeBlock,
                "blockquote" => MarkupSpanType::Blockquote,
                "span" if child.attr("uri").is_some() => MarkupSpanType::Link,
                _ => return None,
            };
            let start: usize = child.attr("start")?.parse().ok()?;
            let end: usize = child.attr("end")?.parse().ok()?;
            let uri = child.attr("uri").map(String::from);
            Some(MarkupSpan {
                span_type,
                start,
                end,
                uri,
            })
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

    InboundPresence {
        from,
        to,
        presence_type,
        status,
        show,
        hats,
        muc_affiliation,
        muc_role,
    }
}

// ─── Outbound trait ───────────────────────────────────────────────────────

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
        let stanza = Element::builder("message", NS_CLIENT)
            .attr("to", jid)
            .attr("type", message_type)
            .append(Element::builder(state, NS_CHAT_STATE).build())
            .build();
        self.send_stanza(stanza).await
    }

    async fn send_displayed_marker(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
    ) -> ClientResult<()> {
        let stanza = Element::builder("message", NS_CLIENT)
            .attr("to", jid)
            .attr("type", message_type)
            .append(
                Element::builder("displayed", NS_MARKERS)
                    .attr("id", message_id)
                    .build(),
            )
            .build();
        self.send_stanza(stanza).await
    }

    async fn retract_message(
        &self,
        jid: &str,
        message_id: &str,
        message_type: &str,
    ) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let stanza = Element::builder("message", NS_CLIENT)
            .attr("to", jid)
            .attr("type", message_type)
            .attr("id", id.as_str())
            .append(
                Element::builder("retract", NS_RETRACT)
                    .attr("id", message_id)
                    .build(),
            )
            .append(
                Element::builder("body", NS_CLIENT)
                    .append("This message was retracted.")
                    .build(),
            )
            .build();
        self.send_stanza(stanza).await
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Build a `<message/>` stanza carrying the body plus any XEP payloads from
/// `options`. All XML construction goes through typed `minidom::Element`
/// builders — never `format!` — per the project XML hard rule.
fn build_outbound_message(
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

    if let Some(marker) = options.reply.as_ref() {
        builder = builder.append(xep_reply::build_reply_element(marker));
    }
    if let Some(range) = options.fallback.as_ref() {
        builder = builder.append(xep_reply::build_fallback_element(range));
    }
    if let Some(thread) = options.thread.as_ref() {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    for file in &options.shared_files {
        builder = builder.append(build_file_sharing_element(file));
    }
    Ok((stanza_id, builder.build()))
}

fn build_file_sharing_element(file: &SharedFile) -> Element {
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
             <item affiliation='owner' role='moderator'/>\
             </x>\
             </presence>",
        );
        let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
            panic!("expected Presence");
        };
        assert_eq!(p.muc_affiliation, Some(MucAffiliation::Owner));
        assert_eq!(p.muc_role, Some(MucRole::Moderator));
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
    fn parse_ignores_iq() {
        let e = el("<iq xmlns='jabber:client' type='get' id='iq-1'/>");
        assert!(parse(&e).is_none());
    }
}
