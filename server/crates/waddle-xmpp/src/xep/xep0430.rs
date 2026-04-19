//! Inbox — unified conversation list with unread counters.
//!
//! This module carries the IQ-level protocol for the "inbox" feature:
//! a single query that returns an ordered list of conversations for the
//! requesting user, each with its last message and unread counter.
//!
//! The contract maps to the in-process types defined in [`crate::inbox`]:
//! `list → Vec<InboxEntry>`, `mark-read`, `total-unread`. We use the
//! `urn:xmpp:inbox:0` namespace while the XSF consolidates the spec; the
//! on-wire shape is stable and already consumed by MongooseIM / Movim.
//!
//! The storage side of the feature lives behind the
//! [`crate::inbox::storage::InboxStorage`] trait.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::inbox::{ConversationKind, InboxEntry};

/// Inbox protocol namespace.
pub const NS_INBOX: &str = "urn:xmpp:inbox:0";

/// Errors returned by inbox stanza parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InboxError {
    #[error("expected <{0}/> in '{NS_INBOX}'")]
    ExpectedElement(&'static str),
    #[error("missing attribute '{0}'")]
    MissingAttribute(&'static str),
    #[error("invalid JID '{0}'")]
    InvalidJid(String),
    #[error("invalid integer '{0}'")]
    InvalidInteger(String),
    #[error("invalid conversation kind '{0}'")]
    InvalidKind(String),
    #[error("payload is not the expected IQ type")]
    WrongIqType,
}

/// A `<query xmlns='urn:xmpp:inbox:0'/>` request for the user's inbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxQuery {
    /// Optional lower bound on `last_updated` (inclusive).
    pub since: Option<i64>,
    /// If true, only return conversations with unread > 0.
    pub only_unread: bool,
}

/// A `<mark-read>` action — `partner` attr carries the target JID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxMarkRead {
    pub partner: BareJid,
}

fn iq_payload<'a>(iq: &'a Iq, want_set: bool) -> Result<&'a Element, InboxError> {
    match &iq.payload {
        IqType::Get(e) if !want_set => Ok(e),
        IqType::Set(e) if want_set => Ok(e),
        _ => Err(InboxError::WrongIqType),
    }
}

pub fn parse_inbox_query(iq: &Iq) -> Result<InboxQuery, InboxError> {
    let elem = iq_payload(iq, false)?;
    if !elem.is("query", NS_INBOX) {
        return Err(InboxError::ExpectedElement("query"));
    }
    let since = match elem.attr("since") {
        Some(raw) => Some(
            raw.parse::<i64>()
                .map_err(|_| InboxError::InvalidInteger(raw.to_string()))?,
        ),
        None => None,
    };
    let only_unread = elem
        .attr("only-unread")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    Ok(InboxQuery { since, only_unread })
}

pub fn parse_mark_read(iq: &Iq) -> Result<InboxMarkRead, InboxError> {
    let elem = iq_payload(iq, true)?;
    if !elem.is("mark-read", NS_INBOX) {
        return Err(InboxError::ExpectedElement("mark-read"));
    }
    let raw = elem
        .attr("partner")
        .ok_or(InboxError::MissingAttribute("partner"))?;
    let partner: BareJid = raw
        .parse()
        .map_err(|_| InboxError::InvalidJid(raw.to_string()))?;
    Ok(InboxMarkRead { partner })
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

pub fn build_entry_element(entry: &InboxEntry) -> Element {
    let mut builder = Element::builder("conversation", NS_INBOX)
        .attr("partner", entry.partner.to_string())
        .attr("kind", kind_str(entry.kind))
        .attr("last-stanza-id", entry.last_stanza_id.as_str())
        .attr("last-updated", entry.last_updated.to_string())
        .attr("unread", entry.unread.to_string());
    if let Some(preview) = &entry.preview {
        builder = builder.append(
            Element::builder("preview", NS_INBOX)
                .append(preview.as_str())
                .build(),
        );
    }
    builder.build()
}

pub fn parse_entry_element(elem: &Element) -> Result<InboxEntry, InboxError> {
    if !elem.is("conversation", NS_INBOX) {
        return Err(InboxError::ExpectedElement("conversation"));
    }
    let partner_raw = elem
        .attr("partner")
        .ok_or(InboxError::MissingAttribute("partner"))?;
    let partner: BareJid = partner_raw
        .parse()
        .map_err(|_| InboxError::InvalidJid(partner_raw.to_string()))?;
    let kind = parse_kind_str(
        elem.attr("kind")
            .ok_or(InboxError::MissingAttribute("kind"))?,
    )?;
    let last_stanza_id = elem
        .attr("last-stanza-id")
        .ok_or(InboxError::MissingAttribute("last-stanza-id"))?
        .to_string();
    let last_updated_raw = elem
        .attr("last-updated")
        .ok_or(InboxError::MissingAttribute("last-updated"))?;
    let last_updated: i64 = last_updated_raw
        .parse()
        .map_err(|_| InboxError::InvalidInteger(last_updated_raw.to_string()))?;
    let unread_raw = elem
        .attr("unread")
        .ok_or(InboxError::MissingAttribute("unread"))?;
    let unread: u32 = unread_raw
        .parse()
        .map_err(|_| InboxError::InvalidInteger(unread_raw.to_string()))?;
    let preview = elem
        .get_child("preview", NS_INBOX)
        .map(|p| p.text())
        .filter(|s| !s.is_empty());
    Ok(InboxEntry {
        partner,
        kind,
        last_stanza_id,
        last_updated,
        unread,
        preview,
    })
}

pub fn build_inbox_query_result(original: &Iq, entries: &[InboxEntry], total_unread: u64) -> Iq {
    let mut container =
        Element::builder("query", NS_INBOX).attr("total-unread", total_unread.to_string());
    for entry in entries {
        container = container.append(build_entry_element(entry));
    }
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(Some(container.build())),
    }
}

