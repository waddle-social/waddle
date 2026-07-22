//! Native [`ClientHandle`] extension for XEP-0430 inbox sync.
//!
//! Mirrors the wasm driver's pending-inbox-query reducer: the query
//! IQ id doubles as the embedded MAM `queryid`, streamed
//! [`ClientEvent::InboxStreamEntry`] query responses are folded into
//! the page, and the correlated `<iq type='result'><fin/></iq>`
//! resolves the fetch. Live pushes are never folded into a page —
//! they stay on the broadcast bus for application callbacks.

use crate::client::ClientHandle;
use crate::discovery::{build_waddle_inbox_mark_read_iq, WaddleInboxMarkRead};
use crate::error::{ClientError, ClientResult};
use crate::event::ClientEvent;
use crate::inbox::{
    build_inbox_query_iq_element, parse_inbox_fin, InboxPage, InboxStreamEntry,
    InboxStreamEntrySource,
};

/// XEP-0430 inbox verbs on the async [`ClientHandle`].
pub trait InboxExt {
    /// Run one streamed `<inbox xmlns='urn:xmpp:inbox:1'/>` query and
    /// resolve on the closing `<fin/>`.
    ///
    /// `unread_only` maps to the XEP-0430 `unread-only` attribute and
    /// `messages` to `messages` (whether the server embeds each
    /// conversation's last archived stanza as a MAM `<result/>`).
    ///
    /// Callers MUST NOT run two inbox queries concurrently on the
    /// same session: `messages='false'` responses carry no MAM
    /// `queryid`, so correlation relies on a single in-flight query —
    /// the same invariant the wasm driver enforces by deferring
    /// overlapping `SendInboxQuery` commands. The FFI verb serializes
    /// its calls behind a gate for this reason.
    fn fetch_inbox(
        &self,
        unread_only: bool,
        messages: bool,
    ) -> impl std::future::Future<Output = ClientResult<InboxPage>> + Send + '_;

    /// Mark one conversation (optionally one thread) read via the
    /// Waddle `urn:waddle:inbox:0` `<mark-read/>` IQ addressed to the
    /// account's own bare JID.
    fn mark_inbox_read<'a>(
        &'a self,
        partner: &'a str,
        thread: Option<&'a str>,
    ) -> impl std::future::Future<Output = ClientResult<()>> + Send + 'a;
}

/// Streamed entries belong to the page when they are query responses
/// correlated to this query: either the embedded MAM `queryid`
/// matches the IQ id, or the server streamed the entry without a
/// `queryid` (`messages='false'` responses carry no MAM `<result/>`),
/// in which case the single in-flight query claims it — the same
/// fallback the wasm driver applies to its lone pending query.
fn entry_belongs_to_query(entry: &InboxStreamEntry, query_id: &str) -> bool {
    entry.source == InboxStreamEntrySource::QueryResponse
        && (entry.query_id.as_deref() == Some(query_id) || entry.query_id.is_none())
}

