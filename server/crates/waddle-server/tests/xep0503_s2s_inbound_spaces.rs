//! XEP-0503 version 0.3.0 (xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2) across S2S, inbound: an
//! account at another server discovers the Spaces of this deployment and
//! reads what they list (ADR-0020 D10, D4, D1 and D8).
//!
//! - A Space is reached only while its `federated` setting is on. Any
//!   other node answers every request exactly as a node that does not
//!   exist: a missing node, a Space whose setting is off, and a private
//!   Space the asker holds no honoured affiliation in.
//! - "Fetching spaces from a spaces service", "Fetching the metadata
//!   associated to a space" and "Discovering the content of a space" are
//!   answered for an open Space with what a remote asker is told: no
//!   address of an account of this deployment, only the rooms that are
//!   themselves open to other servers, and the Space's avatar and banner
//!   items ("Spaces images") where their payload is an image's metadata.
//!   Nothing those items name is fetched.
//! - "Advertising space membership": the `disco#items` of a Space's node
//!   lists the same rooms, as XEP-0060 §5.5 lists published items (the
//!   service's address and the item ID, the room's JID, as its name), paged
//!   with XEP-0059, so a client verifies that a room is part of the Space.
//! - Peer admission is decided where the request is decided, under the
//!   snapshot the replica runs.
//!
//! `example.com` and `home-b.test` are the Spaces fixture's deployments
//! (`s2s_spaces_support`): loopback TLS in one process, in-memory SQLite.
//! No PostgreSQL, SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

#[allow(dead_code, unused_imports)]
mod s2s_spaces_support;

