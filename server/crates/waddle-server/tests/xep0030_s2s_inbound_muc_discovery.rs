//! XEP-0030, XEP-0199 and XEP-0410 across S2S, inbound: an asker at another
//! server discovers and pings the MUC service and rooms of this deployment
//! (ADR-0020 D10).
//!
//! The service answers `disco#info` as a multi-user chat service and names
//! no `server` identity, which is what lets a peer tell it from an account
//! domain (XEP-0045 §6.2); a room answers with a `conference` identity
//! (§6.4) and a room that does not exist with `item-not-found`. Remote
//! askers are told service discovery, the MUC protocol and a room's
//! XEP-0045 type flags, and nothing else of what the deployment advertises
//! to its own sessions.
//!
//! The service's `disco#items` lists the rooms a remote asker may reach
//! (§6.3): public, open to other servers and no group conversation, paged
//! with XEP-0059. A room that is not listed is absent, as one that does not
//! exist is; a room's `disco#items` lists no occupants (§6.5). That listing
//! is the MUC fallback of XEP-0503 version 0.3.0 (xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2): a client without Spaces walks
//! from the server's items to the service's and enters a Space's room by
//! its plain JID, whatever the Space's own setting.
//!
//! Ping is answered for the service and a room; a self-ping (XEP-0410) is
//! answered from the occupant's seat: a result while joined,
//! `not-acceptable` otherwise, and `remote-server-timeout`, which a client
//! takes no decision from, while the room cannot say.
//!
//! The deployments are those of `s2s_inbound_muc_support`: no PostgreSQL,
//! SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use jid::BareJid;
use minidom::Element;
use s2s_inbound_muc_support::{
    error_condition, error_of, federated_form, local_join, local_receives, local_set,
    not_federating, presence_between, remote_get, remote_join, remote_leave,
    room_with_remote_occupant, statuses, wait_for, Hosted, ARRIVES, BOB, DOMAIN, HOME_B, MUC, ROOM,
};
use waddle_server::s2s::route::RouterLimits;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::disco::{DiscoItemsQuery, DiscoItemsResult};
use xmpp_parsers::rsm::SetQuery;

const DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
const NS_PING: &str = "urn:xmpp:ping";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";
/// The Spaces service of `example.com`.
const SPACES: &str = "spaces.example.com";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

fn info() -> Element {
    Element::builder("query", DISCO_INFO).build()
}

fn items() -> Element {
    Element::builder("query", DISCO_ITEMS).build()
}

fn ping() -> Element {
    Element::builder("ping", NS_PING).build()
}

/// A `disco#items` query that asks for at most `max` items after `after`
/// (XEP-0059 §2.2).
fn paged(max: usize, after: Option<&str>) -> Element {
    Element::from(DiscoItemsQuery {
        node: None,
        rsm: Some(SetQuery {
            max: Some(max),
            after: after.map(str::to_owned),
            before: None,
            index: None,
        }),
    })
}

/// The `disco#items` result an answer carries.
fn listing(answer: &Element) -> DiscoItemsResult {
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    let query = answer
        .children()
        .find(|child| child.is("query", DISCO_ITEMS))
        .expect("a disco#items result")
        .clone();
    DiscoItemsResult::try_from(query).expect("a well-formed disco#items result")
}

/// The addresses a `disco#items` result lists, in its order.
fn jids(listed: &DiscoItemsResult) -> Vec<String> {
    listed
        .items
        .iter()
        .map(|item| item.jid.to_string())
        .collect()
}

/// The room `id` of the MUC service.
fn room(id: &str) -> String {
    [id, "@", MUC].concat()
}

/// A local client's `disco#items` request to `to`, and its answer.
async fn local_items(client: &mut WsXmppClient, to: &str, id: &str) -> Element {
    let request = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(items())
        .build();
    client.send(&String::from(&request)).await.expect("send");
    local_receives(client, ARRIVES, |stanza| {
        stanza.name() == "iq" && stanza.attr("id") == Some(id)
    })
    .await
    .expect("the answer to the request")
}

/// The `(category, type)` identities and the features of a `disco#info`
/// result.
fn described(result: &Element) -> (Vec<(String, String)>, Vec<String>) {
    let query = result
        .children()
        .find(|child| child.is("query", DISCO_INFO))
        .expect("a disco#info result");
    let identities = query
        .children()
        .filter(|child| child.name() == "identity")
        .filter_map(|identity| {
            Some((
                identity.attr("category")?.to_owned(),
                identity.attr("type")?.to_owned(),
            ))
        })
        .collect();
    let features = query
        .children()
        .filter(|child| child.name() == "feature")
        .filter_map(|feature| feature.attr("var").map(str::to_owned))
        .collect();
    (identities, features)
}

