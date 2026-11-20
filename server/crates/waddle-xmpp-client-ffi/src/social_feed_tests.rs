//! XEP-0472 FFI test suite: feed verb wire shapes as produced by the
//! exact FFI construction path, reply → typed-result mapping
//! (including the bridged-entry source kind), post validation, and the
//! exported methods' typed failure behavior without a live session.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};
use waddle_xmpp_client::ClientError;

use crate::social_feed::{
    feed_entry_from_post, feed_fetch_stanza, feed_publish_stanza, map_feed_reply,
};
use crate::test_support::offline_client;
use crate::{WaddleError, WaddleFeedEntry, WaddleFeedPost, WaddleFeedSource};

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
const FEED_NODE: &str = "urn:xmpp:pubsub-social-feed:1";

fn community() -> BareJid {
    "community.waddle.test".parse().expect("valid service jid")
}

fn alice() -> BareJid {
    "alice@waddle.test".parse().expect("valid bare jid")
}

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid timestamp")
}

fn post(body: &str) -> WaddleFeedPost {
    WaddleFeedPost {
        title: None,
        body: body.to_string(),
        link: None,
    }
}

fn forbidden() -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Auth,
        condition: "forbidden".to_string(),
        text: Some("nope".to_string()),
        application_condition: None,
    })
}

fn atom_text(entry: &Element, name: &str) -> Option<String> {
    entry.get_child(name, NS_ATOM).map(Element::text)
}

// ── Wire shapes ──────────────────────────────────────────────────────

#[test]
fn fetch_stanza_asks_the_community_feed_node() {
    let iq = feed_fetch_stanza(&community(), Some(50));
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("node"), Some(FEED_NODE));
    assert_eq!(items.attr("max_items"), Some("50"));
}

#[test]
fn fetch_stanzas_never_reuse_an_iq_id() {
    let first = feed_fetch_stanza(&community(), None);
    let second = feed_fetch_stanza(&community(), None);
    assert_ne!(first.attr("id"), second.attr("id"));
}

