//! Client-side parser for XEP-0430 inbox streamed entries.
//!
//! The server emits one `<message><entry xmlns='urn:xmpp:inbox:0'/></message>`
//! per matched conversation (optionally with an embedded
//! `<result xmlns='urn:xmpp:mam:2'>` carrying the forwarded last
//! archived stanza), followed by a closing `<iq type='result'><fin/></iq>`.
//! The driver uses this parser to recognise streamed entries by their
//! `queryid` and accumulate them against the matching pending query.

use minidom::Element;

/// Namespace constant — kept local to the client so the wasm bindings
/// don't need a transitive dep on `waddle-xmpp`.
pub const NS_INBOX: &str = "urn:xmpp:inbox:0";

/// Typed projection of one streamed `<entry/>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxStreamEntry {
    pub query_id: String,
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

/// Parse one streamed inbox `<message/>` into a typed entry.
///
/// Returns `None` for messages that don't carry an
/// `<entry xmlns='urn:xmpp:inbox:0'/>` child — those flow through the
/// other typed handlers (MAM/PEP/messaging) unchanged.
pub fn parse_inbox_stream_message(message: &Element) -> Option<InboxStreamEntry> {
    if message.name() != "message" {
        return None;
    }
    let entry = message.children().find(|c| c.is("entry", NS_INBOX))?;
    let query_id = entry.attr("queryid")?.to_string();
    let partner = entry.attr("jid")?.to_string();
    let last_stanza_id = entry.attr("id")?.to_string();
    let unread = entry
        .attr("unread")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some(InboxStreamEntry {
        query_id,
        partner,
        kind: entry.attr("kind").map(str::to_string),
        last_stanza_id,
        last_updated: entry.attr("last-updated").and_then(|v| v.parse().ok()),
        unread,
        preview: entry
            .attr("preview")
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        thread_id: entry
            .attr("thread")
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        thread_title: entry
            .attr("thread-title")
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        reply_count: entry.attr("reply-count").and_then(|v| v.parse().ok()),
        author: entry
            .attr("author")
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

/// Build the canonical `<inbox xmlns='urn:xmpp:inbox:0'/>` IQ-get
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
                <entry xmlns='urn:xmpp:inbox:0' queryid='q1' jid='alice@example.com' id='mam-1' unread='3' kind='direct' last-updated='1700000000' preview='hi'/>
            </message>"#;
        let element: Element = xml.parse().unwrap();
        let entry = parse_inbox_stream_message(&element).expect("entry");
        assert_eq!(entry.query_id, "q1");
        assert_eq!(entry.partner, "alice@example.com");
        assert_eq!(entry.unread, 3);
        assert_eq!(entry.last_stanza_id, "mam-1");
        assert_eq!(entry.preview.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_inbox_fin_with_rsm() {
        let xml = r#"<iq xmlns='jabber:client' type='result' id='q1'>
                <fin xmlns='urn:xmpp:inbox:0' total='3' unread='2' all-unread='7'>
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
}
