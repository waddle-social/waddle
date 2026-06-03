//! XEP-0430 — Inbox.
//!
//! Conformant wire shape for "give me the list of conversations I'm part of,
//! each with its unread counter and (optionally) the last archived message".
//!
//! ## Request
//!
//! ```xml
//! <iq type='get' id='ib-1'>
//!   <inbox xmlns='urn:xmpp:inbox:1' unread-only='false' messages='true'>
//!     <set xmlns='http://jabber.org/protocol/rsm'>
//!       <max>20</max>
//!     </set>
//!   </inbox>
//! </iq>
//! ```
//!
//! ## Response — two phases
//!
//! Phase 1 — one `<message/>` per conversation:
//!
//! ```xml
//! <message>
//!   <entry xmlns='urn:xmpp:inbox:1' unread='5' jid='alice@example.net' id='mam-uuid'/>
//!   <result xmlns='urn:xmpp:mam:2' queryid='ib-1' id='mam-uuid'>
//!     <forwarded xmlns='urn:xmpp:forward:0'>
//!       <message xmlns='jabber:client' from='alice@example.net' to='me@example.org' type='chat'>
//!         <body>Hello</body>
//!       </message>
//!     </forwarded>
//!   </result>
//! </message>
//! ```
//!
//! Phase 2 — `<iq type='result'><fin/></iq>`:
//!
//! ```xml
//! <iq type='result' id='ib-1'>
//!   <fin xmlns='urn:xmpp:inbox:1' total='3' unread='2' all-unread='6'>
//!     <set xmlns='http://jabber.org/protocol/rsm'/>
//!   </fin>
//! </iq>
//! ```
//!
//! Attributes on `<inbox/>`:
//! - `unread-only` (default `false`): filter to only unread conversations.
//! - `messages` (default `true`): when `false`, omit the embedded MAM
//!   `<result/><forwarded/>` body and emit only the bare `<entry/>` element.
//!
//! ## Mark-read
//!
//! XEP-0430 itself defers read-state semantics to XEP-0333 chat markers.
//! Waddle uses a Waddle-private `urn:waddle:inbox:0` namespace for
//! implementation-specific metadata, live push markers, and the
//! `<mark-read/>` IQ-set. The canonical query surface remains the
//! standards-track `urn:xmpp:inbox:1` shape above.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};

use crate::inbox::{ConversationKind, InboxEntry};
use crate::xep::xep0059::{
    build_rsm_request_element, build_rsm_response_element, parse_rsm_request, parse_rsm_response,
    RsmError, RsmRequest, RsmResponse,
};

/// XEP-0430 inbox query/response namespace.
pub const NS_INBOX: &str = "urn:xmpp:inbox:1";

/// MAM result namespace used for the embedded `<result/>` element in
/// streamed inbox messages (XEP-0313 §4.2). Re-exported here so callers
/// can build the inbox-side wire without taking a transitive dependency
/// on the MAM module.
pub const NS_MAM: &str = "urn:xmpp:mam:2";

/// Default `jabber:client` namespace for the forwarded message body.
pub const NS_CLIENT: &str = "jabber:client";

use crate::xep::xep0297::NS_FORWARD;

/// Waddle-private namespace for inbox metadata, live push markers, and
/// the `<mark-read/>` IQ-set.
///
/// The canonical query path lives at [`NS_INBOX`]. See module docs for
/// the rationale: XEP-0430 leaves read-state up to XEP-0333 chat
/// markers, which does not cover the 1:1 DM case in Waddle today.
pub const NS_WADDLE_INBOX: &str = "urn:waddle:inbox:0";
pub const NS_INBOX_MARK_READ: &str = NS_WADDLE_INBOX;

/// Errors returned by inbox stanza parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InboxError {
    #[error("expected <{0}/> element")]
    ExpectedElement(&'static str),
    #[error("missing attribute '{0}'")]
    MissingAttribute(&'static str),
    #[error("invalid JID '{0}'")]
    InvalidJid(String),
    #[error("invalid integer '{0}'")]
    InvalidInteger(String),
    #[error("invalid conversation kind '{0}'")]
    InvalidKind(String),
    #[error("invalid RSM element: {0}")]
    InvalidRsm(String),
    #[error("payload is not the expected IQ type")]
    WrongIqType,
}

impl From<RsmError> for InboxError {
    fn from(err: RsmError) -> Self {
        InboxError::InvalidRsm(err.to_string())
    }
}

/// Parsed `<inbox xmlns='urn:xmpp:inbox:1'/>` request.
///
/// Defaults match XEP-0430: `unread_only = false`, `messages = true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxQuery {
    /// When `true`, only return conversations with `unread > 0`.
    pub unread_only: bool,
    /// When `false`, omit the embedded MAM `<result/><forwarded/>` body
    /// from each streamed `<message/>` and emit only `<entry/>`.
    pub messages: bool,
    /// Optional RSM pagination cursor (`<set xmlns='…/rsm'/>`).
    pub rsm: Option<RsmRequest>,
}

