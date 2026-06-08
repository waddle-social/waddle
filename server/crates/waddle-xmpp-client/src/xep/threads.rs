//! Client-side typed value + IQ builder/parser for `urn:waddle:threads:0`.
//!
//! Mirrors the wire shape defined by the server in
//! `waddle-server/src/threads/wire.rs`. The chat client uses the IQ
//! builder/parser to drive the global Threads view.

use crate::xep::call_thread::{CallThreadKind, CallThreadMedia};
use chrono::{DateTime, SecondsFormat, Utc};
use jid::BareJid;
use minidom::Element;

/// Namespace for the threads-view query and response.
pub const NS_THREADS: &str = "urn:waddle:threads:0";

const NS_CLIENT: &str = "jabber:client";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

/// Call-thread anchor summary carried by a `<thread>` entry.
///
/// Present only when the thread is a call-thread anchor (the server
/// emits a `<call kind=… media=…/>` child).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCallSummary {
    pub kind: CallThreadKind,
    pub media: CallThreadMedia,
}

/// Ended-call summary carried by a `<thread>` entry.
///
/// Present only when the anchored call has ended (the server emits a
/// `<call-ended ended=… duration=…/>` child). Both fields are kept as
/// wire strings (RFC 3339 timestamp / ISO-8601 duration) so they cross
/// the wasm boundary unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCallEndedSummary {
    pub ended: String,
    pub duration: String,
}

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
    /// Call-thread anchor summary, present when this thread anchors a call.
    pub call_thread: Option<ThreadCallSummary>,
    /// Ended-call summary, present when the anchored call has ended.
    pub call_thread_ended: Option<ThreadCallEndedSummary>,
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

/// Status filter for a threads query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadStatusFilter {
    #[default]
    All,
    Unread,
    Following,
}

impl ThreadStatusFilter {
    fn as_attr(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Unread => Some("unread"),
            Self::Following => Some("following"),
        }
    }
}

/// Sort order for a threads query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadSort {
    #[default]
    Recent,
    Unread,
    Replies,
}

impl ThreadSort {
    fn as_attr(self) -> Option<&'static str> {
        match self {
            Self::Recent => None,
            Self::Unread => Some("unread"),
            Self::Replies => Some("replies"),
        }
    }
}

/// Request options for `urn:waddle:threads:0`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchThreadsQuery<'a> {
    pub page_size: Option<u32>,
    pub after_cursor: Option<&'a str>,
    pub status: ThreadStatusFilter,
    pub active_since: Option<DateTime<Utc>>,
    pub channel: Option<BareJid>,
    pub search: Option<&'a str>,
    pub sort: ThreadSort,
}

/// Build a `<iq type='get'>` requesting the user's threads.
pub fn build_fetch_threads_iq(request_id: &str, opts: &FetchThreadsQuery<'_>) -> Element {
    let mut query_builder = Element::builder("query", NS_THREADS);
    if let Some(status) = opts.status.as_attr() {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("status").to_owned(), status);
    }
    if let Some(active_since) = opts.active_since {
        query_builder = query_builder.attr(
            minidom::rxml::xml_ncname!("active-since").to_owned(),
            active_since.to_rfc3339_opts(SecondsFormat::Secs, true),
        );
    }
    if let Some(ref channel) = opts.channel {
        query_builder = query_builder.attr(
            minidom::rxml::xml_ncname!("channel").to_owned(),
            channel.to_string(),
        );
    }
    if let Some(search) = opts.search {
        let trimmed = search.trim();
        if !trimmed.is_empty() {
            query_builder =
                query_builder.attr(minidom::rxml::xml_ncname!("search").to_owned(), trimmed);
        }
    }
    if let Some(sort) = opts.sort.as_attr() {
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("sort").to_owned(), sort);
    }

    let mut query = query_builder.build();
    if opts.page_size.is_some() || opts.after_cursor.is_some() {
        let mut set = Element::builder("set", NS_RSM).build();
        if let Some(max) = opts.page_size {
            let mut max_el = Element::builder("max", NS_RSM).build();
            max_el.append_text_node(max.to_string());
            set.append_child(max_el);
        }
        if let Some(after) = opts.after_cursor {
            let mut after_el = Element::builder("after", NS_RSM).build();
            after_el.append_text_node(after);
            set.append_child(after_el);
        }
        query.append_child(set);
    }

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
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
            call_thread: parse_call_summary(t),
            call_thread_ended: parse_call_ended_summary(t),
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

/// Parse the optional `<call kind=… media=…/>` child of a `<thread>`.
///
/// Returns `None` when the child is absent or carries an unknown/garbage
/// `kind`/`media` — a malformed call marker is treated as "not a call
/// thread" rather than failing the whole page.
fn parse_call_summary(thread: &Element) -> Option<ThreadCallSummary> {
    let call = thread.get_child("call", NS_THREADS)?;
    let kind = CallThreadKind::parse_token(call.attr("kind")?).ok()?;
    let media = CallThreadMedia::parse_tokens(call.attr("media")?).ok()?;
    Some(ThreadCallSummary { kind, media })
}