#[tokio::test]
async fn the_service_answers_as_a_multi_user_chat_service_to_a_server_and_to_an_account() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    // A peer verifying a room's service asks from its bare domain.
    for (from, id) in [(HOME_B, "d1"), (BOB, "d2")] {
        hosted.peer_sends(remote_get(from, MUC, id, info())).await;
        let answer = hosted.answered(id).await;
        assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
        assert_eq!(answer.attr("from"), Some(MUC));
        assert_eq!(answer.attr("to"), Some(from));
        let (identities, features) = described(&answer);
        assert!(identities.contains(&("conference".to_owned(), "text".to_owned())));
        assert!(!identities.iter().any(|(category, _)| category == "server"));
        assert!(features.contains(&"http://jabber.org/protocol/muc".to_owned()));
        // Nothing beyond discovery and the MUC protocol is advertised.
        assert!(
            features.iter().all(|feature| {
                feature == DISCO_INFO
                    || feature == DISCO_ITEMS
                    || feature == "http://jabber.org/protocol/muc"
                    || feature.starts_with("muc_")
            }),
            "{features:?}"
        );
    }
}

#[tokio::test]
async fn a_room_answers_as_a_conference_and_a_missing_one_with_item_not_found() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    hosted
        .peer_sends(remote_get(HOME_B, ROOM, "r1", info()))
        .await;
    let answer = hosted.answered("r1").await;
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    let (identities, features) = described(&answer);
    assert!(identities
        .iter()
        .any(|(category, _)| category == "conference"));
    assert!(features.contains(&"http://jabber.org/protocol/muc".to_owned()));
    assert!(
        !features.iter().any(|feature| feature.starts_with("urn:")),
        "{features:?}"
    );

    hosted
        .peer_sends(remote_get(HOME_B, "nowhere@muc.example.com", "r2", info()))
        .await;
    let missing = hosted.answered("r2").await;
    assert_eq!(missing.attr("type"), Some("error"));
    assert_eq!(error_condition(&missing).as_deref(), Some("item-not-found"));

    // No node of the service or of a room is described to remote askers.
    let node = Element::builder("query", DISCO_INFO)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "http://jabber.org/protocol/commands",
        )
        .build();
    hosted.peer_sends(remote_get(BOB, ROOM, "r3", node)).await;
    let refused = hosted.answered("r3").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("item-not-found"));
}