impl Default for InboxQuery {
    fn default() -> Self {
        Self {
            unread_only: false,
            messages: true,
            rsm: None,
        }
    }
}

/// Waddle-private `<mark-read/>` IQ-set parameters.
///
/// Carried under [`NS_INBOX_MARK_READ`] (the legacy `urn:waddle:inbox:0`
/// namespace). XEP-0430 leaves read-state up to XEP-0333; Waddle keeps
/// the direct mark-read for 1:1 DMs which the displayed-marker bridge
/// does not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxMarkRead {
    pub partner: BareJid,
    pub thread_id: Option<String>,
}

fn iq_payload(iq: &Iq, want_set: bool) -> Result<&Element, InboxError> {
    match iq {
        Iq::Get { payload: e, .. } if !want_set => Ok(e),
        Iq::Set { payload: e, .. } if want_set => Ok(e),
        _ => Err(InboxError::WrongIqType),
    }
}

fn parse_bool_attr(elem: &Element, attr: &str, default: bool) -> bool {
    match elem.attr(attr) {
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        Some(_) => default,
        None => default,
    }
}

/// Parse a XEP-0430 `<inbox xmlns='urn:xmpp:inbox:1'/>` IQ-get request.
pub fn parse_inbox_query(iq: &Iq) -> Result<InboxQuery, InboxError> {
    let elem = iq_payload(iq, false)?;
    if !elem.is("inbox", NS_INBOX) {
        return Err(InboxError::ExpectedElement("inbox"));
    }
    let unread_only = parse_bool_attr(elem, "unread-only", false);
    let messages = parse_bool_attr(elem, "messages", true);
    let rsm = match elem.get_child("set", crate::xep::xep0059::NS_RSM) {
        Some(set) => Some(parse_rsm_request(set)?),
        None => None,
    };
    Ok(InboxQuery {
        unread_only,
        messages,
        rsm,
    })
}

/// Parse the Waddle-private `<mark-read/>` IQ-set.
pub fn parse_mark_read(iq: &Iq) -> Result<InboxMarkRead, InboxError> {
    let elem = iq_payload(iq, true)?;
    if !elem.is("mark-read", NS_INBOX_MARK_READ) {
        return Err(InboxError::ExpectedElement("mark-read"));
    }
    let raw = elem
        .attr("partner")
        .ok_or(InboxError::MissingAttribute("partner"))?;
    let partner: BareJid = raw
        .parse()
        .map_err(|_| InboxError::InvalidJid(raw.to_string()))?;
    let thread_id = elem
        .attr("thread")
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Ok(InboxMarkRead { partner, thread_id })
}

fn kind_str(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::MucRoom => "muc",
    }
}

fn parse_kind_str(raw: &str) -> Result<ConversationKind, InboxError> {
    Ok(match raw {
        "direct" => ConversationKind::Direct,
        "muc" => ConversationKind::MucRoom,
        other => return Err(InboxError::InvalidKind(other.to_string())),
    })
}

/// Build the bare `<entry xmlns='urn:xmpp:inbox:1' …/>` element that
/// goes inside each streamed inbox `<message/>`.
///
/// Required attributes per XEP-0430:
/// - `unread` — unsigned integer count of unread messages in this
///   conversation.
/// - `jid` — bare JID of the conversation partner (or MUC room).
/// - `id` — XEP-0359 stanza-id of the last archived message.
///
pub fn build_inbox_entry_element(entry: &InboxEntry) -> Element {
    Element::builder("entry", NS_INBOX)
        .attr(
            minidom::rxml::xml_ncname!("unread").to_owned(),
            entry.unread.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            entry.partner.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            entry.last_stanza_id.as_str(),
        )
        .build()
}

