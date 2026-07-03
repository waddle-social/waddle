//! XEP-0402: PEP Native Bookmarks dedicated suite.
//!
//! Beyond the extensions-container coverage, this suite pins the PEP
//! integration surface:
//! - the `urn:xmpp:bookmarks:1` namespace doubling as the PEP node
//!   name (§4.1), mirrored by the `waddle-xmpp-core` constant the
//!   auto-create config table keys on,
//! - the §4.2 node-configuration invariants applied by
//!   `NodeConfig::pep_for_node` (whitelist access, persistent items,
//!   bounded max_items, never send-last),
//! - the item-id-is-room-JID contract of `build_bookmark_item` (§3),
//! - full-attribute parse/build round-trips and the typed
//!   `BookmarkError` surface for malformed payloads.

use minidom::Element;
use waddle_xmpp::xep::xep0402::{
    build_bookmark_element, build_bookmark_item, is_bookmarks_node, parse_bookmark, Bookmark,
    BookmarkError, NS_BOOKMARKS2, PEP_NODE,
};
use waddle_xmpp_core::pubsub::{
    AccessModel, NodeConfig, PublishModel, SendLastPublishedItem, PEP_BOOKMARK_MAX_ITEMS,
    PEP_NODE_BOOKMARKS,
};

#[test]
fn xep0402_extensions_container_is_parsed_as_extension_payloads() {
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'><on-mention /></notify>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid XEP-0402 bookmark");

    let bookmark = parse_bookmark("room@muc.example.com", &payload).expect("bookmark parses");

    assert_eq!(bookmark.extensions.len(), 1);
    assert!(bookmark.extensions[0].is("notify", "urn:xmpp:notification-settings:1"));
}

#[test]
fn xep0402_builder_wraps_extensions_in_bookmark_extensions_element() {
    let mut bookmark = Bookmark::new("room@muc.example.com".parse().expect("room JID"));
    bookmark.extensions.push(
        "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>"
            .parse()
            .expect("notify payload"),
    );

    let payload = build_bookmark_element(&bookmark);
    let extensions = payload
        .get_child("extensions", NS_BOOKMARKS2)
        .expect("extensions container");

    assert!(extensions
        .get_child("notify", "urn:xmpp:notification-settings:1")
        .is_some());
    assert!(payload
        .get_child("notify", "urn:xmpp:notification-settings:1")
        .is_none());
}

// ── §4.1 PEP node identity ───────────────────────────────────────────

#[test]
fn xep0402_pep_node_is_the_bookmarks_namespace() {
    // XEP-0402 §4.1: bookmarks live on the PEP node named after the
    // namespace itself.
    assert_eq!(NS_BOOKMARKS2, "urn:xmpp:bookmarks:1");
    assert_eq!(PEP_NODE, "urn:xmpp:bookmarks:1");
    assert!(is_bookmarks_node("urn:xmpp:bookmarks:1"));
    assert!(!is_bookmarks_node("urn:xmpp:bookmarks:0"));
    assert!(!is_bookmarks_node("storage:bookmarks"));
}

#[test]
fn xep0402_pep_node_constant_mirrors_waddle_xmpp_core() {
    // `waddle-xmpp-core` keeps an independent constant so the
    // `NodeConfig::pep_for_node` auto-create table can reference it
    // without depending on `waddle-xmpp`. The two MUST stay equal or
    // publishes would route through one identifier while the config
    // table keys off the other.
    assert_eq!(PEP_NODE, PEP_NODE_BOOKMARKS);
}

// ── §4.2 node-configuration invariants ───────────────────────────────

#[test]
fn xep0402_auto_created_node_config_enforces_privacy_and_durability() {
    // XEP-0402 §4.2: the bookmarks node MUST be private
    // (access_model=whitelist), persistent, and MUST NOT push the
    // last published item on presence (bookmarks are state, not
    // notifications).
    let config = NodeConfig::pep_for_node(PEP_NODE_BOOKMARKS);

    assert_eq!(config.access_model, AccessModel::Whitelist);
    assert_eq!(config.publish_model, PublishModel::Publishers);
    assert!(config.persist_items);
    assert_eq!(
        config.send_last_published_item,
        SendLastPublishedItem::Never
    );
    assert_eq!(
        config.max_items, PEP_BOOKMARK_MAX_ITEMS,
        "bookmarks node must hold many items (one per room), bounded for anti-DoS"
    );
    assert!(
        config.max_items > 1,
        "PEP default max_items=1 would evict every prior bookmark"
    );
}

#[test]
fn xep0402_normalize_reasserts_invariants_on_owner_reconfigure() {
    // An owner reconfigure trying to flip the node public (or unbound)
    // must be normalised back to the §4.2 invariants.
    let mut config = NodeConfig::pep_for_node(PEP_NODE_BOOKMARKS);
    config.access_model = AccessModel::Open;
    config.persist_items = false;
    config.max_items = 0;
    config.send_last_published_item = SendLastPublishedItem::OnSubAndPresence;

    let normalized = config.normalize_xep0402_bookmarks();
    assert_eq!(normalized.access_model, AccessModel::Whitelist);
    assert!(normalized.persist_items);
    assert_eq!(normalized.max_items, PEP_BOOKMARK_MAX_ITEMS);
    assert_eq!(
        normalized.send_last_published_item,
        SendLastPublishedItem::Never
    );
}