/// Parse the optional `<call-ended ended=… duration=…/>` child of a
/// `<thread>`. Returns `None` when the child or either attribute is absent.
fn parse_call_ended_summary(thread: &Element) -> Option<ThreadCallEndedSummary> {
    let ended_el = thread.get_child("call-ended", NS_THREADS)?;
    let ended = ended_el.attr("ended")?.to_owned();
    let duration = ended_el.attr("duration")?.to_owned();
    Some(ThreadCallEndedSummary { ended, duration })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_iq_has_correct_namespace_and_type() {
        let iq = build_fetch_threads_iq(
            "r-1",
            &FetchThreadsQuery {
                page_size: Some(25),
                after_cursor: Some("CUR"),
                ..Default::default()
            },
        );
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
        let iq = build_fetch_threads_iq("r-2", &FetchThreadsQuery::default());
        let query = iq.get_child("query", NS_THREADS).expect("query");
        assert!(query.get_child("set", NS_RSM).is_none());
        assert_eq!(query.attr("status"), None);
        assert_eq!(query.attr("sort"), None);
    }

    #[test]
    fn fetch_iq_includes_selected_filters() {
        let iq = build_fetch_threads_iq(
            "r-3",
            &FetchThreadsQuery {
                page_size: Some(50),
                status: ThreadStatusFilter::Unread,
                active_since: Some(
                    "2026-05-19T00:00:00Z"
                        .parse::<DateTime<Utc>>()
                        .expect("valid timestamp"),
                ),
                channel: Some("chat@muc.waddle.chat".parse().expect("valid JID")),
                search: Some(" notifications "),
                sort: ThreadSort::Replies,
                ..Default::default()
            },
        );
        let query = iq.get_child("query", NS_THREADS).expect("query");
        assert_eq!(query.attr("status"), Some("unread"));
        assert_eq!(query.attr("active-since"), Some("2026-05-19T00:00:00Z"));
        assert_eq!(query.attr("channel"), Some("chat@muc.waddle.chat"));
        assert_eq!(query.attr("search"), Some("notifications"));
        assert_eq!(query.attr("sort"), Some("replies"));
        assert!(query.get_child("set", NS_RSM).is_some());
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

    #[test]
    fn parse_extracts_call_thread_children() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                     <threads xmlns='urn:waddle:threads:0' total='2' unread-threads='0'>\
                       <thread channel='room@x' thread-id='call-1' \
                               last-stanza-id='S1' last-activity='2026-06-07T14:30:00Z' \
                               unread='0' reply-count='4' has-unread='false'>\
                         <call kind='muc' media='audio video'/>\
                         <call-ended ended='2026-06-07T14:35:00Z' duration='PT5M'/>\
                       </thread>\
                       <thread channel='room@x' thread-id='plain-1' \
                               last-stanza-id='S2' last-activity='2026-06-07T13:00:00Z' \
                               unread='0' reply-count='1' has-unread='false'/>\
                     </threads>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid XML");
        let page = parse_threads_response(&iq).expect("parses");
        assert_eq!(page.entries.len(), 2);

        let call = &page.entries[0];
        let summary = call.call_thread.expect("call-thread summary present");
        assert_eq!(summary.kind, CallThreadKind::Muc);
        assert_eq!(summary.media, CallThreadMedia::audio_video());
        let ended = call
            .call_thread_ended
            .as_ref()
            .expect("call-ended summary present");
        assert_eq!(ended.ended, "2026-06-07T14:35:00Z");
        assert_eq!(ended.duration, "PT5M");

        let plain = &page.entries[1];
        assert!(plain.call_thread.is_none());
        assert!(plain.call_thread_ended.is_none());
    }

    #[test]
    fn parse_ongoing_call_thread_has_no_ended_summary() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                     <threads xmlns='urn:waddle:threads:0' total='1' unread-threads='0'>\
                       <thread channel='room@x' thread-id='call-2' \
                               last-stanza-id='S1' last-activity='2026-06-07T14:30:00Z' \
                               unread='0' reply-count='0' has-unread='false'>\
                         <call kind='dm' media='audio'/>\
                       </thread>\
                     </threads>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid XML");
        let page = parse_threads_response(&iq).expect("parses");
        let entry = &page.entries[0];
        let summary = entry.call_thread.expect("call-thread summary present");
        assert_eq!(summary.kind, CallThreadKind::Dm);
        assert_eq!(summary.media, CallThreadMedia::audio_only());
        assert!(entry.call_thread_ended.is_none());
    }

    #[test]
    fn parse_ignores_call_marker_with_garbage_kind() {
        let xml = "<iq xmlns='jabber:client' type='result' id='r'>\
                     <threads xmlns='urn:waddle:threads:0' total='1' unread-threads='0'>\
                       <thread channel='room@x' thread-id='call-3' \
                               last-stanza-id='S1' last-activity='2026-06-07T14:30:00Z' \
                               unread='0' reply-count='0' has-unread='false'>\
                         <call kind='bogus' media='audio'/>\
                       </thread>\
                     </threads>\
                   </iq>";
        let iq: Element = xml.parse().expect("valid XML");
        let page = parse_threads_response(&iq).expect("parses");
        assert!(page.entries[0].call_thread.is_none());
    }
}