#[tokio::test]
async fn a_room_that_is_not_federated_is_described_as_one_that_does_not_exist() {
    let hosted = Hosted::start(RouterLimits::default(), not_federating).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    let missing = "nowhere@muc.example.com";
    // The room exists and its `federated` setting is off: its description
    // is refused with the condition, type and text of a missing room's.
    hosted.peer_sends(remote_get(BOB, ROOM, "c1", info())).await;
    let closed = hosted.answered("c1").await;
    hosted
        .peer_sends(remote_get(BOB, missing, "c2", info()))
        .await;
    let absent = hosted.answered("c2").await;
    assert_eq!(closed.attr("type"), Some("error"), "{closed:?}");
    assert_eq!(error_condition(&closed).as_deref(), Some("item-not-found"));
    assert_eq!(error_of(&closed), error_of(&absent));
    // Neither a ping nor a `disco#items` request tells them apart.
    for (payload, first, second) in [(ping(), "c3", "c4"), (items(), "c5", "c6")] {
        hosted
            .peer_sends(remote_get(BOB, ROOM, first, payload.clone()))
            .await;
        let of_closed = hosted.answered(first).await;
        hosted
            .peer_sends(remote_get(BOB, missing, second, payload))
            .await;
        let of_absent = hosted.answered(second).await;
        assert_eq!(of_closed.attr("type"), of_absent.attr("type"));
        assert_eq!(
            of_closed.children().collect::<Vec<_>>(),
            of_absent.children().collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn the_service_lists_its_open_rooms_and_a_room_lists_no_occupants() {
    let (hosted, _alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // XEP-0045 §6.3: the room, public and open to other servers, is listed
    // by its plain JID, in one answer without a `<set/>`.
    hosted.peer_sends(remote_get(BOB, MUC, "i1", items())).await;
    let listed = listing(&hosted.answered("i1").await);
    assert_eq!(jids(&listed), [ROOM]);
    assert!(listed.rsm.is_none(), "{listed:?}");
    // §6.5: a room's occupants are not listed, though one is at the
    // asker's own server.
    hosted
        .peer_sends(remote_get(BOB, ROOM, "i2", items()))
        .await;
    let occupants = listing(&hosted.answered("i2").await);
    assert!(occupants.items.is_empty(), "{occupants:?}");
}

/// A room a remote asker may not reach is absent from the listing, as a
/// room that does not exist is: one whose `federated` setting is off, a
/// hidden room (§6.3) and a group conversation. A session of this
/// deployment is still told of the rooms its own rules list.
#[tokio::test]
async fn the_service_lists_no_room_a_remote_asker_may_not_reach() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    hosted.catalogued_room("open", true, true, "text").await;
    hosted.catalogued_room("closed", true, false, "text").await;
    hosted.catalogued_room("hidden", false, true, "text").await;
    hosted
        .catalogued_room("conversation", true, true, "group-dm")
        .await;
    // An instant room its owner closes to other servers.
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    let closed = local_set(&mut alice, ROOM, "close-1", federated_form(false)).await;
    assert_eq!(closed.attr("type"), Some("result"), "{closed:?}");

    hosted.peer_sends(remote_get(BOB, MUC, "n1", items())).await;
    let listed = listing(&hosted.answered("n1").await);
    assert_eq!(jids(&listed), [room("open")]);

    let local = listing(&local_items(&mut alice, MUC, "n2").await);
    let local = jids(&local);
    for told in [room("open"), room("closed"), ROOM.to_owned()] {
        assert!(local.contains(&told), "{told}: {local:?}");
    }
    for untold in [room("hidden"), room("conversation")] {
        assert!(!local.contains(&untold), "{untold}: {local:?}");
    }
}

/// XEP-0059 paging of the service's listing for a remote asker: the count
/// and the first item's index are of the listing built for it, and a page
/// continues from its last item.
#[tokio::test]
async fn the_room_list_is_paged_for_a_remote_asker() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    for id in ["alpha", "bravo", "charlie"] {
        hosted.catalogued_room(id, true, true, "text").await;
    }
    // A room the asker may not reach is not counted.
    hosted.catalogued_room("closed", true, false, "text").await;

    hosted
        .peer_sends(remote_get(BOB, MUC, "p1", paged(2, None)))
        .await;
    let first = listing(&hosted.answered("p1").await);
    assert_eq!(jids(&first), [room("alpha"), room("bravo")]);
    let set = first.rsm.expect("the page's <set/>");
    assert_eq!(set.count, Some(3));
    let head = set.first.expect("the page's first item");
    assert_eq!((head.index, head.item), (Some(0), room("alpha")));
    assert_eq!(set.last, Some(room("bravo")));

    let after = room("bravo");
    hosted
        .peer_sends(remote_get(BOB, MUC, "p2", paged(2, Some(&after))))
        .await;
    let second = listing(&hosted.answered("p2").await);
    assert_eq!(jids(&second), [room("charlie")]);
    let set = second.rsm.expect("the page's <set/>");
    assert_eq!(set.count, Some(3));
    assert_eq!(set.first.and_then(|first| first.index), Some(2));

    // An anchor is placed by its order and never looked up: one that names
    // a room the asker is not listed tells nothing of it.
    let closed = room("closed");
    hosted
        .peer_sends(remote_get(BOB, MUC, "p3", paged(2, Some(&closed))))
        .await;
    let placed = listing(&hosted.answered("p3").await);
    assert!(placed.items.is_empty(), "{placed:?}");
    assert_eq!(placed.rsm.and_then(|set| set.count), Some(3));

    // A paging request that is not well formed is refused.
    let malformed = Element::builder("query", DISCO_ITEMS)
        .append(
            Element::builder("set", NS_RSM)
                .append(Element::builder("max", NS_RSM).append("many").build())
                .build(),
        )
        .build();
    hosted
        .peer_sends(remote_get(BOB, MUC, "p4", malformed))
        .await;
    let refused = hosted.answered("p4").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("bad-request"));
}

/// The MUC fallback of XEP-0503: a client that does not use Spaces walks
/// from the server's items to the MUC service's, finds a room a Space
/// lists by its plain JID and enters it. The Space is not open to other
/// servers; a room's reach is its own setting, and nothing is inherited
/// from its Space (ADR-0020 D4).
#[tokio::test]
async fn a_client_without_spaces_finds_a_spaces_room_and_enters_it_by_its_plain_jid() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    let spaces: BareJid = SPACES.parse().expect("the Spaces service");
    let storage = &hosted.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&spaces, "dev")
        .await
        .expect("the Space node");
    storage
        .update_node_config(&spaces, "dev", &NodeConfig::spaces_public())
        .await
        .expect("the Space's configuration");
    let lounge: BareJid = ROOM.parse().expect("the room");
    let bookmark = PubSubItem::new(
        Some(ROOM.to_owned()),
        Some(build_bookmark_element(
            &Bookmark::new(lounge).with_name("lounge"),
        )),
    );
    storage
        .publish_item(&spaces, "dev", &bookmark, None, false)
        .await
        .expect("the room's bookmark in the Space");

    hosted
        .peer_sends_to(DOMAIN, remote_get(BOB, DOMAIN, "w1", items()))
        .await;
    let services = listing(&hosted.answered("w1").await);
    assert!(jids(&services).contains(&MUC.to_owned()), "{services:?}");
    hosted.peer_sends(remote_get(BOB, MUC, "w2", items())).await;
    let rooms = listing(&hosted.answered("w2").await);
    assert_eq!(jids(&rooms), [ROOM]);

    hosted.peer_sends(remote_join(BOB, ROOM, "bob", "w3")).await;
    let occupant = [ROOM, "/bob"].concat();
    let own = hosted
        .at_peer(|stanza| {
            presence_between(stanza, &occupant, BOB) && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the remote occupant");
    assert_eq!(own.attr("type"), None, "{own:?}");
}

#[tokio::test]
async fn ping_is_answered_for_the_service_and_a_room() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    for (to, id) in [(MUC, "p1"), (ROOM, "p2")] {
        hosted.peer_sends(remote_get(BOB, to, id, ping())).await;
        let answer = hosted.answered(id).await;
        assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
        assert_eq!(answer.attr("from"), Some(to));
    }
}

