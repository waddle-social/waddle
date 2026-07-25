//! Dedicated XEP-0472 test suite for the shared feed verbs: items /
//! publish IQ wire shapes on the community service, Atom entry
//! roundtrip through the items-result parse, `urn:waddle:feed-source:0`
//! bridge-kind parsing (including unknown-kind tolerance), and
//! non-entry-item tolerance.

use chrono::{TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_core::xep0472::build_feed_entry_element;

use super::*;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const CLIENT_NS: &str = "jabber:client";

fn service() -> jid::BareJid {
    "community.waddle.test".parse().expect("valid service jid")
}

fn items_result(items_inner: &str) -> Element {
    format!(
        "<iq xmlns='{CLIENT_NS}' type='result' id='feed-items-1'>\
         <pubsub xmlns='{NS_PUBSUB}'>\
         <items node='{PUBSUB_NODE_FEED}'>{items_inner}</items>\
         </pubsub></iq>"
    )
    .parse()
    .expect("test IQ parses")
}

// ── IQ wire shapes ───────────────────────────────────────────────────

#[test]
fn items_iq_targets_the_feed_node_on_the_community_service() {
    let iq = build_feed_items_iq(&service(), Some(50));
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    assert!(iq.attr("id").expect("iq id").starts_with("feed-items-"));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("node"), Some(PUBSUB_NODE_FEED));
    assert_eq!(items.attr("max_items"), Some("50"));
}

#[test]
fn items_iq_omits_max_items_when_unbounded() {
    let iq = build_feed_items_iq(&service(), None);
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items element");
    assert_eq!(items.attr("max_items"), None);
}

#[test]
fn publish_iq_carries_the_atom_entry_under_the_supplied_item_id() {
    let entry = FeedEntry::new("post-1", "hello waddlers");
    let iq = build_feed_publish_iq(&service(), "post-1", &entry);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("community.waddle.test"));
    assert!(iq.attr("id").expect("iq id").starts_with("feed-publish-"));
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish element");
    assert_eq!(publish.attr("node"), Some(PUBSUB_NODE_FEED));
    let item = publish.get_child("item", NS_PUBSUB).expect("item element");
    assert_eq!(item.attr("id"), Some("post-1"));
    assert!(item.get_child("entry", NS_ATOM).is_some());
    // No publish-options precondition on feed posts.
    assert!(iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish-options", NS_PUBSUB))
        .is_none());
}

// ── Items-result parsing ─────────────────────────────────────────────

#[test]
fn parse_roundtrips_a_built_atom_entry() {
    let published = Utc
        .with_ymd_and_hms(2026, 7, 1, 12, 0, 0)
        .single()
        .expect("valid timestamp");
    let entry = FeedEntry::new("post-42", "Roundtrip body")
        .with_title("Roundtrip")
        .with_author("alice@waddle.test")
        .with_published(published)
        .with_link("https://waddle.test/post/42");
    let entry_xml = String::from(&build_feed_entry_element(&entry));
    let iq = items_result(&format!("<item id='post-42'>{entry_xml}</item>"));

    let parsed = parse_feed_items_result(&iq);
    assert_eq!(parsed.len(), 1);
    let item = &parsed[0];
    assert_eq!(item.entry.id, "post-42");
    assert_eq!(item.entry.title.as_deref(), Some("Roundtrip"));
    assert_eq!(item.entry.body, "Roundtrip body");
    assert_eq!(item.entry.author.as_deref(), Some("alice@waddle.test"));
    assert_eq!(item.entry.published, Some(published));
    assert_eq!(
        item.entry.link.as_deref(),
        Some("https://waddle.test/post/42")
    );
    assert_eq!(item.source, None);
}

#[test]
fn parse_reads_the_bridge_source_kind() {
    let entry_xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                     <title type='text'>Mood update</title>\
                     <id>post-mood</id>\
                     <updated>2026-07-01T12:00:00Z</updated>\
                     <content type='text'>feeling happy</content>\
                     <source xmlns='urn:waddle:feed-source:0' kind='mood'/>\
                     </entry>";
    let iq = items_result(&format!("<item id='post-mood'>{entry_xml}</item>"));
    let parsed = parse_feed_items_result(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].source, Some(FeedSourceKind::Mood));
}

#[test]
fn parse_tolerates_unknown_source_kinds() {
    let entry_xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                     <title type='text'>Future bridge</title>\
                     <id>post-future</id>\
                     <updated>2026-07-01T12:00:00Z</updated>\
                     <source xmlns='urn:waddle:feed-source:0' kind='geoloc'/>\
                     </entry>";
    let iq = items_result(&format!("<item id='post-future'>{entry_xml}</item>"));
    let parsed = parse_feed_items_result(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].source, None);
}

#[test]
fn parse_ignores_source_children_outside_the_waddle_namespace() {
    // Atom itself defines <source/>; only the Waddle-namespaced child
    // carries the bridge kind.
    let entry_xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                     <title type='text'>Atom source</title>\
                     <id>post-atom-source</id>\
                     <updated>2026-07-01T12:00:00Z</updated>\
                     <source kind='mood'/>\
                     </entry>";
    let iq = items_result(&format!("<item id='post-atom-source'>{entry_xml}</item>"));
    let parsed = parse_feed_items_result(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].source, None);
}

#[test]
fn parse_skips_items_without_an_atom_entry_payload() {
    let entry_xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                     <title type='text'>Kept</title>\
                     <id>post-kept</id>\
                     <updated>2026-07-01T12:00:00Z</updated>\
                     </entry>";
    let iq = items_result(&format!(
        "<item id='not-an-entry'><vcalendar xmlns='urn:ietf:params:xml:ns:xcal'/></item>\
         <item id='unparsable'><entry xmlns='http://www.w3.org/2005/Atom'/></item>\
         <item id='post-kept'>{entry_xml}</item>"
    ));
    let parsed = parse_feed_items_result(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].entry.id, "post-kept");
}

#[test]
fn parse_yields_empty_on_a_result_without_items() {
    let iq: Element = format!("<iq xmlns='{CLIENT_NS}' type='result' id='feed-items-1'/>")
        .parse()
        .expect("test IQ parses");
    assert!(parse_feed_items_result(&iq).is_empty());
}

// ── Source-kind domain ───────────────────────────────────────────────

#[test]
fn source_kind_parse_and_as_str_roundtrip() {
    for kind in [
        FeedSourceKind::Mood,
        FeedSourceKind::Activity,
        FeedSourceKind::Tune,
        FeedSourceKind::Avatar,
        FeedSourceKind::Vcard,
        FeedSourceKind::Rsvp,
    ] {
        assert_eq!(FeedSourceKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(FeedSourceKind::parse("unknown"), None);
    assert_eq!(FeedSourceKind::parse(""), None);
}