use minidom::Element;
use s2s_spaces_support::{
    affiliate, affiliation, ask, channel, configure, descendant, disco, error_condition, error_of,
    federate, federated_space, fetched, field, form_value, hosting, hosting_with, image_host,
    image_metadata, item_ids, items, list_in, of_node, owner_subscription, publish, pubsub,
    request, retract, room, rows, space, spaces, store_item, subscription, Adjust, Federated, Pki,
    BOB, BOB_PHONE, FIELD_FEDERATED, NS_AVATAR_METADATA, NS_DISCO_INFO, NS_DISCO_ITEMS, NS_PUBSUB,
    NS_PUBSUB_OWNER, PEER, SPACES,
};
use waddle_server::channel_space_links::ChannelSpaceLink;
use waddle_server::space_identity::{space_jid_for_node, SpaceNode};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{get, is_error, is_result, result_payload, set};
use waddle_xmpp::pubsub::{Affiliation, NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0503::{ITEM_SPACE_AVATAR_METADATA, ITEM_SPACE_BANNER_METADATA};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

const DEV: &str = "dev";
const CLOSED: &str = "closed";
const MISSING: &str = "nowhere";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";

fn item_nodes(answer: &Element) -> Vec<String> {
    answer
        .children()
        .flat_map(Element::children)
        .filter(|child| child.name() == "item")
        .filter_map(|item| item.attr("node").map(ToOwned::to_owned))
        .collect()
}

/// The `(jid, name)` of each item of a `disco#items` result, and whether
/// any names a node.
fn listed_items(answer: &Element) -> (Vec<(String, String)>, bool) {
    let items: Vec<&Element> = answer
        .children()
        .filter(|child| child.is("query", NS_DISCO_ITEMS))
        .flat_map(Element::children)
        .filter(|child| child.name() == "item")
        .collect();
    let pairs = items
        .iter()
        .map(|item| {
            (
                item.attr("jid").unwrap_or_default().to_owned(),
                item.attr("name").unwrap_or_default().to_owned(),
            )
        })
        .collect();
    (pairs, items.iter().any(|item| item.attr("node").is_some()))
}

/// The item IDs, the rooms' JIDs, a node's `disco#items` result lists.
fn listed_children(answer: &Element) -> Vec<String> {
    let (pairs, named_node) = listed_items(answer);
    assert!(!named_node, "a published item names no node: {answer:?}");
    pairs
        .into_iter()
        .map(|(jid, name)| {
            assert_eq!(jid, SPACES, "{answer:?}");
            name
        })
        .collect()
}

fn features(answer: &Element) -> Vec<String> {
    answer
        .children()
        .flat_map(Element::children)
        .filter(|child| child.name() == "feature")
        .filter_map(|feature| feature.attr("var").map(ToOwned::to_owned))
        .collect()
}

/// Every request that names a node, built for `node`, with its kind.
fn requests_naming(node: &str, tag: &str) -> Vec<(&'static str, Iq)> {
    let id = |kind: &str| [tag, "-", kind].concat();
    let own = |name: &str| of_node(name, NS_PUBSUB_OWNER, node);
    vec![
        (
            "disco#info",
            request(
                BOB_PHONE,
                &id("info"),
                false,
                disco(NS_DISCO_INFO, Some(node)),
            ),
        ),
        (
            "disco#items",
            request(
                BOB_PHONE,
                &id("ditems"),
                false,
                disco(NS_DISCO_ITEMS, Some(node)),
            ),
        ),
        (
            "items",
            request(BOB_PHONE, &id("items"), false, items(node)),
        ),
        (
            "subscribe",
            request(
                BOB_PHONE,
                &id("sub"),
                true,
                subscription("subscribe", node, BOB),
            ),
        ),
        (
            "unsubscribe",
            request(
                BOB_PHONE,
                &id("unsub"),
                true,
                subscription("unsubscribe", node, BOB),
            ),
        ),
        (
            "publish",
            request(BOB_PHONE, &id("pub"), true, publish(node, "lounge")),
        ),
        (
            "retract",
            request(BOB_PHONE, &id("ret"), true, retract(node, "lounge")),
        ),
        (
            "configure get",
            request(
                BOB_PHONE,
                &id("cfg"),
                false,
                pubsub(NS_PUBSUB_OWNER, own("configure")),
            ),
        ),
        (
            "configure set",
            request(
                BOB_PHONE,
                &id("cfgset"),
                true,
                configure(node, vec![field("pubsub#access_model", "open")]),
            ),
        ),
        (
            "delete",
            request(
                BOB_PHONE,
                &id("del"),
                true,
                pubsub(NS_PUBSUB_OWNER, own("delete")),
            ),
        ),
        (
            "purge",
            request(
                BOB_PHONE,
                &id("purge"),
                true,
                pubsub(NS_PUBSUB_OWNER, own("purge")),
            ),
        ),
        (
            "affiliations get",
            request(
                BOB_PHONE,
                &id("affs"),
                false,
                pubsub(NS_PUBSUB_OWNER, own("affiliations")),
            ),
        ),
        (
            "affiliations set",
            request(
                BOB_PHONE,
                &id("affset"),
                true,
                affiliation(node, BOB, "owner"),
            ),
        ),
        (
            "subscriptions get",
            request(
                BOB_PHONE,
                &id("subs"),
                false,
                pubsub(NS_PUBSUB_OWNER, own("subscriptions")),
            ),
        ),
        (
            "subscriptions set",
            request(
                BOB_PHONE,
                &id("subset"),
                true,
                owner_subscription(node, BOB),
            ),
        ),
    ]
}

/// The answers of `home-b.test`'s requests about `node`, each reduced to
/// its error, in the order of [`requests_naming`].
async fn refusals(federated: &Federated, node: &str, tag: &str) -> Vec<(&'static str, Element)> {
    let mut refusals = Vec::new();
    for (kind, iq) in requests_naming(node, tag) {
        let answer = ask(federated, iq).await;
        assert_eq!(answer.attr("type"), Some("error"), "{kind}: {answer:?}");
        assert_eq!(answer.attr("from"), Some(SPACES), "{kind}");
        refusals.push((kind, error_of(&answer).expect("an error element")));
    }
    refusals
}

#[tokio::test]
async fn a_federated_space_is_listed_and_described_as_a_space() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    space(&federated, CLOSED, NodeConfig::spaces_public()).await;
    affiliate(&federated, DEV, "victor@example.com", Affiliation::Owner).await;

    // "Fetching spaces from a spaces service": the open Space and no other.
    let listed = ask(
        &federated,
        request(BOB_PHONE, "list-1", false, disco(NS_DISCO_ITEMS, None)),
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert_eq!(listed.attr("from"), Some(SPACES));
    assert_eq!(listed.attr("to"), Some(BOB_PHONE));
    assert_eq!(item_nodes(&listed), [DEV]);

    // "Fetching the metadata associated to a space": a leaf node whose
    // meta-data form names it a Space.
    let described = ask(
        &federated,
        request(BOB_PHONE, "info-1", false, disco(NS_DISCO_INFO, Some(DEV))),
    )
    .await;
    assert_eq!(described.attr("type"), Some("result"), "{described:?}");
    let identity = descendant(&described, "identity").expect("an identity");
    assert_eq!(identity.attr("category"), Some("pubsub"));
    assert_eq!(identity.attr("type"), Some("leaf"));
    assert_eq!(
        form_value(&described, "pubsub#type").as_deref(),
        Some("urn:xmpp:spaces:0")
    );
    assert_eq!(form_value(&described, "pubsub#title").as_deref(), Some(DEV));
    // No account of this deployment and none of its policy is named.
    for var in [
        "pubsub#owner",
        "pubsub#creator",
        "pubsub#access_model",
        "pubsub#affiliation",
        "pubsub#creation_date",
    ] {
        assert_eq!(form_value(&described, var), None, "{var}");
    }
    assert!(!String::from(&described).contains("victor@example.com"));

    // The service itself: a PubSub service, with the features a remote
    // entity is told.
    let service = ask(
        &federated,
        request(BOB_PHONE, "info-2", false, disco(NS_DISCO_INFO, None)),
    )
    .await;
    let identity = descendant(&service, "identity").expect("an identity");
    assert_eq!(identity.attr("category"), Some("pubsub"));
    assert_eq!(identity.attr("type"), Some("service"));
    assert_eq!(
        features(&service),
        [NS_DISCO_INFO, NS_DISCO_ITEMS, NS_PUBSUB]
    );
}

/// The no-existence-leak rule: for every request that names a node, a
/// Space whose `federated` setting is off is answered with the very error
/// a node that does not exist is answered with.
#[tokio::test]
async fn a_space_that_is_not_federated_answers_exactly_like_a_missing_node() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    space(&federated, CLOSED, NodeConfig::spaces_public()).await;
    // Whatever the Space holds for the asker changes nothing.
    affiliate(&federated, CLOSED, BOB, Affiliation::Owner).await;
    channel(&federated, "lounge", true).await;
    list_in(&federated, CLOSED, "lounge", None).await;

    let closed = refusals(&federated, CLOSED, "closed").await;
    let missing = refusals(&federated, MISSING, "missing").await;
    assert_eq!(closed.len(), missing.len());
    for ((kind, closed), (_, missing)) in closed.iter().zip(&missing) {
        assert_eq!(closed, missing, "{kind}");
    }
    // The requests that are refused before a node is looked at are refused
    // alike too: a subscription for somebody else's address.
    for node in [CLOSED, MISSING] {
        let answer = ask(
            &federated,
            request(
                BOB_PHONE,
                &["other-", node].concat(),
                true,
                subscription("subscribe", node, "mallory@home-b.test"),
            ),
        )
        .await;
        assert_eq!(error_condition(&answer).as_deref(), Some("bad-request"));
    }
    let listed = ask(
        &federated,
        request(BOB_PHONE, "list-2", false, disco(NS_DISCO_ITEMS, None)),
    )
    .await;
    assert!(item_nodes(&listed).is_empty(), "{listed:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
}

/// XEP-0503 "exposure": a private Space is told only to those it admits.
#[tokio::test]
async fn a_private_space_is_told_only_to_an_asker_it_admits() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    space(&federated, DEV, NodeConfig::spaces_private()).await;
    federate(&federated, DEV).await;
    channel(&federated, "lounge", true).await;
    list_in(&federated, DEV, "lounge", None).await;

    let stranger = refusals(&federated, DEV, "private").await;
    let missing = refusals(&federated, MISSING, "absent").await;
    for ((kind, stranger), (_, missing)) in stranger.iter().zip(&missing) {
        assert_eq!(stranger, missing, "{kind}");
    }
    let listed = ask(
        &federated,
        request(BOB_PHONE, "list-3", false, disco(NS_DISCO_ITEMS, None)),
    )
    .await;
    assert!(item_nodes(&listed).is_empty());

    // An owner or publisher affiliation of an account whose home is not a
    // trusted home is not honoured, and contributes nothing: not the
    // membership it would otherwise imply.
    for (id, unhonoured) in [
        ("items-owner", Affiliation::Owner),
        ("items-publisher", Affiliation::Publisher),
    ] {
        affiliate(&federated, DEV, BOB, unhonoured).await;
        let answer = ask(&federated, request(BOB_PHONE, id, false, items(DEV))).await;
        assert_eq!(error_condition(&answer).as_deref(), Some("item-not-found"));
    }

    // A membership is honoured on any stream of an admitted domain.
    affiliate(&federated, DEV, BOB, Affiliation::Member).await;
    let listed = ask(
        &federated,
        request(BOB_PHONE, "list-4", false, disco(NS_DISCO_ITEMS, None)),
    )
    .await;
    assert_eq!(item_nodes(&listed), [DEV]);
    let read = ask(&federated, request(BOB_PHONE, "items-1", false, items(DEV))).await;
    assert_eq!(read.attr("type"), Some("result"), "{read:?}");
    assert_eq!(item_ids(&read), [room("lounge").to_string()]);
    let children = ask(
        &federated,
        request(
            BOB_PHONE,
            "children-1",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(children.attr("type"), Some("result"), "{children:?}");
    assert_eq!(listed_children(&children), [room("lounge").to_string()]);
}

/// "Discovering the content of a space": the bookmarks of the rooms a
/// remote asker may be told of, and no other item that is no room's
/// bookmark or Space image.
#[tokio::test]
async fn a_space_lists_only_the_rooms_that_are_open_to_other_servers() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let victor = federated.account("victor").await;
    channel(&federated, "lounge", true).await;
    channel(&federated, "backroom", false).await;
    list_in(&federated, DEV, "lounge", Some(&victor)).await;
    list_in(&federated, DEV, "backroom", Some(&victor)).await;
    // A bookmark of a room that has no catalog row, and an image item
    // whose payload names no image (stored without a publish, which would
    // refuse it).
    list_in(&federated, DEV, "ghost", None).await;
    let avatar = PubSubItem::new(
        Some("urn:xmpp:spaces:avatar:metadata:0".to_owned()),
        Some(Element::builder("metadata", "urn:xmpp:avatar:metadata").build()),
    );
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces(), DEV, &avatar, None, false)
        .await
        .expect("the stored image item");

    let read = ask(&federated, request(BOB_PHONE, "items-2", false, items(DEV))).await;
    assert_eq!(read.attr("type"), Some("result"), "{read:?}");
    assert_eq!(item_ids(&read), [room("lounge").to_string()]);
    let conference = descendant(&read, "conference").expect("the bookmark");
    assert_eq!(conference.ns(), "urn:xmpp:bookmarks:1");
    // The account that listed the room is not named.
    assert!(!String::from(&read).contains("victor@example.com"));

    // "Advertising space membership": the node's `disco#items` lists the
    // same room, and nothing else the node holds.
    let children = ask(
        &federated,
        request(
            BOB_PHONE,
            "children-2",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(children.attr("type"), Some("result"), "{children:?}");
    assert_eq!(
        descendant(&children, "query").and_then(|query| query.attr("node")),
        Some(DEV)
    );
    assert_eq!(listed_children(&children), [room("lounge").to_string()]);
    assert!(!String::from(&children).contains("victor@example.com"));

    // Asking for the closed room's item by id tells nothing more.
    let by_id = ask(
        &federated,
        request(
            BOB_PHONE,
            "items-3",
            false,
            pubsub(
                NS_PUBSUB,
                Element::builder("items", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(
                                minidom::rxml::xml_ncname!("id").to_owned(),
                                room("backroom").to_string(),
                            )
                            .build(),
                    )
                    .build(),
            ),
        ),
    )
    .await;
    assert_eq!(by_id.attr("type"), Some("result"), "{by_id:?}");
    assert!(item_ids(&by_id).is_empty());
}

/// "Spaces images": a remote asker that may read a Space reads its avatar
/// and banner items with its rooms, as published and without their
/// publisher, by the Space's items and by their ids. They are no rooms of
/// the Space: its node's `disco#items` lists its rooms alone. Nothing the
/// images name is requested.
#[tokio::test]
async fn a_remote_reader_reads_a_spaces_images_with_its_rooms() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let victor = federated.account("victor").await;
    channel(&federated, "lounge", true).await;
    list_in(&federated, DEV, "lounge", Some(&victor)).await;
    let (avatar_host, avatar_url) = image_host("/avatar.png").await;
    let (banner_host, banner_url) = image_host("/banner.png").await;
    store_item(
        &federated,
        DEV,
        ITEM_SPACE_AVATAR_METADATA,
        image_metadata(&avatar_url),
    )
    .await;
    store_item(
        &federated,
        DEV,
        ITEM_SPACE_BANNER_METADATA,
        image_metadata(&banner_url),
    )
    .await;

    let read = ask(
        &federated,
        request(BOB_PHONE, "images-1", false, items(DEV)),
    )
    .await;
    assert_eq!(read.attr("type"), Some("result"), "{read:?}");
    let mut told = item_ids(&read);
    told.sort();
    let mut expected = vec![
        room("lounge").to_string(),
        ITEM_SPACE_AVATAR_METADATA.to_owned(),
        ITEM_SPACE_BANNER_METADATA.to_owned(),
    ];
    expected.sort();
    assert_eq!(told, expected);
    let avatar = read
        .children()
        .flat_map(Element::children)
        .flat_map(Element::children)
        .find(|item| item.attr("id") == Some(ITEM_SPACE_AVATAR_METADATA))
        .expect("the avatar item");
    assert_eq!(avatar.attr("publisher"), None);
    assert_eq!(
        avatar
            .get_child("metadata", NS_AVATAR_METADATA)
            .and_then(|metadata| metadata.get_child("info", NS_AVATAR_METADATA))
            .and_then(|info| info.attr("url")),
        Some(avatar_url.as_str())
    );
    assert!(!String::from(&read).contains("victor@example.com"));

    // By its id.
    let by_id = ask(
        &federated,
        request(
            BOB_PHONE,
            "images-2",
            false,
            pubsub(
                NS_PUBSUB,
                Element::builder("items", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(
                                minidom::rxml::xml_ncname!("id").to_owned(),
                                ITEM_SPACE_BANNER_METADATA,
                            )
                            .build(),
                    )
                    .build(),
            ),
        ),
    )
    .await;
    assert_eq!(by_id.attr("type"), Some("result"), "{by_id:?}");
    assert_eq!(item_ids(&by_id), [ITEM_SPACE_BANNER_METADATA]);

    // The Space's rooms are its rooms alone.
    let children = ask(
        &federated,
        request(
            BOB_PHONE,
            "images-3",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(children.attr("type"), Some("result"), "{children:?}");
    assert_eq!(listed_children(&children), [room("lounge").to_string()]);

    // A Space that is not open to other servers tells nothing of its
    // images.
    space(&federated, CLOSED, NodeConfig::spaces_public()).await;
    store_item(
        &federated,
        CLOSED,
        ITEM_SPACE_AVATAR_METADATA,
        image_metadata(&avatar_url),
    )
    .await;
    let closed = ask(
        &federated,
        request(BOB_PHONE, "images-4", false, items(CLOSED)),
    )
    .await;
    assert_eq!(error_condition(&closed).as_deref(), Some("item-not-found"));
    assert!(item_ids(&closed).is_empty(), "{closed:?}");

    assert!(!fetched(&avatar_host).await, "nothing requests the avatar");
    assert!(!fetched(&banner_host).await, "nothing requests the banner");
}

/// The rooms a Space's node lists are paged with XEP-0059: the count and
/// the first item's index are of the rooms this asker may be told of.
#[tokio::test]
async fn a_spaces_rooms_are_listed_in_pages() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    for (id, open) in [
        ("alpha", true),
        ("bravo", true),
        ("charlie", true),
        ("backroom", false),
    ] {
        channel(&federated, id, open).await;
        list_in(&federated, DEV, id, None).await;
    }
    let paged = |after: Option<&str>| {
        let mut set = Element::builder("set", NS_RSM)
            .append(Element::builder("max", NS_RSM).append("2").build());
        if let Some(after) = after {
            set = set.append(Element::builder("after", NS_RSM).append(after).build());
        }
        Element::builder("query", NS_DISCO_ITEMS)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), DEV)
            .append(set.build())
            .build()
    };
    let rsm = |answer: &Element, name: &str| {
        descendant(answer, "set")
            .and_then(|set| set.get_child(name, NS_RSM))
            .map(|child| (child.attr("index").map(ToOwned::to_owned), child.text()))
    };

    let first = ask(&federated, request(BOB_PHONE, "page-1", false, paged(None))).await;
    assert_eq!(first.attr("type"), Some("result"), "{first:?}");
    assert_eq!(
        listed_children(&first),
        [room("alpha").to_string(), room("bravo").to_string()]
    );
    assert_eq!(rsm(&first, "count"), Some((None, "3".to_owned())));
    assert_eq!(
        rsm(&first, "first"),
        Some((Some("0".to_owned()), room("alpha").to_string()))
    );
    assert_eq!(rsm(&first, "last"), Some((None, room("bravo").to_string())));

    let bravo = room("bravo").to_string();
    let second = ask(
        &federated,
        request(BOB_PHONE, "page-2", false, paged(Some(&bravo))),
    )
    .await;
    assert_eq!(listed_children(&second), [room("charlie").to_string()]);
    assert_eq!(
        rsm(&second, "first"),
        Some((Some("2".to_owned()), room("charlie").to_string()))
    );
}