pub fn build_mark_read_result(original: &Iq) -> Iq {
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(None),
    }
}

pub fn is_inbox_iq(iq: &Iq) -> bool {
    let elem = match &iq.payload {
        IqType::Get(e) | IqType::Set(e) => e,
        _ => return false,
    };
    if elem.ns() != NS_INBOX {
        return false;
    }
    matches!(elem.name(), "query" | "mark-read")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_iq(child: Element) -> Iq {
        Iq {
            from: Some("me@example.com/res".parse().unwrap()),
            to: Some("me@example.com".parse().unwrap()),
            id: "ib-1".into(),
            payload: IqType::Get(child),
        }
    }

    fn set_iq(child: Element) -> Iq {
        Iq {
            from: Some("me@example.com/res".parse().unwrap()),
            to: Some("me@example.com".parse().unwrap()),
            id: "ib-2".into(),
            payload: IqType::Set(child),
        }
    }

    #[test]
    fn test_parse_query_defaults() {
        let iq = get_iq(Element::builder("query", NS_INBOX).build());
        let parsed = parse_inbox_query(&iq).unwrap();
        assert_eq!(parsed.since, None);
        assert!(!parsed.only_unread);
    }

    #[test]
    fn test_parse_query_with_since_and_only_unread() {
        let iq = get_iq(
            Element::builder("query", NS_INBOX)
                .attr("since", "123")
                .attr("only-unread", "true")
                .build(),
        );
        let parsed = parse_inbox_query(&iq).unwrap();
        assert_eq!(parsed.since, Some(123));
        assert!(parsed.only_unread);
    }

    #[test]
    fn test_parse_query_accepts_numeric_only_unread() {
        let iq = get_iq(
            Element::builder("query", NS_INBOX)
                .attr("since", "123")
                .attr("only-unread", "1")
                .build(),
        );
        let parsed = parse_inbox_query(&iq).unwrap();
        assert_eq!(parsed.since, Some(123));
        assert!(parsed.only_unread);
    }

    #[test]
    fn test_parse_mark_read() {
        let iq = set_iq(
            Element::builder("mark-read", NS_INBOX)
                .attr("partner", "alice@example.com")
                .build(),
        );
        let parsed = parse_mark_read(&iq).unwrap();
        assert_eq!(parsed.partner.to_string(), "alice@example.com");
    }

    #[test]
    fn test_mark_read_requires_partner() {
        let iq = set_iq(Element::builder("mark-read", NS_INBOX).build());
        assert_eq!(
            parse_mark_read(&iq),
            Err(InboxError::MissingAttribute("partner"))
        );
    }

    #[test]
    fn test_entry_round_trip_direct() {
        let entry = InboxEntry::new(
            "alice@example.com".parse().unwrap(),
            ConversationKind::Direct,
            "sid-42",
            1700_000,
        )
        .with_unread(3)
        .with_preview("hi there");
        let elem = build_entry_element(&entry);
        let parsed = parse_entry_element(&elem).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn test_entry_round_trip_muc() {
        let entry = InboxEntry::new(
            "general@conference.example.com".parse().unwrap(),
            ConversationKind::MucRoom,
            "sid-99",
            1_700_000_000,
        )
        .with_unread(0);
        let elem = build_entry_element(&entry);
        let parsed = parse_entry_element(&elem).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn test_result_shape() {
        let entry = InboxEntry::new(
            "a@example.com".parse().unwrap(),
            ConversationKind::Direct,
            "s1",
            1,
        )
        .with_unread(2);
        let iq = get_iq(Element::builder("query", NS_INBOX).build());
        let out = build_inbox_query_result(&iq, std::slice::from_ref(&entry), 2);
        match out.payload {
            IqType::Result(Some(e)) => {
                assert!(e.is("query", NS_INBOX));
                assert_eq!(e.attr("total-unread"), Some("2"));
                assert_eq!(e.children().count(), 1);
            }
            _ => panic!("expected result payload"),
        }
    }

    #[test]
    fn test_is_inbox_iq() {
        assert!(is_inbox_iq(&get_iq(
            Element::builder("query", NS_INBOX).build()
        )));
        assert!(is_inbox_iq(&set_iq(
            Element::builder("mark-read", NS_INBOX)
                .attr("partner", "x@example.com")
                .build(),
        )));
        assert!(!is_inbox_iq(&get_iq(
            Element::builder("query", "other").build()
        )));
    }

    #[test]
    fn test_invalid_kind_rejected() {
        let elem = Element::builder("conversation", NS_INBOX)
            .attr("partner", "a@example.com")
            .attr("kind", "bogus")
            .attr("last-stanza-id", "s")
            .attr("last-updated", "0")
            .attr("unread", "0")
            .build();
        assert!(matches!(
            parse_entry_element(&elem),
            Err(InboxError::InvalidKind(_))
        ));
    }
}
