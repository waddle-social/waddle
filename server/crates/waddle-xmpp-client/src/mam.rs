//! MAM (XEP-0313) history queries for room and DM history.
//!
//! Exposes MAM parsers and page types for every build. With the `native`
//! feature enabled, also implements the MAM query helper on the native client
//! handle to send IQ queries, collect result stanzas, and return a structured
//! [`MamPage`] when the server signals completion via `<fin/>`.

use chrono::{DateTime, Utc};
use minidom::Element;
use waddle_xmpp_core::mam::{
    DELAY_NS, FORWARD_NS, FULLTEXT_MAM_FIELD, MAM_NS, RSM_NS, WADDLE_MAM_THREAD_FIELD,
};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::time::Duration;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::sync::broadcast;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::time::timeout;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use uuid::Uuid;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::error::{ClientError, ClientResult};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::event::ClientEvent;

const CLIENT_NS: &str = "jabber:client";
const DATA_FORMS_NS: &str = "jabber:x:data";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub const MAM_START_FIELD: &str = "start";
pub const MAM_END_FIELD: &str = "end";

/// A page of archived messages plus RSM pagination info.
#[derive(Debug, Clone)]
pub struct MamPage {
    pub messages: Vec<ArchivedMessage>,
    pub rsm: RsmPageInfo,
    pub query_id: String,
    /// True when the server sent `<fin complete='true'/>`.
    pub is_complete: bool,
}

/// One archived message unwrapped from a MAM result stanza.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedMessage {
    pub mam_id: String,
    pub query_id: Option<String>,
    pub stanza_id: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub message_type: String,
    pub body: Option<String>,
    pub thread: Option<String>,
    /// XEP-0201 nested-thread parent. Populated from the `parent`
    /// attribute on the inner `<thread/>` element via the canonical
    /// `crate::xep::thread::parse_thread` helper. `None` for root
    /// threads or messages without a `<thread/>`.
    pub parent_thread_id: Option<String>,
    /// XEP-0045 MUC real JID from archived `<x><item jid='...'/></x>` payloads.
    pub author_real_jid: Option<String>,
    /// Raw inner `<message>` element for full parsing by the messaging module.
    pub inner: Element,
}

/// RSM (XEP-0059) pagination info from a MAM `<fin/>`.
#[derive(Debug, Clone, Default)]
pub struct RsmPageInfo {
    pub first: Option<String>,
    pub last: Option<String>,
    pub count: Option<u32>,
    pub index: Option<u32>,
    pub is_complete: bool,
}