/// A Space's node lists, to accounts at other servers, the rooms among its
/// 256 most recently published items; a Space that holds more answers with
/// a `<set/>` though none was asked for, and tells no count, since what the
/// bounded read left out is not known.
#[tokio::test]
async fn a_spaces_listing_beyond_its_read_tells_no_count() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let ids: Vec<String> = (0..257).map(|index| format!("room-{index:03}")).collect();
    for id in &ids {
        channel(&federated, id, true).await;
        list_in(&federated, DEV, id, None).await;
    }
    let listed = ask(
        &federated,
        request(
            BOB_PHONE,
            "beyond-1",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    let children = listed_children(&listed);
    assert_eq!(children.len(), 256);
    // The oldest item is the one the read left out.
    assert!(!children.contains(&room(&ids[0]).to_string()));
    assert!(children.contains(&room(&ids[256]).to_string()));
    let set = descendant(&listed, "set").expect("the unasked <set/>");
    assert!(set.get_child("count", NS_RSM).is_none(), "{listed:?}");
    assert_eq!(
        set.get_child("first", NS_RSM)
            .and_then(|first| first.attr("index").map(ToOwned::to_owned)),
        Some("0".to_owned())
    );
}

/// Holds a room's actor until its sender is dropped or sent on.
struct Wedge(tokio::sync::oneshot::Receiver<()>);

impl kameo::message::Message<Wedge> for waddle_xmpp::muc::room_actor::RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        wedge: Wedge,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = wedge.0.await;
    }
}

