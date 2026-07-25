//! XEP-0472 FFI test suite: feed verb wire shapes as produced by the
//! exact FFI construction path, reply → typed-record mapping
//! (timestamps as unix millis, the six bridge-source kinds), publish
//! item-id resolution (server echo vs. minted fallback), and the
//! exported methods' typed failure behavior (not-connected, empty
//! body) through an offline client.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};
use waddle_xmpp_client::social_feed::{FeedItem, FeedSourceKind};
use waddle_xmpp_client::ClientError;
use waddle_xmpp_core::xep0472::{build_feed_entry_element, FeedEntry, NS_ATOM, PUBSUB_NODE_FEED};

use crate::social_feed::{
    feed_item_to_ffi, feed_items_stanza, feed_publish_stanza, map_fetch_feed_reply,
    map_publish_feed_reply,
};
use crate::{
    WaddleClient, WaddleClientEvent, WaddleConfig, WaddleError, WaddleEventListener,
    WaddleFeedSourceKind,
};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

struct NullListener;

impl WaddleEventListener for NullListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(NullListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

fn service() -> BareJid {
    "community.waddle.test".parse().expect("test service JID")
}

fn forbidden() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Auth,
        condition: "forbidden".to_string(),
        text: None,
        application_condition: None,
    })
}

fn items_result(items_inner: &str) -> Element {
    format!(
        "<iq xmlns='jabber:client' type='result' id='feed-items-1'>\
         <pubsub xmlns='{NS_PUBSUB}'>\
         <items node='{PUBSUB_NODE_FEED}'>{items_inner}</items>\
         </pubsub></iq>"
    )
    .parse()
    .expect("test IQ parses")
}

// ── Wire shapes ──────────────────────────────────────────────────────

#[test]
fn feed_items_stanza_targets_the_community_feed_node() {
    let iq = feed_items_stanza(&service(), Some(25));
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    assert!(iq.attr("id").expect("iq id").starts_with("feed-items-"));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("node"), Some(PUBSUB_NODE_FEED));
    assert_eq!(items.attr("max_items"), Some("25"));
}

#[test]
fn feed_publish_stanza_carries_the_entry_under_the_minted_item_id() {
    let entry = FeedEntry::new("post-abc", "hello from android")
        .with_title("Hello")
        .with_author("alice@waddle.test")
        .with_published_now();
    let iq = feed_publish_stanza(&service(), "post-abc", &entry);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    assert!(iq.attr("id").expect("iq id").starts_with("feed-publish-"));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish element");
    assert_eq!(publish.attr("node"), Some(PUBSUB_NODE_FEED));
    let item = publish.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some("post-abc"));
    let entry_el = item.get_child("entry", NS_ATOM).expect("entry element");
    let author = entry_el
        .get_child("author", NS_ATOM)
        .and_then(|author| author.get_child("uri", NS_ATOM))
        .expect("author uri");
    assert_eq!(author.text(), "xmpp:alice@waddle.test");
}

// ── Reply mapping ────────────────────────────────────────────────────

#[test]
fn fetch_reply_maps_entries_to_typed_records_with_unix_millis() {
    let entry = FeedEntry::new("post-1", "Feed body")
        .with_title("Feed title")
        .with_author("bob@waddle.test")
        .with_published(
            "2026-07-01T12:00:00Z"
                .parse()
                .expect("test timestamp parses"),
        )
        .with_link("https://waddle.test/p/1");
    let entry_xml = String::from(&build_feed_entry_element(&entry));
    let reply = items_result(&format!("<item id='post-1'>{entry_xml}</item>"));

    let records = map_fetch_feed_reply(Ok(reply)).expect("maps");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.id, "post-1");
    assert_eq!(record.title.as_deref(), Some("Feed title"));
    assert_eq!(record.body, "Feed body");
    assert_eq!(record.author.as_deref(), Some("bob@waddle.test"));
    assert_eq!(record.published_epoch_ms, Some(1_782_907_200_000));
    assert_eq!(record.link.as_deref(), Some("https://waddle.test/p/1"));
    assert_eq!(record.source, None);
}