// ── Extension trait ──────────────────────────────────────────────────────────

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait MamExt {
    /// Fetch archived messages for a MUC room.
    fn fetch_room_history<'a>(
        &'a self,
        room_jid: &'a str,
        max: u32,
        before: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;

    /// Fetch archived messages for a 1:1 DM conversation.
    fn fetch_dm_history<'a>(
        &'a self,
        peer_jid: &'a str,
        max: u32,
        before: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<MamPage>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl MamExt for ClientHandle {
    async fn fetch_room_history(
        &self,
        room_jid: &str,
        max: u32,
        before: Option<&str>,
    ) -> ClientResult<MamPage> {
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_mam_iq(&iq_id, &query_id, max, before, None, Some(room_jid));
        run_mam_query(self, iq, &query_id).await
    }

    async fn fetch_dm_history(
        &self,
        peer_jid: &str,
        max: u32,
        before: Option<&str>,
    ) -> ClientResult<MamPage> {
        let query_id = Uuid::new_v4().to_string();
        let iq_id = Uuid::new_v4().to_string();

        let iq = build_mam_iq(&iq_id, &query_id, max, before, Some(peer_jid), None);
        run_mam_query(self, iq, &query_id).await
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Build a MAM query IQ element.
///
/// * `with_jid` — set as the `<with>` data form field (DM queries).
/// * `to_jid`   — set as the `to` attribute on the IQ (room queries).
pub fn build_mam_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    before: Option<&str>,
    with_jid: Option<&str>,
    to_jid: Option<&str>,
) -> Element {
    build_mam_iq_extended(
        iq_id,
        query_id,
        max,
        before.or(Some("")),
        None,
        with_jid,
        to_jid,
        None,
        None,
        None,
        None,
    )
}

fn build_form_field(var: &str, value: &str) -> Element {
    Element::builder("field", DATA_FORMS_NS)
        .attr("var", var)
        .append(
            Element::builder("value", DATA_FORMS_NS)
                .append(value)
                .build(),
        )
        .build()
}

fn bare_jid(value: &str) -> &str {
    value.split('/').next().unwrap_or(value)
}

fn parse_archived_author_real_jid(inner: &Element) -> Option<String> {
    inner
        .get_child("x", NS_MUC_USER)
        .and_then(|payload| payload.get_child("item", NS_MUC_USER))
        .and_then(|item| item.attr("jid"))
        .map(|jid| bare_jid(jid).to_string())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Phase 3 API shape is required by the wasm bindings and TS parity"
)]
pub fn build_mam_iq_extended(
    iq_id: &str,
    query_id: &str,
    max: u32,
    before: Option<&str>,
    after: Option<&str>,
    with_jid: Option<&str>,
    to_jid: Option<&str>,
    thread_id: Option<&str>,
    fulltext: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
) -> Element {
    let mut rsm = Element::builder("set", RSM_NS).append(
        Element::builder("max", RSM_NS)
            .append(max.to_string())
            .build(),
    );
    if let Some(before) = before {
        rsm = rsm.append(Element::builder("before", RSM_NS).append(before).build());
    }
    if let Some(after) = after {
        rsm = rsm.append(Element::builder("after", RSM_NS).append(after).build());
    }

    let mut form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
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
        );
    if let Some(with_jid) = with_jid {
        form = form.append(build_form_field("with", with_jid));
    }
    if let Some(thread_id) = thread_id {
        form = form.append(build_form_field(WADDLE_MAM_THREAD_FIELD, thread_id));
    }
    if let Some(fulltext) = fulltext {
        form = form.append(build_form_field(FULLTEXT_MAM_FIELD, fulltext));
    }
    if let Some(start) = start {
        form = form.append(build_form_field(MAM_START_FIELD, start));
    }
    if let Some(end) = end {
        form = form.append(build_form_field(MAM_END_FIELD, end));
    }

    let query = Element::builder("query", MAM_NS)
        .attr("queryid", query_id)
        .append(form.build())
        .append(rsm.build())
        .build();

    let mut iq = Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", iq_id)
        .append(query);
    if let Some(to_jid) = to_jid {
        iq = iq.attr("to", to_jid);
    }
    iq.build()
}