/// A room whose openness to other servers cannot be read now (its live
/// actor does not answer) is left out of a Space's listing for an account
/// at another server, and the listing tells no count, since it does not
/// know whether it left a room out. Once the actor answers, the room is
/// listed and the count told.
#[tokio::test]
async fn a_spaces_listing_that_could_not_read_a_room_tells_no_count() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    for id in ["alpha", "lounge"] {
        channel(&federated, id, true).await;
        list_in(&federated, DEV, id, None).await;
    }
    // The lounge's actor is live on this replica, which then reads the
    // room's openness from it rather than from the catalog, and every ask
    // of it waits behind a wedge.
    let registry = waddle_xmpp::muc::RoomRegistry::wrap(
        federated.loopback.state.deps.protocol.room_registry.clone(),
    );
    let actor = registry
        .get_or_create_room(
            room("lounge"),
            "lounge".to_owned(),
            "lounge".to_owned(),
            waddle_xmpp::muc::RoomConfig {
                public_room: true,
                federated: true,
                ..Default::default()
            },
        )
        .await
        .expect("the room registry answers")
        .actor_ref;
    let (release, wedged) = tokio::sync::oneshot::channel();
    actor
        .tell(Wedge(wedged))
        .await
        .expect("the wedge is queued");

    let listed = ask(
        &federated,
        request(
            BOB_PHONE,
            "undecided-1",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert_eq!(listed_children(&listed), [room("alpha").to_string()]);
    let set = descendant(&listed, "set").expect("the unasked <set/>");
    assert!(set.get_child("count", NS_RSM).is_none(), "{listed:?}");

    let _ = release.send(());
    let listed = ask(
        &federated,
        request(
            BOB_PHONE,
            "undecided-2",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(
        listed_children(&listed),
        [room("alpha").to_string(), room("lounge").to_string()]
    );
    // A complete listing asked for no page carries no `<set/>`.
    assert!(descendant(&listed, "set").is_none(), "{listed:?}");
}

/// An account banned from a public Space is refused its node's listing,
/// as a read of its items is.
#[tokio::test]
async fn a_banned_account_is_refused_a_spaces_listing() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    channel(&federated, "lounge", true).await;
    list_in(&federated, DEV, "lounge", None).await;
    affiliate(&federated, DEV, BOB, Affiliation::Outcast).await;
    let children = ask(
        &federated,
        request(
            BOB_PHONE,
            "banned-children",
            false,
            disco(NS_DISCO_ITEMS, Some(DEV)),
        ),
    )
    .await;
    assert_eq!(error_condition(&children).as_deref(), Some("forbidden"));
    let read = ask(
        &federated,
        request(BOB_PHONE, "banned-items", false, items(DEV)),
    )
    .await;
    assert_eq!(error_condition(&read).as_deref(), Some("forbidden"));
}

/// A session's read of a Space publishes the bookmarks its linked rooms
/// lack and repairs their parent. A remote principal's read writes
/// nothing.
#[tokio::test]
async fn a_remote_read_of_a_space_writes_nothing() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    channel(&federated, "lounge", true).await;
    let state = &federated.loopback.state;
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&ChannelSpaceLink {
            channel_jid: room("lounge"),
            space_jid: space_jid_for_node(&spaces(), &SpaceNode::from(DEV)).expect("a Space JID"),
            space_node: SpaceNode::from(DEV),
            created_at: 0,
        })
        .await
        .expect("the channel-space link");

    let read = ask(&federated, request(BOB_PHONE, "items-4", false, items(DEV))).await;
    assert_eq!(read.attr("type"), Some("result"), "{read:?}");
    assert!(item_ids(&read).is_empty());
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces(), DEV, None, &[])
        .await
        .expect("the Space's items");
    assert!(stored.is_empty(), "the read published {stored:?}");
}