#[test]
fn fetch_reply_maps_the_bridge_source_kind() {
    let entry_xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                     <title type='text'>Mood update</title>\
                     <id>post-mood</id>\
                     <updated>2026-07-01T12:00:00Z</updated>\
                     <source xmlns='urn:waddle:feed-source:0' kind='mood'/>\
                     </entry>";
    let reply = items_result(&format!("<item id='post-mood'>{entry_xml}</item>"));
    let records = map_fetch_feed_reply(Ok(reply)).expect("maps");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source, Some(WaddleFeedSourceKind::Mood));
}

#[test]
fn fetch_reply_surfaces_stanza_errors_typed() {
    let err = map_fetch_feed_reply(Err(forbidden())).expect_err("stanza error surfaces");
    assert_eq!(
        err,
        WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: None,
        }
    );
}

#[test]
fn publish_reply_prefers_the_server_item_id_echo() {
    let reply: Element = format!(
        "<iq xmlns='jabber:client' type='result' id='feed-publish-1'>\
         <pubsub xmlns='{NS_PUBSUB}'>\
         <publish node='{PUBSUB_NODE_FEED}'><item id='post-server'/></publish>\
         </pubsub></iq>"
    )
    .parse()
    .expect("test IQ parses");
    let id = map_publish_feed_reply(Ok(reply), "post-minted".to_string()).expect("maps");
    assert_eq!(id, "post-server");
}

#[test]
fn publish_reply_falls_back_to_the_minted_item_id() {
    let reply: Element = "<iq xmlns='jabber:client' type='result' id='feed-publish-1'/>"
        .parse()
        .expect("test IQ parses");
    let id = map_publish_feed_reply(Ok(reply), "post-minted".to_string()).expect("maps");
    assert_eq!(id, "post-minted");

    let err = map_publish_feed_reply(Err(forbidden()), "post-minted".to_string())
        .expect_err("stanza error surfaces");
    assert!(matches!(err, WaddleError::Stanza { .. }));
}

// ── Record conversion ────────────────────────────────────────────────

#[test]
fn feed_item_conversion_covers_every_source_kind() {
    for (kind, expected) in [
        (FeedSourceKind::Mood, WaddleFeedSourceKind::Mood),
        (FeedSourceKind::Activity, WaddleFeedSourceKind::Activity),
        (FeedSourceKind::Tune, WaddleFeedSourceKind::Tune),
        (FeedSourceKind::Avatar, WaddleFeedSourceKind::Avatar),
        (FeedSourceKind::Vcard, WaddleFeedSourceKind::Vcard),
        (FeedSourceKind::Rsvp, WaddleFeedSourceKind::Rsvp),
    ] {
        let record = feed_item_to_ffi(FeedItem {
            entry: FeedEntry::new("post-k", "body"),
            source: Some(kind),
        });
        assert_eq!(record.source, Some(expected));
        assert_eq!(record.published_epoch_ms, None);
    }
}

// ── Exported methods (offline client) ────────────────────────────────

#[tokio::test]
async fn fetch_feed_requires_a_live_session() {
    let client = offline_client();
    assert_eq!(
        client.fetch_feed(Some(10)).await.expect_err("offline"),
        WaddleError::NotConnected
    );
}

#[tokio::test]
async fn publish_feed_post_rejects_an_empty_body_before_connecting() {
    let client = offline_client();
    assert_eq!(
        client
            .publish_feed_post("   ".to_string(), None)
            .await
            .expect_err("empty body"),
        WaddleError::InvalidArgument
    );
    assert_eq!(
        client
            .publish_feed_post("hello".to_string(), Some("title".to_string()))
            .await
            .expect_err("offline"),
        WaddleError::NotConnected
    );
}