// ── §3 item shape ────────────────────────────────────────────────────

#[test]
fn xep0402_bookmark_item_id_is_the_room_bare_jid() {
    // XEP-0402 §3: "The id of the item is the JID of its conference."
    let bookmark = Bookmark::new("orchard@conference.shakespeare.lit".parse().expect("jid"))
        .with_name("The Orchard")
        .with_autojoin(true);
    let item = build_bookmark_item(&bookmark);

    assert_eq!(
        item.id.as_deref(),
        Some("orchard@conference.shakespeare.lit")
    );
    let payload = item
        .payload
        .expect("item carries the <conference/> payload");
    assert_eq!(payload.name(), "conference");
    assert_eq!(payload.ns(), NS_BOOKMARKS2);
}

// ── Round-trips ──────────────────────────────────────────────────────

#[test]
fn xep0402_full_bookmark_survives_serialize_reparse_round_trip() {
    let original = Bookmark::new("theatre@conference.shakespeare.lit".parse().expect("jid"))
        .with_name("The Play's the Thing")
        .with_autojoin(true)
        .with_nick("JC")
        .with_password("cauldronburn");

    let elem = build_bookmark_element(&original);
    let xml = String::from(&elem);
    let reparsed: Element = xml.parse().expect("built element must reparse");

    let parsed =
        parse_bookmark("theatre@conference.shakespeare.lit", &reparsed).expect("bookmark parses");
    assert_eq!(parsed, original);
}

#[test]
fn xep0402_builder_encodes_fields_in_spec_shape() {
    // §3 example: name/autojoin are attributes; nick/password are
    // namespaced children.
    let bookmark = Bookmark::new("room@muc.example.com".parse().expect("jid"))
        .with_name("General")
        .with_autojoin(true)
        .with_nick("penguin");
    let elem = build_bookmark_element(&bookmark);

    assert_eq!(elem.attr("name"), Some("General"));
    assert_eq!(elem.attr("autojoin"), Some("true"));
    assert_eq!(
        elem.get_child("nick", NS_BOOKMARKS2).map(|n| n.text()),
        Some("penguin".to_owned())
    );
    assert!(elem.get_child("password", NS_BOOKMARKS2).is_none());
}

#[test]
fn xep0402_autojoin_false_is_omitted_and_defaults_false() {
    // The builder omits `autojoin` when false; the parser defaults an
    // absent attribute to false — both directions of the same rule.
    let bookmark = Bookmark::new("room@muc.example.com".parse().expect("jid"));
    let elem = build_bookmark_element(&bookmark);
    assert_eq!(elem.attr("autojoin"), None);

    let parsed = parse_bookmark("room@muc.example.com", &elem).expect("parses");
    assert!(!parsed.autojoin);
}

#[test]
fn xep0402_autojoin_accepts_true_and_numeric_one() {
    // xs:boolean lexical space: both "true" and "1" are true.
    for (raw, expected) in [("true", true), ("1", true), ("false", false), ("0", false)] {
        let payload: Element =
            format!("<conference xmlns='urn:xmpp:bookmarks:1' autojoin='{raw}'/>")
                .parse()
                .expect("valid xml");
        let parsed = parse_bookmark("room@muc.example.com", &payload).expect("parses");
        assert_eq!(parsed.autojoin, expected, "autojoin='{raw}'");
    }
}

// ── Typed error surface ──────────────────────────────────────────────

#[test]
fn xep0402_parse_rejects_wrong_element_name() {
    let payload: Element = "<storage xmlns='urn:xmpp:bookmarks:1'/>"
        .parse()
        .expect("valid xml");
    assert!(matches!(
        parse_bookmark("room@muc.example.com", &payload),
        Err(BookmarkError::WrongElement(_))
    ));
}

#[test]
fn xep0402_parse_rejects_legacy_bookmarks_namespace() {
    // A XEP-0048 `storage:bookmarks`-era `<conference/>` must not be
    // silently accepted as a native bookmark.
    let payload: Element = "<conference xmlns='storage:bookmarks' name='Legacy'/>"
        .parse()
        .expect("valid xml");
    assert!(matches!(
        parse_bookmark("room@muc.example.com", &payload),
        Err(BookmarkError::WrongNamespace(_))
    ));
}

#[test]
fn xep0402_parse_rejects_invalid_item_id_jid() {
    // §3 makes the item id the room JID; a non-JID id cannot key a
    // bookmark.
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'/>"
        .parse()
        .expect("valid xml");
    assert!(matches!(
        parse_bookmark("", &payload),
        Err(BookmarkError::InvalidJid(_))
    ));
    assert!(matches!(
        parse_bookmark("not a jid at all", &payload),
        Err(BookmarkError::InvalidJid(_))
    ));
}