/// Build Waddle-private metadata for an inbox entry.
///
/// This keeps Waddle-specific fields out of the official
/// `urn:xmpp:inbox:1` `<entry/>` shape while preserving the data the
/// chat UI needs for MUC/thread unread state.
pub fn build_inbox_metadata_element(entry: &InboxEntry) -> Element {
    let mut builder = Element::builder("metadata", NS_WADDLE_INBOX)
        .attr(
            minidom::rxml::xml_ncname!("kind").to_owned(),
            kind_str(entry.kind),
        )
        .attr(
            minidom::rxml::xml_ncname!("last-updated").to_owned(),
            entry.last_updated.to_string(),
        );
    if let Some(thread_id) = &entry.thread_id {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("thread").to_owned(),
            thread_id.as_str(),
        );
    }
    if let Some(title) = &entry.thread_title {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("thread-title").to_owned(),
            title.as_str(),
        );
    }
    if entry.reply_count > 0 {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("reply-count").to_owned(),
            entry.reply_count.to_string(),
        );
    }
    if let Some(author) = &entry.author {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("author").to_owned(),
            author.as_str(),
        );
    }
    if let Some(preview) = &entry.preview {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("preview").to_owned(),
            preview.as_str(),
        );
    }
    builder.build()
}

/// Parse an `<entry/>` element back into a typed [`InboxEntry`].
///
/// This parses only the official XEP-0430 fields. Use
/// [`parse_inbox_entry_with_metadata`] when a sibling Waddle metadata
/// element is available.
pub fn parse_inbox_entry_element(elem: &Element) -> Result<InboxEntry, InboxError> {
    parse_inbox_entry_with_metadata(elem, None)
}

/// Parse an official XEP-0430 `<entry/>` plus optional Waddle metadata
/// into a typed [`InboxEntry`].
pub fn parse_inbox_entry_with_metadata(
    elem: &Element,
    metadata: Option<&Element>,
) -> Result<InboxEntry, InboxError> {
    if !elem.is("entry", NS_INBOX) {
        return Err(InboxError::ExpectedElement("entry"));
    }
    let partner_raw = elem
        .attr("jid")
        .ok_or(InboxError::MissingAttribute("jid"))?;
    let partner: BareJid = partner_raw
        .parse()
        .map_err(|_| InboxError::InvalidJid(partner_raw.to_string()))?;
    let last_stanza_id = elem
        .attr("id")
        .ok_or(InboxError::MissingAttribute("id"))?
        .to_string();
    let unread_raw = elem
        .attr("unread")
        .ok_or(InboxError::MissingAttribute("unread"))?;
    let unread: u32 = unread_raw
        .parse()
        .map_err(|_| InboxError::InvalidInteger(unread_raw.to_string()))?;
    let mut kind = ConversationKind::Direct;
    let mut last_updated: i64 = 0;
    let mut thread_id = None;
    let mut thread_title = None;
    let mut reply_count: u32 = 0;
    let mut author = None;
    let mut preview = None;

    if let Some(metadata) = metadata {
        if !metadata.is("metadata", NS_WADDLE_INBOX) {
            return Err(InboxError::ExpectedElement("metadata"));
        }
        if let Some(raw) = metadata.attr("kind") {
            kind = parse_kind_str(raw)?;
        }
        last_updated = match metadata.attr("last-updated") {
            Some(raw) => raw
                .parse()
                .map_err(|_| InboxError::InvalidInteger(raw.to_string()))?,
            None => 0,
        };
        thread_id = metadata
            .attr("thread")
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
        thread_title = metadata
            .attr("thread-title")
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
        reply_count = metadata
            .attr("reply-count")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        author = metadata
            .attr("author")
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
        preview = metadata
            .attr("preview")
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned);
    }

    Ok(InboxEntry {
        partner,
        kind,
        last_stanza_id,
        last_updated,
        unread,
        preview,
        thread_id,
        thread_title,
        reply_count,
        author,
    })
}

/// Embedded MAM `<result xmlns='urn:xmpp:mam:2'/>` payload for an inbox
/// `<message/>`. Carries the last archived stanza body forwarded under
/// XEP-0297, exactly as a MAM query would.
#[derive(Debug, Clone)]
pub struct InboxLastMessage<'a> {
    /// XEP-0313 archive id of the last message in this conversation.
    pub mam_id: &'a str,
    /// The forwarded inner `<message/>` element (already a typed
    /// minidom build, e.g. emitted by the MAM response builder).
    pub forwarded_inner: Element,
    /// XEP-0203 delay stamp (RFC3339) for the forwarded message.
    pub delay_stamp: Option<&'a str>,
}