#[tokio::test]
async fn the_service_answers_ping_and_refuses_what_is_not_reviewed() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;

    let pong = ask(
        &federated,
        request(
            BOB_PHONE,
            "ping-1",
            false,
            Element::builder("ping", "urn:xmpp:ping").build(),
        ),
    )
    .await;
    assert_eq!(pong.attr("type"), Some("result"), "{pong:?}");

    // An ad-hoc command, and any request to an address that is not the
    // service's bare domain, whatever it names.
    let command = ask(
        &federated,
        request(
            BOB_PHONE,
            "cmd-1",
            true,
            Element::builder("command", "http://jabber.org/protocol/commands").build(),
        ),
    )
    .await;
    assert_eq!(
        error_condition(&command).as_deref(),
        Some("service-unavailable")
    );
    for (id, to) in [
        ("addr-1", "dev@spaces.example.com"),
        ("addr-2", "nowhere@spaces.example.com"),
        ("addr-3", "spaces.example.com/dev"),
    ] {
        let mut iq = request(BOB_PHONE, id, false, disco(NS_DISCO_INFO, None));
        *iq.to_mut() = Some(s2s_spaces_support::jid(to));
        let answer = ask(&federated, iq).await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("service-unavailable"),
            "{to}"
        );
    }
    // No Space is created for a remote principal.
    let created = ask(
        &federated,
        request(
            BOB_PHONE,
            "create-1",
            true,
            pubsub(NS_PUBSUB, of_node("create", NS_PUBSUB, "mine")),
        ),
    )
    .await;
    assert_eq!(error_condition(&created).as_deref(), Some("forbidden"));
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    assert!(storage
        .get_node(&spaces(), "mine")
        .await
        .expect("the node is read")
        .is_none());
}

