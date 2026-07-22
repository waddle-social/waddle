//! Client-side parser for XEP-0430 inbox streamed entries.
//!
//! The server emits one `<message><entry xmlns='urn:xmpp:inbox:1'/></message>`
//! per matched conversation (optionally with an embedded
//! `<result xmlns='urn:xmpp:mam:2'>` carrying the forwarded last
//! archived stanza), followed by a closing `<iq type='result'><fin/></iq>`.
//! Waddle-specific metadata and live push markers use
//! `urn:waddle:inbox:0`, keeping the official XEP-0430 `<entry/>`
//! shape exact.

use minidom::Element;

/// Namespace constant — kept local to the client so the wasm bindings
/// don't need a transitive dep on `waddle-xmpp`.
pub const NS_INBOX: &str = "urn:xmpp:inbox:1";
pub const NS_WADDLE_INBOX: &str = "urn:waddle:inbox:0";
const NS_MAM: &str = "urn:xmpp:mam:2";

/// Source surface for one streamed XEP-0430 inbox `<entry/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxStreamEntrySource {
    QueryResponse,
    Push,
}

/// Typed projection of one streamed `<entry/>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxStreamEntry {
    pub source: InboxStreamEntrySource,
    pub query_id: Option<String>,
    pub partner: String,
    pub kind: Option<String>,
    pub last_stanza_id: String,
    pub last_updated: Option<i64>,
    pub unread: u32,
    pub preview: Option<String>,
    pub thread_id: Option<String>,
    pub thread_title: Option<String>,
    pub reply_count: Option<u32>,
    pub author: Option<String>,
}

/// Counts carried in the closing `<fin/>` element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboxFinCounts {
    pub total: u32,
    pub unread: u32,
    pub all_unread: u32,
}

/// Closing fin payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxFin {
    pub counts: InboxFinCounts,
    pub rsm_first: Option<String>,
    pub rsm_last: Option<String>,
    pub rsm_count: Option<u32>,
}

/// Result of one streamed XEP-0430 inbox query: the correlated
/// `<entry/>` stream folded together with the closing `<fin/>`.
/// Shared by the native `inbox_ext::InboxExt` handle extension
/// (cfg-gated) and the wasm driver's pending-query reducer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxPage {
    pub entries: Vec<InboxStreamEntry>,
    pub fin: InboxFin,
}

/// Parse one streamed inbox `<message/>` into a typed entry.
///
/// Returns `None` for messages that carry neither a direct
/// `<entry xmlns='urn:xmpp:inbox:1'/>` query response nor a Waddle
/// `<push xmlns='urn:waddle:inbox:0'/>` wrapper — those flow through
/// the other typed handlers (MAM/PEP/messaging) unchanged.
pub fn parse_inbox_stream_message(message: &Element) -> Option<InboxStreamEntry> {
    if message.name() != "message" {
        return None;
    }
    let message_type = message.attr("type").unwrap_or("normal");
    let (source, entry, metadata) = if message_type == "headline" {
        if message.attr("from").is_some() {
            return None;
        }
        let push = message.children().find(|c| c.is("push", NS_WADDLE_INBOX))?;
        let metadata = push
            .children()
            .find(|c| c.is("metadata", NS_WADDLE_INBOX))?;
        (
            InboxStreamEntrySource::Push,
            push.children().find(|c| c.is("entry", NS_INBOX))?,
            Some(metadata),
        )
    } else {
        if message.attr("from").is_some() {
            return None;
        }
        (
            InboxStreamEntrySource::QueryResponse,
            message.children().find(|c| c.is("entry", NS_INBOX))?,
            message
                .children()
                .find(|c| c.is("metadata", NS_WADDLE_INBOX)),
        )
    };
    let mam_query_id = if source == InboxStreamEntrySource::QueryResponse {
        message
            .children()
            .find(|c| c.is("result", NS_MAM))
            .and_then(|result| result.attr("queryid"))
    } else {
        None
    };
    let partner = entry.attr("jid")?.to_string();
    let last_stanza_id = entry.attr("id")?.to_string();
    let unread = entry.attr("unread")?.parse().ok()?;
    let kind = metadata.and_then(|metadata| metadata.attr("kind"));
    if let Some(kind) = kind {
        if kind != "direct" && kind != "muc" {
            return None;
        }
    } else if source == InboxStreamEntrySource::Push {
        return None;
    }
    let last_updated = match metadata.and_then(|metadata| metadata.attr("last-updated")) {
        Some(raw) => Some(raw.parse().ok()?),
        None if source == InboxStreamEntrySource::Push => return None,
        None => None,
    };
    let reply_count = match metadata.and_then(|metadata| metadata.attr("reply-count")) {
        Some(raw) => Some(raw.parse().ok()?),
        None => None,
    };
    Some(InboxStreamEntry {
        source,
        query_id: mam_query_id.map(str::to_string),
        partner,
        kind: kind.map(str::to_string),
        last_stanza_id,
        last_updated,
        unread,
        preview: metadata
            .and_then(|metadata| metadata.attr("preview"))
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        thread_id: metadata
            .and_then(|metadata| metadata.attr("thread"))
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        thread_title: metadata
            .and_then(|metadata| metadata.attr("thread-title"))
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        reply_count,
        author: metadata
            .and_then(|metadata| metadata.attr("author"))
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    })
}