/// Build one streamed `<message/>` stanza for an inbox entry.
///
/// The `<message/>` always carries an `<entry/>` element. When
/// `last_message` is `Some(_)`, the message also carries a
/// `<result xmlns='urn:xmpp:mam:2' queryid='…' id='…'>` payload pinning
/// the conversation's most recent archived message — the embedded MAM
/// payload is omitted under `messages='false'`.
///
/// `to` is the recipient's full JID (the requesting client); `query_id`
/// is the IQ id correlating with the eventual `<fin/>` response.
pub fn build_inbox_entry_message(
    to: jid::Jid,
    query_id: &str,
    entry: &InboxEntry,
    last_message: Option<InboxLastMessage<'_>>,
) -> Message {
    let mut msg = Message::new(Some(to));
    msg.type_ = MessageType::Normal;
    msg.payloads.push(build_inbox_entry_element(entry));
    if let Some(last) = last_message {
        let mut forwarded = Element::builder("forwarded", NS_FORWARD);
        if let Some(stamp) = last.delay_stamp {
            forwarded = forwarded.append(
                Element::builder("delay", "urn:xmpp:delay")
                    .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp)
                    .build(),
            );
        }
        forwarded = forwarded.append(last.forwarded_inner);
        let result = Element::builder("result", NS_MAM)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), query_id)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), last.mam_id)
            .append(forwarded.build())
            .build();
        msg.payloads.push(result);
    }
    msg.payloads.push(build_inbox_metadata_element(entry));
    msg
}

/// Counts carried in the XEP-0430 `<fin/>` element.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxFinCounts {
    /// Total conversations matched by the query (post-filter).
    pub total: u32,
    /// Conversations in the result with `unread > 0`.
    pub unread: u32,
    /// Sum of unread counts across the result.
    pub all_unread: u32,
}

/// Build the final `<iq type='result'><fin xmlns='urn:xmpp:inbox:1'/></iq>`
/// closing the streamed inbox response.
pub fn build_inbox_fin_iq(original: &Iq, counts: InboxFinCounts, rsm: Option<RsmResponse>) -> Iq {
    let mut fin = Element::builder("fin", NS_INBOX)
        .attr(
            minidom::rxml::xml_ncname!("total").to_owned(),
            counts.total.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("unread").to_owned(),
            counts.unread.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("all-unread").to_owned(),
            counts.all_unread.to_string(),
        );
    if let Some(rsm) = rsm {
        fin = fin.append(build_rsm_response_element(&rsm));
    }
    Iq::Result {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        payload: Some(fin.build()),
    }
}

/// Parse a XEP-0430 `<fin/>` IQ result (client-side helper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxFin {
    pub counts: InboxFinCounts,
    pub rsm: Option<RsmResponse>,
}

pub fn parse_inbox_fin(iq: &Iq) -> Result<InboxFin, InboxError> {
    let elem = match iq {
        Iq::Result {
            payload: Some(e), ..
        } => e,
        _ => return Err(InboxError::WrongIqType),
    };
    if !elem.is("fin", NS_INBOX) {
        return Err(InboxError::ExpectedElement("fin"));
    }
    fn read_u32(elem: &Element, name: &'static str) -> Result<u32, InboxError> {
        let raw = elem.attr(name).ok_or(InboxError::MissingAttribute(name))?;
        raw.parse()
            .map_err(|_| InboxError::InvalidInteger(raw.to_string()))
    }
    let counts = InboxFinCounts {
        total: read_u32(elem, "total")?,
        unread: read_u32(elem, "unread")?,
        all_unread: read_u32(elem, "all-unread")?,
    };
    let rsm = match elem.get_child("set", crate::xep::xep0059::NS_RSM) {
        Some(set) => Some(parse_rsm_response(set)?),
        None => None,
    };
    Ok(InboxFin { counts, rsm })
}

/// Build the response to a Waddle-private `<mark-read/>` IQ-set.
pub fn build_mark_read_result(original: &Iq) -> Iq {
    Iq::Result {
        from: original.to().cloned(),
        to: original.from().cloned(),
        id: original.id().to_string(),
        payload: None,
    }
}