/// ADR-0020 D1 at the effect: a domain the snapshot the replica runs no
/// longer admits is refused everything, on a stream admitted before.
#[tokio::test]
async fn a_domain_the_replica_no_longer_admits_is_refused_everything() {
    let pki = Pki::new();
    let federated = hosting_with(
        &pki,
        Adjust {
            effect_snapshot: Some(|spec| {
                spec.allow = Vec::new();
                spec.private = Vec::new();
                spec.deny = vec![PEER];
            }),
            ..Adjust::default()
        },
    )
    .await;
    federated_space(&federated, DEV).await;

    for (id, set, payload) in [
        ("d1-list", false, disco(NS_DISCO_ITEMS, None)),
        ("d1-info", false, disco(NS_DISCO_INFO, Some(DEV))),
        ("d1-items", false, items(DEV)),
        ("d1-sub", true, subscription("subscribe", DEV, BOB)),
        (
            "d1-ping",
            false,
            Element::builder("ping", "urn:xmpp:ping").build(),
        ),
    ] {
        let answer = ask(&federated, request(BOB_PHONE, id, set, payload)).await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("forbidden"),
            "{id}"
        );
    }
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
}

/// The `federated` setting is the Space owner's to read and to set, in the
/// node's configuration form, and is off until the owner turns it on.
#[tokio::test]
async fn an_owner_opens_and_closes_a_space_in_its_configuration_form() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    space(&federated, DEV, NodeConfig::spaces_public()).await;
    federated.account("victor").await;
    affiliate(&federated, DEV, "victor@example.com", Affiliation::Owner).await;
    let mut victor = federated.loopback.client("victor", "desk").await;

    let form = round_trip(
        &mut victor,
        get(
            "cfg-1",
            SPACES,
            pubsub(NS_PUBSUB_OWNER, of_node("configure", NS_PUBSUB_OWNER, DEV)),
        ),
    )
    .await;
    assert!(is_result(&form), "{form:?}");
    let shown = result_payload(&form).expect("the configuration form");
    assert_eq!(form_value(shown, FIELD_FEDERATED).as_deref(), Some("0"));
    let hidden = ask(&federated, request(BOB_PHONE, "cfg-a", false, items(DEV))).await;
    assert_eq!(error_condition(&hidden).as_deref(), Some("item-not-found"));

    // A value that is no boolean changes nothing.
    let malformed = round_trip(
        &mut victor,
        set(
            "cfg-2",
            SPACES,
            configure(DEV, vec![field(FIELD_FEDERATED, "maybe")]),
        ),
    )
    .await;
    assert!(
        is_error(&malformed, DefinedCondition::BadRequest),
        "{malformed:?}"
    );
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);

    let opened = round_trip(
        &mut victor,
        set(
            "cfg-3",
            SPACES,
            configure(DEV, vec![field(FIELD_FEDERATED, "1")]),
        ),
    )
    .await;
    assert!(is_result(&opened), "{opened:?}");
    let open = ask(&federated, request(BOB_PHONE, "cfg-b", false, items(DEV))).await;
    assert_eq!(open.attr("type"), Some("result"), "{open:?}");
    let subscribed = ask(
        &federated,
        request(
            BOB_PHONE,
            "cfg-c",
            true,
            subscription("subscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);

    // A configuration that does not carry the field leaves it as it is.
    let untouched = round_trip(
        &mut victor,
        set(
            "cfg-4",
            SPACES,
            configure(DEV, vec![field("pubsub#access_model", "open")]),
        ),
    )
    .await;
    assert!(is_result(&untouched), "{untouched:?}");
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 1);

    // Turning it off closes the Space and forgets the subscriptions remote
    // accounts made to it.
    let closed = round_trip(
        &mut victor,
        set(
            "cfg-5",
            SPACES,
            configure(DEV, vec![field(FIELD_FEDERATED, "false")]),
        ),
    )
    .await;
    assert!(is_result(&closed), "{closed:?}");
    let gone = ask(&federated, request(BOB_PHONE, "cfg-d", false, items(DEV))).await;
    assert_eq!(error_condition(&gone).as_deref(), Some("item-not-found"));
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);
}