/// Subscribe to the event bus, send the IQ, collect MAM result messages until
/// the IQ correlation resolves, then assemble and return a [`MamPage`].
///
/// The collector runs concurrently with `send_iq` via `tokio::select!`: MAM
/// `<message>` result stanzas arrive on the broadcast bus while the IQ's
/// `<fin/>` response resolves the `send_iq` oneshot. Once the oneshot resolves,
/// the `select!` exits the main loop and we drain any residual results still
/// buffered in the receiver — the driver dispatches transport events in order,
/// so by the time `<fin/>` reaches us every prior MAM result is already in our
/// receiver's ring.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
async fn run_mam_query(
    handle: &ClientHandle,
    iq: Element,
    query_id: &str,
) -> ClientResult<MamPage> {
    // Subscribe BEFORE sending so we don't miss any result messages.
    let mut rx = handle.events();
    let query_id_owned = query_id.to_string();

    let query = async {
        let mut messages: Vec<ArchivedMessage> = Vec::new();
        let mut send_iq_fut = Box::pin(handle.send_iq(iq));

        let fin_el = loop {
            tokio::select! {
                result = &mut send_iq_fut => break result?,
                event = rx.recv() => match event {
                    Ok(ClientEvent::MamResult(archived))
                        if archived.query_id.as_deref() == Some(&query_id_owned) =>
                    {
                        messages.push(archived);
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(ClientError::Disconnected);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        };

        // Drain any MAM results that landed in the buffer between the final
        // `broadcast.send` and `send_iq` winning the select. Sequential
        // driver dispatch guarantees every prior MAM result is buffered
        // before the IQ result resolves the oneshot.
        loop {
            match rx.try_recv() {
                Ok(ClientEvent::MamResult(archived))
                    if archived.query_id.as_deref() == Some(&query_id_owned) =>
                {
                    messages.push(archived);
                }
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }

        Ok::<_, ClientError>((fin_el, messages))
    };

    let (fin_el, messages) = timeout(Duration::from_secs(30), query)
        .await
        .map_err(|_| ClientError::Disconnected)??;

    let (rsm, is_complete) = parse_fin_from_iq_result(&fin_el);

    Ok(MamPage {
        messages,
        rsm,
        query_id: query_id.to_string(),
        is_complete,
    })
}

/// Extract RSM and completeness from the IQ result element wrapping `<fin/>`.
pub fn parse_fin_from_iq_result(iq_result: &Element) -> (RsmPageInfo, bool) {
    // The result may be the raw <fin/> (if send_iq returns it directly) or
    // an <iq type='result'> wrapping a <fin/> child.
    let fin = if iq_result.name() == "fin" && iq_result.ns() == MAM_NS {
        Some(iq_result)
    } else {
        iq_result.get_child("fin", MAM_NS)
    };

    match fin.and_then(parse_mam_fin) {
        Some((rsm, complete)) => (rsm, complete),
        None => (RsmPageInfo::default(), false),
    }
}

// ── Public parse helpers ─────────────────────────────────────────────────────

/// Parse a MAM result `<message>` wrapper into an [`ArchivedMessage`].
///
/// Returns `None` if the element is not a MAM result message.
pub fn parse_mam_result(element: &Element) -> Option<ArchivedMessage> {
    if element.name() != "message" {
        return None;
    }

    let result = element.get_child("result", MAM_NS)?;

    let mam_id = result.attr("id")?.to_string();
    let query_id = result.attr("queryid").map(str::to_string);

    let forwarded = result.get_child("forwarded", FORWARD_NS)?;

    let delay = forwarded.get_child("delay", DELAY_NS);
    let timestamp = delay
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let inner = forwarded
        .children()
        .find(|c| c.name() == "message")?
        .clone();

    let from = inner.attr("from").map(str::to_string);
    let to = inner.attr("to").map(str::to_string);
    let message_type = inner.attr("type").unwrap_or("normal").to_string();

    let body = inner
        .get_child("body", CLIENT_NS)
        .or_else(|| inner.get_child("body", ""))
        .map(|b| b.text());

    // XEP-0201: parse via the canonical `crate::xep::thread::parse_thread`
    // helper so the optional `parent` attribute is surfaced as a typed
    // field instead of being recoverable only via re-parsing `inner`
    // downstream of FFI.
    let thread_ref = crate::xep::thread::parse_thread(&inner);
    let thread = thread_ref.as_ref().map(|t| t.id.clone());
    let parent_thread_id = thread_ref.as_ref().and_then(|t| t.parent.clone());

    // XEP-0359 stanza-id embedded in the inner message.
    let stanza_id = inner
        .get_child("stanza-id", "urn:xmpp:sid:0")
        .and_then(|s| s.attr("id"))
        .map(str::to_string);
    let author_real_jid = parse_archived_author_real_jid(&inner);

    Some(ArchivedMessage {
        mam_id,
        query_id,
        stanza_id,
        timestamp,
        from,
        to,
        message_type,
        body,
        thread,
        parent_thread_id,
        author_real_jid,
        inner,
    })
}

/// Parse a MAM `<fin/>` element into [`RsmPageInfo`] and a completeness flag.
///
/// Returns `None` if the element is not a MAM `<fin/>`.
pub fn parse_mam_fin(element: &Element) -> Option<(RsmPageInfo, bool)> {
    if element.name() != "fin" || element.ns() != MAM_NS {
        return None;
    }

    let is_complete = element.attr("complete") == Some("true");

    let set = element.get_child("set", RSM_NS);

    let first = set
        .and_then(|s| s.get_child("first", RSM_NS))
        .map(|e| e.text());

    let last = set
        .and_then(|s| s.get_child("last", RSM_NS))
        .map(|e| e.text());

    let count = set
        .and_then(|s| s.get_child("count", RSM_NS))
        .and_then(|e| e.text().parse::<u32>().ok());

    let index = set
        .and_then(|s| s.get_child("first", RSM_NS))
        .and_then(|e| e.attr("index"))
        .and_then(|v| v.parse::<u32>().ok());

    Some((
        RsmPageInfo {
            first,
            last,
            count,
            index,
            is_complete,
        },
        is_complete,
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::Datelike;

    use super::*;

    fn make_mam_result_message(
        mam_id: &str,
        query_id: &str,
        stamp: &str,
        from: &str,
        msg_type: &str,
        body: &str,
    ) -> Element {
        let inner_message = Element::builder("message", CLIENT_NS)
            .attr("from", from)
            .attr("type", msg_type)
            .append(Element::builder("body", CLIENT_NS).append(body).build())
            .build();

        let delay = Element::builder("delay", DELAY_NS)
            .attr("stamp", stamp)
            .build();

        let forwarded = Element::builder("forwarded", FORWARD_NS)
            .append(delay)
            .append(inner_message)
            .build();

        let result = Element::builder("result", MAM_NS)
            .attr("id", mam_id)
            .attr("queryid", query_id)
            .append(forwarded)
            .build();

        Element::builder("message", CLIENT_NS)
            .append(result)
            .build()
    }

    fn make_mam_fin(
        complete: bool,
        first: Option<&str>,
        last: Option<&str>,
        count: Option<u32>,
    ) -> Element {
        let mut set_builder = Element::builder("set", RSM_NS);

        if let Some(f) = first {
            set_builder = set_builder.append(Element::builder("first", RSM_NS).append(f).build());
        }
        if let Some(l) = last {
            set_builder = set_builder.append(Element::builder("last", RSM_NS).append(l).build());
        }
        if let Some(c) = count {
            set_builder = set_builder.append(
                Element::builder("count", RSM_NS)
                    .append(c.to_string())
                    .build(),
            );
        }

        let mut fin_builder = Element::builder("fin", MAM_NS);
        if complete {
            fin_builder = fin_builder.attr("complete", "true");
        }
        fin_builder.append(set_builder.build()).build()
    }

    #[test]
    fn parse_mam_result_happy_path() {
        let el = make_mam_result_message(
            "mam-id-1",
            "qid-42",
            "2024-01-01T12:00:00Z",
            "alice@example.com/res",
            "chat",
            "Hello world",
        );

        let archived = parse_mam_result(&el).expect("should parse");

        assert_eq!(archived.mam_id, "mam-id-1");
        assert_eq!(archived.query_id.as_deref(), Some("qid-42"));
        assert_eq!(archived.message_type, "chat");
        assert_eq!(archived.body.as_deref(), Some("Hello world"));
        assert_eq!(archived.from.as_deref(), Some("alice@example.com/res"));
        assert!(archived.timestamp.is_some());
        let ts = archived.timestamp.unwrap();
        assert_eq!(ts.year(), 2024);
    }

    #[test]
    fn parse_mam_result_extracts_archived_author_real_jid() {
        let inner = Element::builder("message", CLIENT_NS)
            .attr("from", "room@muc.example.com/alice")
            .attr("type", "groupchat")
            .append(Element::builder("body", CLIENT_NS).append("Hello world").build())
            .append(
                Element::builder("x", NS_MUC_USER)
                    .append(
                        Element::builder("item", NS_MUC_USER)
                            .attr("jid", "alice@example.com/phone")
                            .build(),
                    )
                    .build(),
            )
            .build();
        let delay = Element::builder("delay", DELAY_NS)
            .attr("stamp", "2024-01-01T12:00:00Z")
            .build();
        let forwarded = Element::builder("forwarded", FORWARD_NS)
            .append(delay)
            .append(inner)
            .build();
        let result = Element::builder("result", MAM_NS)
            .attr("id", "mam-id-2")
            .attr("queryid", "qid-43")
            .append(forwarded)
            .build();
        let el = Element::builder("message", CLIENT_NS).append(result).build();

        let archived = parse_mam_result(&el).expect("should parse");

        assert_eq!(archived.author_real_jid.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn xep_0201_parses_archived_message_with_thread_parent() {
        // Locks the typed-parent surface on the client `ArchivedMessage`:
        // a MAM result carrying `<thread parent='X'>id</thread>` populates
        // both `thread` and `parent_thread_id` instead of dropping parent.
        // FFI consumers (Swift/Kotlin) read `archived.parent_thread_id`
        // directly via `archived_to_ffi`.
        let inner = Element::builder("message", CLIENT_NS)
            .attr("from", "alice@example.com/web")
            .attr("type", "chat")
            .append(Element::builder("body", CLIENT_NS).append("hi").build())
            .append(
                Element::builder("thread", CLIENT_NS)
                    .attr("parent", "root-thread")
                    .append("child-thread")
                    .build(),
            )
            .build();
        let forwarded = Element::builder("forwarded", "urn:xmpp:forward:0")
            .append(
                Element::builder("delay", "urn:xmpp:delay")
                    .attr("stamp", "2024-01-01T12:00:00Z")
                    .build(),
            )
            .append(inner)
            .build();
        let result = Element::builder("message", CLIENT_NS)
            .attr("type", "normal")
            .append(
                Element::builder("result", MAM_NS)
                    .attr("queryid", "q1")
                    .attr("id", "mam-1")
                    .append(forwarded)
                    .build(),
            )
            .build();

        let archived = parse_mam_result(&result).expect("should parse");
        assert_eq!(archived.thread.as_deref(), Some("child-thread"));
        assert_eq!(archived.parent_thread_id.as_deref(), Some("root-thread"));
    }

    #[test]
    fn xep_0201_parses_archived_message_without_thread_parent() {
        // Root-only thread: parent_thread_id stays None.
        let inner = Element::builder("message", CLIENT_NS)
            .attr("from", "alice@example.com/web")
            .attr("type", "chat")
            .append(Element::builder("body", CLIENT_NS).append("hi").build())
            .append(
                Element::builder("thread", CLIENT_NS)
                    .append("root-thread")
                    .build(),
            )
            .build();
        let forwarded = Element::builder("forwarded", "urn:xmpp:forward:0")
            .append(
                Element::builder("delay", "urn:xmpp:delay")
                    .attr("stamp", "2024-01-01T12:00:00Z")
                    .build(),
            )
            .append(inner)
            .build();
        let result = Element::builder("message", CLIENT_NS)
            .attr("type", "normal")
            .append(
                Element::builder("result", MAM_NS)
                    .attr("queryid", "q1")
                    .attr("id", "mam-1")
                    .append(forwarded)
                    .build(),
            )
            .build();

        let archived = parse_mam_result(&result).expect("should parse");
        assert_eq!(archived.thread.as_deref(), Some("root-thread"));
        assert_eq!(archived.parent_thread_id, None);
    }

    #[test]
    fn parse_mam_result_ignores_non_mam_message() {
        let plain = Element::builder("message", CLIENT_NS)
            .attr("type", "chat")
            .append(Element::builder("body", CLIENT_NS).append("plain").build())
            .build();

        assert!(parse_mam_result(&plain).is_none());
    }

    #[test]
    fn parse_mam_fin_complete() {
        let fin = make_mam_fin(true, Some("first-id"), Some("last-id"), Some(42));
        let (rsm, is_complete) = parse_mam_fin(&fin).expect("should parse");

        assert!(is_complete);
        assert!(rsm.is_complete);
        assert_eq!(rsm.first.as_deref(), Some("first-id"));
        assert_eq!(rsm.last.as_deref(), Some("last-id"));
        assert_eq!(rsm.count, Some(42));
    }

    #[test]
    fn parse_mam_fin_partial() {
        let fin = make_mam_fin(false, Some("a"), Some("b"), None);
        let (rsm, is_complete) = parse_mam_fin(&fin).expect("should parse");

        assert!(!is_complete);
        assert!(!rsm.is_complete);
        assert_eq!(rsm.first.as_deref(), Some("a"));
        assert_eq!(rsm.last.as_deref(), Some("b"));
        assert_eq!(rsm.count, None);
    }

    #[test]
    fn rsm_page_info_defaults() {
        let info = RsmPageInfo::default();
        assert!(info.first.is_none());
        assert!(info.last.is_none());
        assert!(info.count.is_none());
        assert!(info.index.is_none());
        assert!(!info.is_complete);
    }

    #[test]
    fn build_mam_iq_extended_supports_thread_fulltext_and_after() {
        let iq = build_mam_iq_extended(
            "iq-1",
            "query-1",
            25,
            None,
            Some("after-1"),
            Some("alice@example.com"),
            Some("room@muc.example.com"),
            Some("thread-42"),
            Some("needle"),
            Some("2024-01-01T00:00:00Z"),
            Some("2024-01-31T23:59:59Z"),
        );
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let form = query.get_child("x", DATA_FORMS_NS).expect("form child");
        let fields: Vec<(String, String)> = form
            .children()
            .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
            .filter_map(|child| {
                Some((
                    child.attr("var")?.to_string(),
                    child.get_child("value", DATA_FORMS_NS)?.text(),
                ))
            })
            .collect();
        assert!(fields.contains(&("with".to_string(), "alice@example.com".to_string())));
        assert!(fields.contains(&(WADDLE_MAM_THREAD_FIELD.to_string(), "thread-42".to_string())));
        assert!(fields.contains(&(FULLTEXT_MAM_FIELD.to_string(), "needle".to_string())));
        assert!(fields.contains(&(
            MAM_START_FIELD.to_string(),
            "2024-01-01T00:00:00Z".to_string()
        )));
        assert!(fields.contains(&(
            MAM_END_FIELD.to_string(),
            "2024-01-31T23:59:59Z".to_string()
        )));
        let set = query.get_child("set", RSM_NS).expect("rsm set");
        assert_eq!(
            set.get_child("after", RSM_NS).map(|child| child.text()),
            Some("after-1".to_string())
        );
        assert_eq!(iq.attr("to"), Some("room@muc.example.com"));
    }

    #[test]
    fn build_mam_iq_extended_supports_latest_before_marker() {
        let iq = build_mam_iq_extended(
            "iq-2",
            "query-2",
            10,
            Some(""),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let before = iq
            .get_child("query", MAM_NS)
            .and_then(|query| query.get_child("set", RSM_NS))
            .and_then(|set| set.get_child("before", RSM_NS))
            .expect("before child");
        assert_eq!(before.text(), "");
    }

    #[test]
    fn build_mam_iq_preserves_existing_before_behavior() {
        let iq = build_mam_iq(
            "iq-3",
            "query-3",
            50,
            Some("last-id"),
            Some("bob@example.com"),
            None,
        );
        let query = iq.get_child("query", MAM_NS).expect("query child");
        let set = query.get_child("set", RSM_NS).expect("set child");
        assert_eq!(
            set.get_child("before", RSM_NS).map(|child| child.text()),
            Some("last-id".to_string())
        );
        let fields: Vec<(String, String)> = query
            .get_child("x", DATA_FORMS_NS)
            .expect("form child")
            .children()
            .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
            .filter_map(|child| {
                Some((
                    child.attr("var")?.to_string(),
                    child.get_child("value", DATA_FORMS_NS)?.text(),
                ))
            })
            .collect();
        assert!(fields.contains(&("with".to_string(), "bob@example.com".to_string())));
    }

    // ── Query orchestration integration tests ────────────────────────────────

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    mod query {
        use std::sync::{Arc, RwLock};
        use std::time::Duration;

        use minidom::Element;
        use tokio::sync::{broadcast, mpsc};
        use tokio::time::timeout;

        use super::super::{ArchivedMessage, MamExt, CLIENT_NS};
        use crate::client::ClientHandle;
        use crate::command::XmppCommand;
        use crate::event::ClientEvent;
        use crate::state::SessionSnapshot;
        use waddle_xmpp_core::mam::{MAM_NS, RSM_NS};

        fn make_handle() -> (
            ClientHandle,
            mpsc::Receiver<XmppCommand>,
            broadcast::Sender<ClientEvent>,
        ) {
            let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(4);
            let (evt_tx, _) = broadcast::channel::<ClientEvent>(64);
            let state = Arc::new(RwLock::new(SessionSnapshot::new()));
            let handle = ClientHandle::from_parts(cmd_tx, evt_tx.clone(), state);
            (handle, cmd_rx, evt_tx)
        }

        fn build_archived(mam_id: &str, query_id: &str, body: &str) -> ArchivedMessage {
            ArchivedMessage {
                mam_id: mam_id.to_string(),
                query_id: Some(query_id.to_string()),
                stanza_id: Some(mam_id.to_string()),
                timestamp: None,
                from: Some("room@muc.example.com/alice".to_string()),
                to: Some("alice@example.com/res".to_string()),
                parent_thread_id: None,
                message_type: "groupchat".to_string(),
                body: Some(body.to_string()),
                thread: None,
                author_real_jid: None,
                inner: Element::builder("message", CLIENT_NS).build(),
            }
        }

        fn build_fin_iq(iq_id: &str, first: &str, last: &str, count: u32) -> Element {
            let set = Element::builder("set", RSM_NS)
                .append(Element::builder("first", RSM_NS).append(first).build())
                .append(Element::builder("last", RSM_NS).append(last).build())
                .append(
                    Element::builder("count", RSM_NS)
                        .append(count.to_string())
                        .build(),
                )
                .build();

            let fin = Element::builder("fin", MAM_NS)
                .attr("complete", "true")
                .append(set)
                .build();

            Element::builder("iq", CLIENT_NS)
                .attr("type", "result")
                .attr("id", iq_id)
                .append(fin)
                .build()
        }

        #[tokio::test(flavor = "current_thread")]
        async fn fetch_room_history_collects_results_then_resolves_on_fin() {
            let (handle, mut cmd_rx, evt_tx) = make_handle();

            tokio::spawn(async move {
                let cmd = cmd_rx.recv().await.expect("driver received cmd");
                let (stanza, responder) = match cmd {
                    XmppCommand::SendIq { stanza, responder } => (stanza, responder),
                    other => panic!("unexpected command: {other:?}"),
                };

                let query_id = stanza
                    .get_child("query", MAM_NS)
                    .and_then(|q| q.attr("queryid"))
                    .expect("queryid attribute on <query>")
                    .to_string();
                let iq_id = stanza.attr("id").expect("id attribute on <iq>").to_string();

                // Broadcast MAM results carrying the correct queryid, plus one
                // result with a foreign queryid that must be filtered out.
                for i in 0..5u32 {
                    evt_tx
                        .send(ClientEvent::MamResult(build_archived(
                            &format!("mam-{i}"),
                            &query_id,
                            &format!("hello {i}"),
                        )))
                        .expect("broadcast MAM result");
                }

                evt_tx
                    .send(ClientEvent::MamResult(build_archived(
                        "mam-other",
                        "some-other-query",
                        "noise",
                    )))
                    .expect("broadcast foreign MAM result");

                // Resolve send_iq with the <fin/> IQ — must be after the
                // MAM broadcasts to match XEP-0313 server behaviour.
                responder
                    .send(Ok(build_fin_iq(&iq_id, "mam-0", "mam-4", 5)))
                    .expect("responder not dropped");
            });

            let page = timeout(
                Duration::from_secs(2),
                handle.fetch_room_history("room@muc.example.com", 50, None),
            )
            .await
            .expect("run_mam_query must resolve once <fin/> arrives (not block for 30s)")
            .expect("fetch_room_history succeeds");

            assert_eq!(
                page.messages.len(),
                5,
                "only messages matching the query_id must be collected"
            );
            for (i, msg) in page.messages.iter().enumerate() {
                assert_eq!(msg.mam_id, format!("mam-{i}"));
                assert_eq!(msg.body.as_deref(), Some(format!("hello {i}").as_str()));
            }
            assert!(page.is_complete, "<fin complete='true'/> must propagate");
            assert_eq!(page.rsm.first.as_deref(), Some("mam-0"));
            assert_eq!(page.rsm.last.as_deref(), Some("mam-4"));
            assert_eq!(page.rsm.count, Some(5));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn fetch_room_history_propagates_send_iq_error() {
            let (handle, mut cmd_rx, _evt_tx) = make_handle();

            tokio::spawn(async move {
                let cmd = cmd_rx.recv().await.expect("driver received cmd");
                let XmppCommand::SendIq { responder, .. } = cmd else {
                    panic!("expected SendIq");
                };
                // Drop the responder to simulate a disconnected driver.
                drop(responder);
            });

            let result = timeout(
                Duration::from_secs(2),
                handle.fetch_room_history("room@muc.example.com", 50, None),
            )
            .await
            .expect("must not hang the full 30s timeout");

            assert!(result.is_err(), "dropped responder must surface as error");
        }
    }
}