/// Build a headline message that pushes an updated inbox entry to a
/// user's resource (cross-device sync after a mark-read or a new
/// message). The stanza carries a Waddle-private `<push/>` marker with
/// a conformant XEP-0430 `<entry/>` child plus Waddle metadata.
pub fn build_inbox_push(to: jid::Jid, entry: &InboxEntry) -> Message {
    let mut msg = Message::new(Some(to));
    msg.type_ = MessageType::Headline;
    msg.payloads.push(
        Element::builder("push", NS_WADDLE_INBOX)
            .append(build_inbox_entry_element(entry))
            .append(build_inbox_metadata_element(entry))
            .build(),
    );
    msg
}

/// Whether the given IQ targets the conformant XEP-0430 inbox surface.
pub fn is_inbox_iq(iq: &Iq) -> bool {
    let elem = match iq {
        Iq::Get { payload: e, .. } | Iq::Set { payload: e, .. } => e,
        _ => return false,
    };
    elem.ns() == NS_INBOX && elem.name() == "inbox"
}

/// Whether the given IQ targets the Waddle-private mark-read action.
pub fn is_mark_read_iq(iq: &Iq) -> bool {
    let elem = match iq {
        Iq::Get { payload: e, .. } | Iq::Set { payload: e, .. } => e,
        _ => return false,
    };
    elem.ns() == NS_INBOX_MARK_READ && elem.name() == "mark-read"
}