/// Drain every already-buffered broadcast event without blocking,
/// folding correlated entries into `entries`. Used after the fin
/// resolves: the driver task broadcasts each streamed entry before it
/// resolves the fin IQ oneshot, so everything for this query is
/// already queued on the subscription by then.
fn collect_buffered_entries(
    events: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    query_id: &str,
    entries: &mut Vec<InboxStreamEntry>,
) {
    use tokio::sync::broadcast::error::TryRecvError;
    loop {
        match events.try_recv() {
            Ok(ClientEvent::InboxStreamEntry(entry)) => {
                if entry_belongs_to_query(&entry, query_id) {
                    entries.push(entry);
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
}

impl InboxExt for ClientHandle {
    async fn fetch_inbox(&self, unread_only: bool, messages: bool) -> ClientResult<InboxPage> {
        use tokio::sync::broadcast::error::RecvError;

        let query_id = uuid::Uuid::new_v4().to_string();
        // Subscribe BEFORE the IQ leaves so no streamed entry can slip
        // past between send and subscription.
        let mut events = self.events();
        let iq = build_inbox_query_iq_element(&query_id, unread_only, messages);
        let fin_fut = self.send_iq(iq);
        tokio::pin!(fin_fut);

        // Fold entries WHILE the fin IQ is pending: parking on
        // `send_iq` alone would leave every streamed entry sitting in
        // the fixed-capacity broadcast ring, where a large inbox (or
        // any concurrent event burst) overwrites the oldest rows
        // before the drain ever runs.
        let mut entries = Vec::new();
        let fin_iq = loop {
            tokio::select! {
                result = &mut fin_fut => break result?,
                event = events.recv() => match event {
                    Ok(ClientEvent::InboxStreamEntry(entry)) => {
                        if entry_belongs_to_query(&entry, &query_id) {
                            entries.push(entry);
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {}
                    // Bus closed: no more entries can arrive; the fin
                    // future still resolves (or errors) on its own.
                    Err(RecvError::Closed) => break (&mut fin_fut).await?,
                },
            }
        };
        // Entries broadcast before the fin resolved can still be
        // queued when the select races in the fin's favor — drain
        // what is already buffered.
        collect_buffered_entries(&mut events, &query_id, &mut entries);
        let fin = parse_inbox_fin(&fin_iq).unwrap_or_default();
        Ok(InboxPage { entries, fin })
    }

    async fn mark_inbox_read(&self, partner: &str, thread: Option<&str>) -> ClientResult<()> {
        let binding = self.snapshot().binding.ok_or(ClientError::Disconnected)?;
        let account = binding.jid.to_bare().to_string();
        let iq = build_waddle_inbox_mark_read_iq(
            &account,
            &WaddleInboxMarkRead {
                partner: partner.to_string(),
                thread: thread.map(str::to_string),
            },
        );
        self.send_iq(iq).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::{Arc, RwLock};

    use jid::FullJid;
    use minidom::Element;
    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::command::XmppCommand;
    use crate::inbox::{InboxFin, InboxFinCounts};
    use crate::state::{SessionBinding, SessionSnapshot};

    fn entry(
        source: InboxStreamEntrySource,
        query_id: Option<&str>,
        partner: &str,
        unread: u32,
    ) -> InboxStreamEntry {
        InboxStreamEntry {
            source,
            query_id: query_id.map(str::to_string),
            partner: partner.to_string(),
            kind: Some("direct".to_string()),
            last_stanza_id: format!("sid-{partner}"),
            last_updated: Some(1_700_000_000),
            unread,
            preview: None,
            thread_id: None,
            thread_title: None,
            reply_count: None,
            author: None,
        }
    }

    fn fin_result_element(id: &str) -> Element {
        let xml = format!(
            "<iq xmlns='jabber:client' type='result' id='{id}'>\
                <fin xmlns='urn:xmpp:inbox:1' total='2' unread='1' all-unread='5'>\
                    <set xmlns='http://jabber.org/protocol/rsm'>\
                        <first>a</first><last>b</last><count>2</count>\
                    </set>\
                </fin>\
            </iq>"
        );
        xml.parse().expect("fin fixture parses")
    }

    struct MockDriver {
        handle: ClientHandle,
        commands: mpsc::Receiver<XmppCommand>,
        events: broadcast::Sender<ClientEvent>,
    }

    fn mock_driver(snapshot: SessionSnapshot) -> MockDriver {
        let (cmd_tx, cmd_rx) = mpsc::channel::<XmppCommand>(8);
        let (evt_tx, _) = broadcast::channel::<ClientEvent>(256);
        let state = Arc::new(RwLock::new(snapshot));
        let handle = ClientHandle::from_parts(cmd_tx, evt_tx.clone(), state);
        MockDriver {
            handle,
            commands: cmd_rx,
            events: evt_tx,
        }
    }

    fn bound_snapshot() -> SessionSnapshot {
        let mut snapshot = SessionSnapshot::new();
        snapshot.binding = Some(SessionBinding {
            jid: FullJid::from_str("alice@waddle.test/phone").expect("valid jid"),
            stream_id: None,
            resumable: false,
        });
        snapshot
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_inbox_folds_correlated_entries_and_fin() {
        let MockDriver {
            handle,
            mut commands,
            events,
        } = mock_driver(SessionSnapshot::new());

        // Mock driver: broadcast entries (matching, foreign-query,
        // push) then resolve the fin — exactly the ordering the real
        // driver task produces.
        tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            assert_eq!(stanza.name(), "iq");
            assert_eq!(stanza.attr("type"), Some("get"));
            let inbox = stanza
                .get_child("inbox", "urn:xmpp:inbox:1")
                .expect("inbox payload");
            assert_eq!(inbox.attr("unread-only"), Some("true"));
            assert_eq!(inbox.attr("messages"), Some("false"));
            let id = stanza.attr("id").expect("iq id").to_string();

            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::QueryResponse,
                Some(&id),
                "alice@example.com",
                3,
            )));
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::QueryResponse,
                Some("some-other-query"),
                "eve@example.com",
                9,
            )));
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::Push,
                None,
                "push@example.com",
                1,
            )));
            let _ = responder.send(Ok(fin_result_element(&id)));
        });

        let page = handle.fetch_inbox(true, false).await.expect("inbox page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].partner, "alice@example.com");
        assert_eq!(page.entries[0].unread, 3);
        assert_eq!(
            page.fin,
            InboxFin {
                counts: InboxFinCounts {
                    total: 2,
                    unread: 1,
                    all_unread: 5,
                },
                rsm_first: Some("a".to_string()),
                rsm_last: Some("b".to_string()),
                rsm_count: Some(2),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_inbox_claims_queryid_less_entries_as_single_pending_query() {
        let MockDriver {
            handle,
            mut commands,
            events,
        } = mock_driver(SessionSnapshot::new());

        tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            let id = stanza.attr("id").expect("iq id").to_string();
            // `messages='false'` responses carry no MAM <result/>,
            // hence no queryid — the lone in-flight query claims them.
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::QueryResponse,
                None,
                "bob@example.com",
                2,
            )));
            let _ = responder.send(Ok(fin_result_element(&id)));
        });

        let page = handle.fetch_inbox(false, true).await.expect("inbox page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].partner, "bob@example.com");
        assert_eq!(page.entries[0].query_id, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_inbox_never_folds_pushes_into_the_page() {
        let MockDriver {
            handle,
            mut commands,
            events,
        } = mock_driver(SessionSnapshot::new());

        tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            let id = stanza.attr("id").expect("iq id").to_string();
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::Push,
                None,
                "room@muc.example.com",
                4,
            )));
            let _ = responder.send(Ok(fin_result_element(&id)));
        });

        let page = handle.fetch_inbox(false, false).await.expect("inbox page");
        assert!(page.entries.is_empty(), "pushes must never enter a page");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_inbox_survives_broadcast_bursts_beyond_ring_capacity() {
        let MockDriver {
            handle,
            mut commands,
            events,
        } = mock_driver(SessionSnapshot::new());

        // 300 unrelated events land between the first entry and the
        // fin — more than the 256-slot broadcast ring holds. The live
        // select-drain must keep up (the mock yields between sends,
        // like the real driver task yields between transport frames)
        // so the early entry is never overwritten.
        tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            let id = stanza.attr("id").expect("iq id").to_string();
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::QueryResponse,
                Some(&id),
                "early@example.com",
                1,
            )));
            tokio::task::yield_now().await;
            for _ in 0..300 {
                let filler = Element::builder("presence", "jabber:client").build();
                let _ = events.send(ClientEvent::UnhandledStanza(filler));
                tokio::task::yield_now().await;
            }
            let _ = events.send(ClientEvent::InboxStreamEntry(entry(
                InboxStreamEntrySource::QueryResponse,
                Some(&id),
                "late@example.com",
                2,
            )));
            let _ = responder.send(Ok(fin_result_element(&id)));
        });

        let page = handle.fetch_inbox(false, false).await.expect("inbox page");
        let partners: Vec<&str> = page
            .entries
            .iter()
            .map(|entry| entry.partner.as_str())
            .collect();
        assert_eq!(partners, vec!["early@example.com", "late@example.com"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_inbox_propagates_stanza_errors() {
        let MockDriver {
            handle,
            mut commands,
            events: _events,
        } = mock_driver(SessionSnapshot::new());

        tokio::spawn(async move {
            let Some(XmppCommand::SendIq {
                stanza: _,
                responder,
            }) = commands.recv().await
            else {
                panic!("expected SendIq command");
            };
            let _ = responder.send(Err(ClientError::Disconnected));
        });

        let err = handle
            .fetch_inbox(false, false)
            .await
            .expect_err("error must surface");
        assert!(matches!(err, ClientError::Disconnected));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mark_inbox_read_sends_waddle_mark_read_iq_to_own_bare_jid() {
        let MockDriver {
            handle,
            mut commands,
            events: _events,
        } = mock_driver(bound_snapshot());

        let driver = tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            assert_eq!(stanza.attr("type"), Some("set"));
            assert_eq!(stanza.attr("to"), Some("alice@waddle.test"));
            let mark_read = stanza
                .get_child("mark-read", "urn:waddle:inbox:0")
                .expect("mark-read payload");
            assert_eq!(mark_read.attr("partner"), Some("bob@waddle.test"));
            assert_eq!(mark_read.attr("thread"), Some("thread-9"));
            let id = stanza.attr("id").expect("iq id");
            let reply = Element::builder("iq", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                .build();
            let _ = responder.send(Ok(reply));
        });

        handle
            .mark_inbox_read("bob@waddle.test", Some("thread-9"))
            .await
            .expect("mark-read resolves");
        driver.await.expect("mock driver assertions pass");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mark_inbox_read_omits_thread_attr_when_absent() {
        let MockDriver {
            handle,
            mut commands,
            events: _events,
        } = mock_driver(bound_snapshot());

        let driver = tokio::spawn(async move {
            let Some(XmppCommand::SendIq { stanza, responder }) = commands.recv().await else {
                panic!("expected SendIq command");
            };
            let mark_read = stanza
                .get_child("mark-read", "urn:waddle:inbox:0")
                .expect("mark-read payload");
            assert_eq!(mark_read.attr("thread"), None);
            let id = stanza.attr("id").expect("iq id");
            let reply = Element::builder("iq", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
                .build();
            let _ = responder.send(Ok(reply));
        });

        handle
            .mark_inbox_read("bob@waddle.test", None)
            .await
            .expect("mark-read resolves");
        driver.await.expect("mock driver assertions pass");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mark_inbox_read_requires_a_bound_session() {
        let MockDriver { handle, .. } = mock_driver(SessionSnapshot::new());

        let err = handle
            .mark_inbox_read("bob@waddle.test", None)
            .await
            .expect_err("unbound session must fail");
        assert!(matches!(err, ClientError::Disconnected));
    }
}
