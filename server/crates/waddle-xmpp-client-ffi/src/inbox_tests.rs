//! XEP-0430 inbox FFI test suite.
//!
//! Pins the boundary contracts the Kotlin/Swift apps rely on:
//! wire-parsed push entries broadcast as `InboxPush` while
//! query-response entries never broadcast (wasm parity), the folded
//! page maps the `<fin/>` counts onto `WaddleInboxResult`, and the
//! exported verbs fail typed (`NotConnected`, `InvalidJid`) before
//! any stanza is built.

use std::sync::{Arc, Mutex as StdMutex};

use minidom::Element;

use waddle_xmpp_client::inbox::{
    parse_inbox_fin, parse_inbox_stream_message, InboxPage, InboxStreamEntrySource,
};
use waddle_xmpp_client::ClientEvent;

use crate::convert::{dispatch_event, inbox_entry_to_ffi};
use crate::inbox_verbs::inbox_page_to_ffi;
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener,
    WaddleInboxEntry,
};

const ACCOUNT: &str = "alice@waddle.test";

#[derive(Clone, Default)]
struct RecordingListener {
    pushes: Arc<StdMutex<Vec<WaddleInboxEntry>>>,
    other_events: Arc<StdMutex<u32>>,
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        match event {
            WaddleClientEvent::InboxPush { entry } => {
                self.pushes.lock().expect("test mutex poisoned").push(entry);
            }
            _ => {
                *self.other_events.lock().expect("test mutex poisoned") += 1;
            }
        }
    }
}

fn test_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: ACCOUNT.to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(RecordingListener::default()) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

fn parse_entry(xml: &str) -> waddle_xmpp_client::inbox::InboxStreamEntry {
    let element: Element = xml.parse().expect("fixture parses");
    parse_inbox_stream_message(&element).expect("inbox entry parses")
}

fn push_entry_fixture() -> waddle_xmpp_client::inbox::InboxStreamEntry {
    parse_entry(
        "<message xmlns='jabber:client' type='headline'>\
            <push xmlns='urn:waddle:inbox:0'>\
                <entry xmlns='urn:xmpp:inbox:1' jid='room@muc.waddle.test' id='sid-9' unread='2'/>\
                <metadata xmlns='urn:waddle:inbox:0' kind='muc' last-updated='1700000001' \
                          preview='new message' thread='t-1' thread-title='Planning' \
                          reply-count='4' author='bob@waddle.test'/>\
            </push>\
        </message>",
    )
}

fn query_entry_fixture() -> waddle_xmpp_client::inbox::InboxStreamEntry {
    parse_entry(
        "<message xmlns='jabber:client'>\
            <entry xmlns='urn:xmpp:inbox:1' jid='bob@waddle.test' id='mam-1' unread='3'/>\
            <result xmlns='urn:xmpp:mam:2' queryid='q1' id='mam-1'/>\
            <metadata xmlns='urn:waddle:inbox:0' kind='direct' last-updated='1700000000' \
                      preview='hi'/>\
        </message>",
    )
}

// ── Event dispatch: push-only broadcast (wasm parity) ────────────────────────

#[test]
fn inbox_push_broadcasts_as_inbox_push_event() {
    let listener = RecordingListener::default();
    let entry = push_entry_fixture();
    assert_eq!(entry.source, InboxStreamEntrySource::Push);

    dispatch_event(ClientEvent::InboxStreamEntry(entry), ACCOUNT, &listener);

    let pushes = listener.pushes.lock().expect("test mutex poisoned");
    assert_eq!(pushes.len(), 1);
    assert_eq!(
        pushes[0],
        WaddleInboxEntry {
            partner: "room@muc.waddle.test".to_string(),
            kind: "muc".to_string(),
            last_stanza_id: Some("sid-9".to_string()),
            last_updated: Some(1_700_000_001),
            unread: 2,
            preview: Some("new message".to_string()),
            thread_id: Some("t-1".to_string()),
            thread_title: Some("Planning".to_string()),
            reply_count: Some(4),
            author: Some("bob@waddle.test".to_string()),
        }
    );
    assert_eq!(
        *listener.other_events.lock().expect("test mutex poisoned"),
        0
    );
}

#[test]
fn query_response_entries_never_broadcast() {
    let listener = RecordingListener::default();
    let entry = query_entry_fixture();
    assert_eq!(entry.source, InboxStreamEntrySource::QueryResponse);

    dispatch_event(ClientEvent::InboxStreamEntry(entry), ACCOUNT, &listener);

    assert!(
        listener
            .pushes
            .lock()
            .expect("test mutex poisoned")
            .is_empty(),
        "query-response entries resolve fetch_inbox, never the event bus"
    );
    assert_eq!(
        *listener.other_events.lock().expect("test mutex poisoned"),
        0,
        "query-response entries must not surface as any other event"
    );
}

// ── Entry / page conversion shapes ───────────────────────────────────────────

#[test]
fn inbox_entry_without_metadata_defaults_kind_to_direct() {
    let entry = parse_entry(
        "<message xmlns='jabber:client'>\
            <entry xmlns='urn:xmpp:inbox:1' jid='bob@waddle.test' id='mam-1' unread='1'/>\
        </message>",
    );
    let ffi = inbox_entry_to_ffi(entry);
    assert_eq!(ffi.kind, "direct");
    assert_eq!(ffi.last_stanza_id.as_deref(), Some("mam-1"));
    assert_eq!(ffi.last_updated, None);
    assert_eq!(ffi.unread, 1);
}

#[test]
fn inbox_page_maps_fin_counts_and_entries() {
    let fin_iq: Element = "<iq xmlns='jabber:client' type='result' id='q1'>\
            <fin xmlns='urn:xmpp:inbox:1' total='7' unread='3' all-unread='11'/>\
        </iq>"
        .parse()
        .expect("fin fixture parses");
    let page = InboxPage {
        entries: vec![query_entry_fixture()],
        fin: parse_inbox_fin(&fin_iq).expect("fin parses"),
    };

    let result = inbox_page_to_ffi(page);
    assert_eq!(result.total, 7);
    assert_eq!(result.unread, 3);
    assert_eq!(result.all_unread, 11);
    assert_eq!(result.conversations.len(), 1);
    assert_eq!(result.conversations[0].partner, "bob@waddle.test");
    assert_eq!(result.conversations[0].kind, "direct");
    assert_eq!(result.conversations[0].unread, 3);
}

// ── Verb failure shapes ──────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_inbox_without_session_fails_not_connected() {
    let client = test_client();
    let err = client
        .fetch_inbox(false, true)
        .await
        .expect_err("no session must fail");
    assert_eq!(err, WaddleError::NotConnected);
}

#[tokio::test]
async fn mark_inbox_read_without_session_fails_not_connected() {
    let client = test_client();
    let err = client
        .mark_inbox_read("bob@waddle.test".to_string(), None)
        .await
        .expect_err("no session must fail");
    assert_eq!(err, WaddleError::NotConnected);
}

#[tokio::test]
async fn mark_inbox_read_rejects_invalid_partner_jid() {
    let client = test_client();
    let err = client
        .mark_inbox_read("not a jid".to_string(), Some("t-1".to_string()))
        .await
        .expect_err("invalid JID must fail before any stanza is built");
    assert_eq!(err, WaddleError::InvalidJid);
}