#[test]
fn publish_stanza_carries_the_post_as_an_atom_entry() {
    let entry = feed_entry_from_post(
        WaddleFeedPost {
            title: Some("Hello".to_string()),
            body: "hello waddlers".to_string(),
            link: Some("https://waddle.test/post".to_string()),
        },
        &alice(),
        at("2026-11-20T10:00:00Z"),
    )
    .expect("valid post");
    let iq = feed_publish_stanza(&community(), &entry);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish element");
    assert_eq!(publish.attr("node"), Some(FEED_NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some(entry.id.as_str()));
    let atom = item.get_child("entry", NS_ATOM).expect("atom entry");
    assert_eq!(atom_text(atom, "title").as_deref(), Some("Hello"));
    assert_eq!(
        atom_text(atom, "content").as_deref(),
        Some("hello waddlers")
    );
    assert_eq!(
        atom_text(atom, "published").as_deref(),
        Some("2026-11-20T10:00:00+00:00")
    );
    assert_eq!(
        atom.get_child("link", NS_ATOM)
            .and_then(|link| link.attr("href")),
        Some("https://waddle.test/post")
    );
}

// ── Post validation ──────────────────────────────────────────────────

#[test]
fn a_post_gets_a_fresh_id_and_the_account_as_author() {
    let now = at("2026-11-20T10:00:00Z");
    let first = feed_entry_from_post(post("one"), &alice(), now).expect("valid post");
    let second = feed_entry_from_post(post("two"), &alice(), now).expect("valid post");
    assert!(first.id.starts_with("post-"));
    assert_ne!(first.id, second.id);
    assert_eq!(first.author.as_deref(), Some("alice@waddle.test"));
    assert_eq!(first.published, Some(now));
}

#[test]
fn a_post_without_text_is_refused() {
    let result = feed_entry_from_post(post("  \n"), &alice(), at("2026-11-20T10:00:00Z"));
    assert_eq!(result, Err(WaddleError::InvalidArgument));
}

#[test]
fn blank_title_and_link_are_dropped() {
    let entry = feed_entry_from_post(
        WaddleFeedPost {
            title: Some("  ".to_string()),
            body: "hi".to_string(),
            link: Some(String::new()),
        },
        &alice(),
        at("2026-11-20T10:00:00Z"),
    )
    .expect("valid post");
    assert_eq!(entry.title, None);
    assert_eq!(entry.link, None);
}

// ── Reply mapping ────────────────────────────────────────────────────

fn feed_result() -> Element {
    "<iq xmlns='jabber:client' type='result' id='x'>\
       <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
         <items node='urn:xmpp:pubsub-social-feed:1'>\
           <item id='post-1'>\
             <entry xmlns='http://www.w3.org/2005/Atom'>\
               <title type='text'>Hello</title>\
               <id>post-1</id>\
               <published>2026-11-20T10:00:00Z</published>\
               <updated>2026-11-20T10:00:00Z</updated>\
               <content type='text'>hello waddlers</content>\
               <author><uri>xmpp:alice@waddle.test</uri></author>\
               <link rel='alternate' href='https://waddle.test/post'/>\
             </entry>\
           </item>\
           <item id='pep-mood-1'>\
             <entry xmlns='http://www.w3.org/2005/Atom'>\
               <title type='text'>bob is happy</title>\
               <id>pep-mood-1</id>\
               <updated>2026-11-20T09:00:00Z</updated>\
               <source xmlns='urn:waddle:feed-source:0' kind='mood'/>\
             </entry>\
           </item>\
           <item id='pep-future-1'>\
             <entry xmlns='http://www.w3.org/2005/Atom'>\
               <title type='text'>something new</title>\
               <id>pep-future-1</id>\
               <updated>2026-11-20T08:00:00Z</updated>\
               <source xmlns='urn:waddle:feed-source:0' kind='hologram'/>\
             </entry>\
           </item>\
           <item id='broken'><entry xmlns='http://www.w3.org/2005/Atom'/></item>\
         </items>\
       </pubsub>\
     </iq>"
        .parse()
        .expect("fixture parses")
}

#[test]
fn a_feed_reply_maps_to_typed_entries_in_server_order() {
    let entries = map_feed_reply(Ok(feed_result())).expect("entries");
    assert_eq!(
        entries[0],
        WaddleFeedEntry {
            id: "post-1".to_string(),
            title: Some("Hello".to_string()),
            body: "hello waddlers".to_string(),
            author: Some("alice@waddle.test".to_string()),
            published_ms: Some(1_795_168_800_000),
            link: Some("https://waddle.test/post".to_string()),
            source: None,
        }
    );
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, ["post-1", "pep-mood-1", "pep-future-1"]);
}

#[test]
fn bridged_entries_carry_their_source_kind() {
    let entries = map_feed_reply(Ok(feed_result())).expect("entries");
    assert_eq!(entries[1].source, Some(WaddleFeedSource::Mood));
    assert_eq!(entries[2].source, Some(WaddleFeedSource::Other));
}

#[test]
fn a_stanza_error_keeps_its_condition() {
    assert_eq!(
        map_feed_reply(Err(forbidden())),
        Err(WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: Some("nope".to_string()),
        })
    );
}

// ── Exported verbs without a session ─────────────────────────────────

#[tokio::test]
async fn fetch_feed_reports_not_connected_and_invalid_jid() {
    let client = offline_client();
    let result = client
        .fetch_feed("community.waddle.test".to_string(), None)
        .await;
    assert_eq!(result, Err(WaddleError::NotConnected));
    let invalid = client.fetch_feed("not a jid".to_string(), None).await;
    assert_eq!(invalid, Err(WaddleError::InvalidJid));
}

#[tokio::test]
async fn publish_feed_post_validates_before_it_needs_a_session() {
    let client = offline_client();
    let blank = client
        .publish_feed_post("community.waddle.test".to_string(), post(" "))
        .await;
    assert_eq!(blank, Err(WaddleError::InvalidArgument));
    let offline = client
        .publish_feed_post("community.waddle.test".to_string(), post("hi"))
        .await;
    assert_eq!(offline, Err(WaddleError::NotConnected));
}
