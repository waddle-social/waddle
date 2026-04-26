//! MAM (XEP-0313) history queries for room and DM history.
//!
//! Implements [`MamExt`] on [`ClientHandle`] to send MAM IQ queries,
//! collect the resulting `<message>` stanzas from the event stream, and
//! return a structured [`MamPage`] when the server signals completion via
//! the IQ result `<fin/>`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use minidom::Element;
use tokio::sync::broadcast;
use tokio::time::timeout;
use uuid::Uuid;
use waddle_xmpp_core::mam::{DELAY_NS, FORWARD_NS, MAM_NS, RSM_NS};

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult};
use crate::event::ClientEvent;

const CLIENT_NS: &str = "jabber:client";
const DATA_FORMS_NS: &str = "jabber:x:data";

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
fn build_mam_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    before: Option<&str>,
    with_jid: Option<&str>,
    to_jid: Option<&str>,
) -> Element {
    // <set xmlns='http://jabber.org/protocol/rsm'>
    let before_el = Element::builder("before", RSM_NS)
        .append(before.unwrap_or(""))
        .build();

    let rsm = Element::builder("set", RSM_NS)
        .append(
            Element::builder("max", RSM_NS)
                .append(max.to_string())
                .build(),
        )
        .append(before_el)
        .build();

    // <x xmlns='jabber:x:data' type='submit'>
    let form_type_field = Element::builder("field", DATA_FORMS_NS)
        .attr("var", "FORM_TYPE")
        .attr("type", "hidden")
        .append(
            Element::builder("value", DATA_FORMS_NS)
                .append(MAM_NS)
                .build(),
        )
        .build();

    let mut form_builder = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(form_type_field);

    if let Some(with) = with_jid {
        let with_field = Element::builder("field", DATA_FORMS_NS)
            .attr("var", "with")
            .append(
                Element::builder("value", DATA_FORMS_NS)
                    .append(with)
                    .build(),
            )
            .build();
        form_builder = form_builder.append(with_field);
    }

    let form = form_builder.build();

    // <query xmlns='urn:xmpp:mam:2' queryid='...'>
    let query = Element::builder("query", MAM_NS)
        .attr("queryid", query_id)
        .append(form)
        .append(rsm)
        .build();

    // <iq type='set' id='...' xmlns='jabber:client'>
    let mut iq_builder = Element::builder("iq", CLIENT_NS)
        .attr("type", "set")
        .attr("id", iq_id)
        .append(query);

    if let Some(to) = to_jid {
        iq_builder = iq_builder.attr("to", to);
    }

    iq_builder.build()
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
fn parse_fin_from_iq_result(iq_result: &Element) -> (RsmPageInfo, bool) {
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

    let thread = inner
        .get_child("thread", CLIENT_NS)
        .or_else(|| inner.get_child("thread", ""))
        .map(|t| t.text());

    // XEP-0359 stanza-id embedded in the inner message.
    let stanza_id = inner
        .get_child("stanza-id", "urn:xmpp:sid:0")
        .and_then(|s| s.attr("id"))
        .map(str::to_string);

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

    // ── Query orchestration integration tests ────────────────────────────────

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
                message_type: "groupchat".to_string(),
                body: Some(body.to_string()),
                thread: None,
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