/// Helper for building an outbound inbox query IQ (clients).
pub fn build_inbox_query_iq(query: &InboxQuery, id: impl Into<String>) -> Iq {
    let mut inbox = Element::builder("inbox", NS_INBOX)
        .attr(
            minidom::rxml::xml_ncname!("unread-only").to_owned(),
            if query.unread_only { "true" } else { "false" },
        )
        .attr(
            minidom::rxml::xml_ncname!("messages").to_owned(),
            if query.messages { "true" } else { "false" },
        );
    if let Some(rsm) = query.rsm.as_ref() {
        inbox = inbox.append(build_rsm_request_element(rsm));
    }
    Iq::Get {
        from: None,
        to: None,
        id: id.into(),
        payload: inbox.build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_iq(child: Element) -> Iq {
        Iq::Get {
            from: Some("me@example.com/res".parse().unwrap()),
            to: Some("me@example.com".parse().unwrap()),
            id: "ib-1".into(),
            payload: child,
        }
    }

    fn set_iq(child: Element) -> Iq {
        Iq::Set {
            from: Some("me@example.com/res".parse().unwrap()),
            to: Some("me@example.com".parse().unwrap()),
            id: "ib-2".into(),
            payload: child,
        }
    }

    #[test]
    fn parse_inbox_query_defaults() {
        let iq = get_iq(Element::builder("inbox", NS_INBOX).build());
        let parsed = parse_inbox_query(&iq).expect("query parses");
        assert!(!parsed.unread_only);
        assert!(parsed.messages);
        assert!(parsed.rsm.is_none());
    }

    #[test]
    fn parse_inbox_query_unread_only_and_no_messages() {
        let iq = get_iq(
            Element::builder("inbox", NS_INBOX)
                .attr(minidom::rxml::xml_ncname!("unread-only").to_owned(), "true")
                .attr(minidom::rxml::xml_ncname!("messages").to_owned(), "false")
                .build(),
        );
        let parsed = parse_inbox_query(&iq).expect("query parses");
        assert!(parsed.unread_only);
        assert!(!parsed.messages);
    }

    #[test]
    fn parse_inbox_query_with_rsm() {
        let iq = get_iq(
            Element::builder("inbox", NS_INBOX)
                .append(
                    Element::builder("set", crate::xep::xep0059::NS_RSM)
                        .append(
                            Element::builder("max", crate::xep::xep0059::NS_RSM)
                                .append("20")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        );
        let parsed = parse_inbox_query(&iq).expect("rsm parses");
        let rsm = parsed.rsm.expect("rsm present");
        assert_eq!(rsm.max, Some(20));
    }

    #[test]
    fn parse_inbox_query_rejects_wrong_namespace() {
        let iq = get_iq(Element::builder("inbox", "urn:waddle:inbox:0").build());
        assert!(matches!(
            parse_inbox_query(&iq),
            Err(InboxError::ExpectedElement("inbox"))
        ));
    }

    #[test]
    fn parse_mark_read_namespace_unchanged() {
        let iq = set_iq(
            Element::builder("mark-read", NS_INBOX_MARK_READ)
                .attr(
                    minidom::rxml::xml_ncname!("partner").to_owned(),
                    "alice@example.com",
                )
                .build(),
        );
        let parsed = parse_mark_read(&iq).expect("mark-read parses");
        assert_eq!(parsed.partner.to_string(), "alice@example.com");
        assert!(parsed.thread_id.is_none());
    }

    #[test]
    fn parse_mark_read_with_thread() {
        let iq = set_iq(
            Element::builder("mark-read", NS_INBOX_MARK_READ)
                .attr(
                    minidom::rxml::xml_ncname!("partner").to_owned(),
                    "room@muc.example.com",
                )
                .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "t-42")
                .build(),
        );
        let parsed = parse_mark_read(&iq).expect("mark-read parses");
        assert_eq!(parsed.thread_id.as_deref(), Some("t-42"));
    }

    #[test]
    fn entry_round_trip_direct() {
        let entry = InboxEntry::new(
            "alice@example.com".parse().unwrap(),
            ConversationKind::Direct,
            "sid-42",
            1_700_000,
        )
        .with_unread(3)
        .with_preview("hi there");
        let elem = build_inbox_entry_element(&entry);
        assert_eq!(elem.name(), "entry");
        assert_eq!(elem.ns(), NS_INBOX);
        assert_eq!(elem.attr("jid"), Some("alice@example.com"));
        assert_eq!(elem.attr("id"), Some("sid-42"));
        assert_eq!(elem.attr("unread"), Some("3"));
        assert_eq!(elem.attr("kind"), None);
        assert_eq!(elem.attr("last-updated"), None);
        assert_eq!(elem.attr("preview"), None);
        let metadata = build_inbox_metadata_element(&entry);
        assert_eq!(metadata.name(), "metadata");
        assert_eq!(metadata.ns(), NS_WADDLE_INBOX);
        assert_eq!(metadata.attr("kind"), Some("direct"));
        assert_eq!(metadata.attr("last-updated"), Some("1700000"));
        assert_eq!(metadata.attr("preview"), Some("hi there"));
        let parsed = parse_inbox_entry_with_metadata(&elem, Some(&metadata)).expect("entry parses");
        assert_eq!(parsed, entry);
    }

    #[test]
    fn entry_round_trip_thread() {
        let entry = InboxEntry::new(
            "room@muc.example.com".parse().unwrap(),
            ConversationKind::MucRoom,
            "sid-99",
            1_700_000_000,
        )
        .with_unread(2)
        .with_thread("t-99")
        .with_thread_title("Getting Started")
        .with_reply_count(7)
        .with_author("alice");
        let elem = build_inbox_entry_element(&entry);
        assert_eq!(elem.attr("thread"), None);
        assert_eq!(elem.attr("thread-title"), None);
        assert_eq!(elem.attr("reply-count"), None);
        assert_eq!(elem.attr("author"), None);
        let metadata = build_inbox_metadata_element(&entry);
        assert_eq!(metadata.attr("kind"), Some("muc"));
        assert_eq!(metadata.attr("thread"), Some("t-99"));
        assert_eq!(metadata.attr("thread-title"), Some("Getting Started"));
        assert_eq!(metadata.attr("reply-count"), Some("7"));
        assert_eq!(metadata.attr("author"), Some("alice"));
        let parsed = parse_inbox_entry_with_metadata(&elem, Some(&metadata)).expect("entry parses");
        assert_eq!(parsed, entry);
    }

    #[test]
    fn build_inbox_entry_message_without_last_message_omits_result() {
        let entry = InboxEntry::new(
            "alice@example.com".parse().unwrap(),
            ConversationKind::Direct,
            "sid-1",
            1,
        )
        .with_unread(1);
        let msg =
            build_inbox_entry_message("me@example.com/res".parse().unwrap(), "q-1", &entry, None);
        assert_eq!(msg.type_, MessageType::Normal);
        assert_eq!(msg.payloads.len(), 2);
        assert!(msg.payloads[0].is("entry", NS_INBOX));
        assert_eq!(msg.payloads[0].attr("queryid"), None);
        assert!(msg.payloads[1].is("metadata", NS_WADDLE_INBOX));
    }

    #[test]
    fn build_inbox_entry_message_with_last_message_wraps_in_mam_result() {
        let entry = InboxEntry::new(
            "alice@example.com".parse().unwrap(),
            ConversationKind::Direct,
            "mam-1",
            1,
        )
        .with_unread(1);
        let inner = Element::builder("message", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@example.com",
            )
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                "me@example.com",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
            .append(Element::builder("body", NS_CLIENT).append("Hello").build())
            .build();
        let msg = build_inbox_entry_message(
            "me@example.com/res".parse().unwrap(),
            "q-1",
            &entry,
            Some(InboxLastMessage {
                mam_id: "mam-1",
                forwarded_inner: inner,
                delay_stamp: Some("2026-05-17T00:00:00Z"),
            }),
        );
        assert_eq!(msg.payloads.len(), 3);
        assert!(msg.payloads[0].is("entry", NS_INBOX));
        assert!(msg.payloads[2].is("metadata", NS_WADDLE_INBOX));
        let result = msg
            .payloads
            .iter()
            .find(|p| p.is("result", NS_MAM))
            .expect("result element");
        assert_eq!(result.attr("queryid"), Some("q-1"));
        assert_eq!(result.attr("id"), Some("mam-1"));
        let forwarded = result
            .get_child("forwarded", NS_FORWARD)
            .expect("forwarded");
        assert!(forwarded.get_child("delay", "urn:xmpp:delay").is_some());
        assert!(forwarded.get_child("message", NS_CLIENT).is_some());
    }

    #[test]
    fn fin_iq_round_trip() {
        let original = get_iq(Element::builder("inbox", NS_INBOX).build());
        let counts = InboxFinCounts {
            total: 3,
            unread: 2,
            all_unread: 7,
        };
        let rsm = RsmResponse::new()
            .with_first("first-id", None)
            .with_last("last-id")
            .with_count(3);
        let fin_iq = build_inbox_fin_iq(&original, counts, Some(rsm.clone()));
        let parsed = parse_inbox_fin(&fin_iq).expect("fin parses");
        assert_eq!(parsed.counts, counts);
        let rsm_back = parsed.rsm.expect("rsm round-trips");
        assert_eq!(rsm_back.first.as_deref(), Some("first-id"));
        assert_eq!(rsm_back.last.as_deref(), Some("last-id"));
        assert_eq!(rsm_back.count, Some(3));
    }

    #[test]
    fn is_inbox_iq_recognises_conformant_namespace() {
        assert!(is_inbox_iq(&get_iq(
            Element::builder("inbox", NS_INBOX).build()
        )));
        // Legacy `urn:waddle:inbox:0` queries no longer match.
        assert!(!is_inbox_iq(&get_iq(
            Element::builder("query", NS_INBOX_MARK_READ).build()
        )));
        // mark-read goes through `is_mark_read_iq`, not `is_inbox_iq`.
        assert!(!is_inbox_iq(&set_iq(
            Element::builder("mark-read", NS_INBOX_MARK_READ)
                .attr(
                    minidom::rxml::xml_ncname!("partner").to_owned(),
                    "x@example.com"
                )
                .build(),
        )));
    }

    #[test]
    fn is_mark_read_iq_recognises_legacy_namespace() {
        assert!(is_mark_read_iq(&set_iq(
            Element::builder("mark-read", NS_INBOX_MARK_READ)
                .attr(
                    minidom::rxml::xml_ncname!("partner").to_owned(),
                    "x@example.com"
                )
                .build(),
        )));
        assert!(!is_mark_read_iq(&set_iq(
            Element::builder("mark-read", NS_INBOX)
                .attr(
                    minidom::rxml::xml_ncname!("partner").to_owned(),
                    "x@example.com"
                )
                .build(),
        )));
    }

    #[test]
    fn build_inbox_query_iq_default_attrs() {
        let iq = build_inbox_query_iq(&InboxQuery::default(), "id-1");
        match iq {
            Iq::Get { payload: elem, .. } => {
                assert!(elem.is("inbox", NS_INBOX));
                assert_eq!(elem.attr("unread-only"), Some("false"));
                assert_eq!(elem.attr("messages"), Some("true"));
                assert!(elem.get_child("set", crate::xep::xep0059::NS_RSM).is_none());
            }
            _ => panic!("expected Get"),
        }
    }
}