/// Parse the closing `<iq type='result'><fin/></iq>`.
pub fn parse_inbox_fin(iq: &Element) -> Option<InboxFin> {
    if iq.name() != "iq" {
        return None;
    }
    let fin = iq.children().find(|c| c.is("fin", NS_INBOX))?;
    let counts = InboxFinCounts {
        total: fin.attr("total").and_then(|v| v.parse().ok()).unwrap_or(0),
        unread: fin.attr("unread").and_then(|v| v.parse().ok()).unwrap_or(0),
        all_unread: fin
            .attr("all-unread")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };
    const NS_RSM: &str = "http://jabber.org/protocol/rsm";
    let set = fin.get_child("set", NS_RSM);
    let rsm_first = set
        .and_then(|el| el.get_child("first", NS_RSM))
        .map(|el| el.text())
        .filter(|s| !s.is_empty());
    let rsm_last = set
        .and_then(|el| el.get_child("last", NS_RSM))
        .map(|el| el.text())
        .filter(|s| !s.is_empty());
    let rsm_count = set
        .and_then(|el| el.get_child("count", NS_RSM))
        .and_then(|el| el.text().parse().ok());
    Some(InboxFin {
        counts,
        rsm_first,
        rsm_last,
        rsm_count,
    })
}

/// Build the canonical `<inbox xmlns='urn:xmpp:inbox:1'/>` IQ-get
/// request. The default attribute values match XEP-0430 defaults
/// (`unread-only='false'`, `messages='true'`) and the helper writes
/// them explicitly so the wire form is unambiguous on the receiver.
pub fn build_inbox_query_iq_element(id: &str, unread_only: bool, messages: bool) -> Element {
    let inbox = Element::builder("inbox", NS_INBOX)
        .attr(
            minidom::rxml::xml_ncname!("unread-only").to_owned(),
            if unread_only { "true" } else { "false" },
        )
        .attr(
            minidom::rxml::xml_ncname!("messages").to_owned(),
            if messages { "true" } else { "false" },
        )
        .build();
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(inbox)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inbox_entry_from_streamed_message() {
        let xml = r#"<message xmlns='jabber:client'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1' unread='3'/>
                <result xmlns='urn:xmpp:mam:2' queryid='q1' id='mam-1'/>
                <metadata xmlns='urn:waddle:inbox:0' kind='direct' last-updated='1700000000' preview='hi'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();
        let entry = parse_inbox_stream_message(&element).expect("entry");
        assert_eq!(entry.source, InboxStreamEntrySource::QueryResponse);
        assert_eq!(entry.query_id.as_deref(), Some("q1"));
        assert_eq!(entry.partner, "alice@example.com");
        assert_eq!(entry.unread, 3);
        assert_eq!(entry.last_stanza_id, "mam-1");
        assert_eq!(entry.preview.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_inbox_fin_with_rsm() {
        let xml = r#"<iq xmlns='jabber:client' type='result' id='q1'>
                <fin xmlns='urn:xmpp:inbox:1' total='3' unread='2' all-unread='7'>
                    <set xmlns='http://jabber.org/protocol/rsm'>
                        <first>a</first><last>b</last><count>3</count>
                    </set>
                </fin>
            </iq>"#;
        let element: Element = xml.parse().unwrap();
        let fin = parse_inbox_fin(&element).expect("fin");
        assert_eq!(fin.counts.total, 3);
        assert_eq!(fin.counts.unread, 2);
        assert_eq!(fin.counts.all_unread, 7);
        assert_eq!(fin.rsm_first.as_deref(), Some("a"));
        assert_eq!(fin.rsm_last.as_deref(), Some("b"));
        assert_eq!(fin.rsm_count, Some(3));
    }

    #[test]
    fn parse_inbox_stream_message_ignores_plain_message() {
        let xml = r#"<message xmlns='jabber:client'><body>hi</body></message>"#;
        let element: Element = xml.parse().unwrap();
        assert!(parse_inbox_stream_message(&element).is_none());
    }

    #[test]
    fn parse_unsolicited_inbox_push_without_queryid() {
        let xml = r#"<message xmlns='jabber:client' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='1700000001' preview='new message'/>
                </push>
            </message>"#;
        let element: Element = xml.parse().unwrap();
        let entry = parse_inbox_stream_message(&element).expect("entry");
        assert_eq!(entry.source, InboxStreamEntrySource::Push);
        assert_eq!(entry.query_id, None);
        assert_eq!(entry.partner, "room@muc.example.com");
        assert_eq!(entry.kind.as_deref(), Some("muc"));
        assert_eq!(entry.unread, 2);
        assert_eq!(entry.last_stanza_id, "sid-1");
        assert_eq!(entry.preview.as_deref(), Some("new message"));
    }

    #[test]
    fn parse_ignores_waddle_push_on_non_headline_message() {
        let xml = r#"<message xmlns='jabber:client' type='groupchat'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='1700000001'/>
                </push>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_ignores_waddle_push_with_sender_provenance() {
        let xml = r#"<message xmlns='jabber:client' from='mallory@example.com/web' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='1700000001'/>
                </push>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_ignores_direct_inbox_entry_with_sender_provenance() {
        let xml = r#"<message xmlns='jabber:client' from='mallory@example.com/web'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1' unread='1'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_rejects_inbox_entry_without_unread() {
        let xml = r#"<message xmlns='jabber:client'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_rejects_inbox_entry_with_invalid_unread() {
        let xml = r#"<message xmlns='jabber:client'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1' unread='nope'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_rejects_waddle_push_without_metadata() {
        let xml = r#"<message xmlns='jabber:client' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                </push>
            </message>"#;
        let element: Element = xml.parse().unwrap();

        assert_eq!(parse_inbox_stream_message(&element), None);
    }

    #[test]
    fn parse_rejects_waddle_push_with_invalid_metadata() {
        let invalid_kind = r#"<message xmlns='jabber:client' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='bogus' last-updated='1700000001'/>
                </push>
            </message>"#;
        let invalid_updated = r#"<message xmlns='jabber:client' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='soon'/>
                </push>
            </message>"#;
        let invalid_reply_count = r#"<message xmlns='jabber:client' type='headline'>
                <push xmlns='urn:waddle:inbox:0'>
                    <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.example.com' id='sid-1' unread='2'/>
                    <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='1700000001' reply-count='many'/>
                </push>
            </message>"#;

        for xml in [invalid_kind, invalid_updated, invalid_reply_count] {
            let element: Element = xml.parse().unwrap();
            assert_eq!(parse_inbox_stream_message(&element), None);
        }
    }

    #[test]
    fn parse_query_response_uses_embedded_mam_queryid() {
        let xml = r#"<message xmlns='jabber:client'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1' unread='1'/>
                <result xmlns='urn:xmpp:mam:2' queryid='q-mam' id='mam-1'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();
        let entry = parse_inbox_stream_message(&element).expect("entry");
        assert_eq!(entry.source, InboxStreamEntrySource::QueryResponse);
        assert_eq!(entry.query_id.as_deref(), Some("q-mam"));
        assert_eq!(entry.partner, "alice@example.com");
    }

    #[test]
    fn parse_query_response_without_queryid_is_not_a_push() {
        let xml = r#"<message xmlns='jabber:client'>
                <entry xmlns='urn:xmpp:inbox:1' jid='alice@example.com' id='mam-1' unread='1'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();
        let entry = parse_inbox_stream_message(&element).expect("entry");
        assert_eq!(entry.source, InboxStreamEntrySource::QueryResponse);
        assert_eq!(entry.query_id, None);
    }
}