#[tokio::test]
async fn a_self_ping_tells_a_remote_occupant_whether_it_is_joined() {
    let (hosted, _alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let own = "lounge@muc.example.com/bob";
    // XEP-0410 §3: joined.
    hosted.peer_sends(remote_get(BOB, own, "s1", ping())).await;
    let joined = hosted.answered("s1").await;
    assert_eq!(joined.attr("type"), Some("result"), "{joined:?}");
    // A ping for another occupant would be relayed to an account.
    hosted
        .peer_sends(remote_get(
            BOB,
            "lounge@muc.example.com/alice",
            "s2",
            ping(),
        ))
        .await;
    let refused = hosted.answered("s2").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("service-unavailable")
    );
    // XEP-0410 §3: not joined, once the occupant left.
    hosted
        .peer_sends(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    assert!(hosted.unseated(ROOM, BOB).await);
    hosted.peer_sends(remote_get(BOB, own, "s3", ping())).await;
    let left = hosted.answered("s3").await;
    assert_eq!(left.attr("type"), Some("error"));
    assert_eq!(error_condition(&left).as_deref(), Some("not-acceptable"));
}

#[tokio::test]
async fn a_self_ping_the_room_cannot_decide_is_answered_as_undecided() {
    let (hosted, _alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let own = "lounge@muc.example.com/bob";
    // The room registry stops: whether the room seats the occupant cannot
    // be told on this replica.
    let registry = hosted.state.deps.protocol.room_registry.clone();
    registry.kill();
    wait_for(ARRIVES, || (!registry.is_alive()).then_some(()))
        .await
        .expect("the room registry stops");
    hosted
        .peer_sends(remote_get(BOB, own, "s-undecided", ping()))
        .await;
    let answer = hosted.answered("s-undecided").await;
    assert_eq!(answer.attr("type"), Some("error"), "{answer:?}");
    // XEP-0410 §4: the one error a client takes no decision from, so it
    // neither gives the room up nor enters it again on this answer.
    assert_eq!(
        error_condition(&answer).as_deref(),
        Some("remote-server-timeout")
    );
    assert!(hosted.seat(ROOM, BOB).is_some());
}