/// The setting belongs to the node it was turned on for: a Space that is
/// deleted and created again under the same name is not federated.
#[tokio::test]
async fn a_space_created_again_under_a_deleted_spaces_name_is_not_federated() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    federated.account("victor").await;
    affiliate(&federated, DEV, "victor@example.com", Affiliation::Owner).await;
    let mut victor = federated.loopback.client("victor", "desk").await;
    let subscribed = ask(
        &federated,
        request(
            BOB_PHONE,
            "del-a",
            true,
            subscription("subscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");

    let deleted = round_trip(
        &mut victor,
        set(
            "del-1",
            SPACES,
            pubsub(NS_PUBSUB_OWNER, of_node("delete", NS_PUBSUB_OWNER, DEV)),
        ),
    )
    .await;
    assert!(is_result(&deleted), "{deleted:?}");
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);

    // A row that outlived its Space counts for nothing either: it names
    // another node's creation time.
    space(&federated, CLOSED, NodeConfig::spaces_public()).await;
    federate(&federated, CLOSED).await;
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    assert!(storage
        .delete_node(&spaces(), CLOSED)
        .await
        .expect("the node is deleted"));
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    space(&federated, CLOSED, NodeConfig::spaces_public()).await;
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 1);
    let answer = ask(
        &federated,
        request(BOB_PHONE, "del-b", false, items(CLOSED)),
    )
    .await;
    assert_eq!(error_condition(&answer).as_deref(), Some("item-not-found"));
}
