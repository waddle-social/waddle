//! Client-side typed value + IQ builder/parser for `urn:waddle:threads:0`.
//!
//! Mirrors the wire shape defined by the server in
//! `waddle-server/src/threads/wire.rs`. The chat client uses the IQ
//! builder/parser to drive the global Threads view.

use minidom::Element;

/// Namespace for the threads-view query and response.
pub const NS_THREADS: &str = "urn:waddle:threads:0";

const NS_CLIENT: &str = "jabber:client";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

/// One entry in a `<threads>` response.
///
/// All timestamps are RFC 3339 strings (timezone-safe across the
/// wasm boundary). The server's typed value uses `i64` seconds; the
/// conversion happens in the server's `build_thread_entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    pub channel: String,
    pub thread_id: String,
    pub last_stanza_id: String,
    pub last_activity: String,
    pub unread: u32,
    pub reply_count: u32,
    pub has_unread: bool,
    pub root_author: Option<String>,
    pub preview: Option<String>,
    pub thread_title: Option<String>,
}

/// Full response payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadsPage {
    pub total: u64,
    pub unread_threads: u64,
    pub entries: Vec<ThreadEntry>,
    /// Opaque RSM `<last>` cursor — pass to the next request to
    /// continue paginating.
    pub next_cursor: Option<String>,
}

/// Build a `<iq type='get'>` requesting the user's threads.
pub fn build_fetch_threads_iq(
    request_id: &str,
    page_size: Option<u32>,
    after_cursor: Option<&str>,
) -> Element {
    let mut query = Element::builder("query", NS_THREADS).build();
    if page_size.is_some() || after_cursor.is_some() {
        let mut set = Element::builder("set", NS_RSM).build();
        if let Some(max) = page_size {
            let mut max_el = Element::builder("max", NS_RSM).build();
            max_el.append_text_node(max.to_string());
            set.append_child(max_el);
        }
        if let Some(after) = after_cursor {
            let mut after_el = Element::builder("after", NS_RSM).build();
            after_el.append_text_node(after);
            set.append_child(after_el);
        }
        query.append_child(set);
    }

    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("id", request_id)
        .append(query)
        .build()
}

/// Parse the `<iq type='result'>` response into a typed `ThreadsPage`.
/// Returns `None` when the IQ does not carry a `<threads xmlns='urn:waddle:threads:0'/>`.
pub fn parse_threads_response(iq: &Element) -> Option<ThreadsPage> {
    let threads = iq.get_child("threads", NS_THREADS)?;
    let total: u64 = threads
        .attr("total")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let unread_threads: u64 = threads
        .attr("unread-threads")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let entries: Vec<ThreadEntry> = threads
        .children()
        .filter(|c| c.name() == "thread" && c.ns() == NS_THREADS)
        .map(|t| ThreadEntry {
            channel: t.attr("channel").unwrap_or("").to_string(),
            thread_id: t.attr("thread-id").unwrap_or("").to_string(),
            last_stanza_id: t.attr("last-stanza-id").unwrap_or("").to_string(),
            last_activity: t.attr("last-activity").unwrap_or("").to_string(),
            unread: t.attr("unread").and_then(|s| s.parse().ok()).unwrap_or(0),
            reply_count: t
                .attr("reply-count")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            has_unread: t.attr("has-unread") == Some("true"),
            root_author: t
                .get_child("root-author", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
            preview: t
                .get_child("preview", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
            thread_title: t
                .get_child("thread-title", NS_THREADS)
                .map(|e| e.text())
                .filter(|s| !s.is_empty()),
        })
        .collect();

    let next_cursor = threads
        .get_child("set", NS_RSM)
        .and_then(|s| s.get_child("last", NS_RSM))
        .map(|e| e.text())
        .filter(|s| !s.is_empty());

    Some(ThreadsPage {
        total,
        unread_threads,
        entries,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_iq_has_correct_namespace_and_type() {
        let iq = build_fetch_threads_iq("r-1", Some(25), Some("CUR"));
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("id"), Some("r-1"));
        let query = iq.get_child("query", NS_THREADS).expect("query");
        let set = query.get_child("set", NS_RSM).expect("rsm set");
        assert_eq!(
            set.get_child("max", NS_RSM).map(|e| e.text()),
            Some("25".into())
        );
        assert_eq!(
            set.get_child("after", NS_RSM).map(|e| e.text()),
            Some("CUR".into())
        );
    }

    #[test]
    fn fetch_iq_without_pagination_omits_set() {
        let iq = build_fetch_threads_iq("r-2", None, None);
        let query = iq.get_child("query", NS_THREADS).expect("query");
        assert!(query.get_child("set", NS_RSM).is_none());
    }

    #[test]
    fn parse_extracts_entries_and_cursor() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                     <threads xmlns='urn:waddle:threads:0' total='2' unread-threads='1'>\
                       <thread channel='room@x' thread-id='t1' \
                               last-stanza-id='S1' last-activity='2026-01-01T00:00:00Z' \
                               unread='2' reply-count='5' has-unread='true'>\
                         <preview>hi</preview>\
                       </thread>\
                       <thread channel='room@x' thread-id='t2' \
                               last-stanza-id='S2' last-activity='2025-12-31T00:00:00Z' \
                               unread='0' reply-count='3' has-unread='false'/>\
                       <set xmlns='http://jabber.org/protocol/rsm'>\
                         <last>LAST-CUR</last><count>2</count>\
                       </set>\
                     </threads>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid XML");
        let page = parse_threads_response(&iq).expect("parses");
        assert_eq!(page.total, 2);
        assert_eq!(page.unread_threads, 1);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].thread_id, "t1");
        assert!(page.entries[0].has_unread);
        assert_eq!(page.entries[0].preview.as_deref(), Some("hi"));
        assert!(!page.entries[1].has_unread);
        assert_eq!(page.next_cursor.as_deref(), Some("LAST-CUR"));
    }

    #[test]
    fn parse_returns_none_when_threads_missing() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'/>";
        let iq: Element = xml.parse().expect("valid XML");
        assert!(parse_threads_response(&iq).is_none());
    }
}
