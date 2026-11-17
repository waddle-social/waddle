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

use std::time::Duration;

use jid::BareJid;
use minidom::Element;
use s2s_inbound_muc_support::{
    error_condition, error_of, federated_form, local_join, local_receives, local_set,
    not_federating, presence_between, remote_get, remote_join, remote_leave,
    room_with_remote_occupant, statuses, wait_for, Hosted, ARRIVES, BOB, DOMAIN, HOME_B, MUC, ROOM,
};
use waddle_server::s2s::route::RouterLimits;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::muc::room_actor::RoomActor;
use waddle_xmpp::muc::RoomRegistry;
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
    set_query(SetQuery {
        max: Some(max),
        after: after.map(str::to_owned),
        before: None,
        index: None,
    })
}

/// A `disco#items` query carrying the XEP-0059 request `set`.
fn set_query(set: SetQuery) -> Element {
    Element::from(DiscoItemsQuery {
        node: None,
        rsm: Some(set),
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

/// Insert `count` catalog rows `{prefix}-{n}` in one statement, each with
/// whether service discovery lists it (`public_room`), whether it is open
/// to other servers (`federated`) and its `channel_type`, all created
/// before any room [`Hosted::catalogued_room`] inserts, so they come first
/// in the catalog's order.
async fn catalogued_rows(
    hosted: &Hosted,
    prefix: &str,
    count: usize,
    public_room: bool,
    federated: bool,
    channel_type: &str,
) {
    hosted
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(waddle_server::db::actor::DbExecute {
            sql: "INSERT INTO channels (id, name, description, channel_type, position, \
                  is_default, pin_permission, members_only, public_room, federated, \
                  created_at, updated_at) \
                  WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?) \
                  SELECT ? || '-' || printf('%05d', i), ? || '-' || printf('%05d', i), NULL, ?, \
                  0, ?, 'anyone', ?, ?, ?, '2000-01-01T00:00:00+00:00', \
                  '2000-01-01T00:00:00+00:00' FROM n"
                .to_owned(),
            params: vec![
                i64::try_from(count).expect("a row count").into(),
                prefix.into(),
                prefix.into(),
                channel_type.into(),
                false.into(),
                false.into(),
                public_room.into(),
                federated.into(),
            ],
        })
        .await
        .expect("channel catalog rows");
}

/// Hold `room`'s actor until the returned sender is dropped or sent on:
/// every ask of the room waits behind it, as behind a wedged actor.
async fn wedge_room(hosted: &Hosted, room: &str) -> tokio::sync::oneshot::Sender<()> {
    let registry = RoomRegistry::wrap(hosted.state.deps.protocol.room_registry.clone());
    let actor = registry
        .get_room(room.parse().expect("a room JID"))
        .await
        .expect("the room registry answers")
        .expect("the room's actor");
    let (release, wedged) = tokio::sync::oneshot::channel();
    actor
        .tell(Wedge(wedged))
        .await
        .expect("the wedge is queued");
    release
}

/// Holds a room's actor until its sender is dropped or sent on.
struct Wedge(tokio::sync::oneshot::Receiver<()>);

impl kameo::message::Message<Wedge> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        wedge: Wedge,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = wedge.0.await;
    }
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

/// XEP-0059 §2.3 and §2.5 for a remote asker: `before` pages backward,
/// from an anchor or, empty, from the end of the listing, and `index`
/// starts a page at its place in the listing built for the asker.
#[tokio::test]
async fn the_room_list_pages_backward_and_from_an_index_for_a_remote_asker() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    for id in ["alpha", "bravo", "charlie"] {
        hosted.catalogued_room(id, true, true, "text").await;
    }
    hosted
        .peer_sends(remote_get(
            BOB,
            MUC,
            "b1",
            set_query(SetQuery {
                max: Some(2),
                after: None,
                before: Some(String::new()),
                index: None,
            }),
        ))
        .await;
    let last = listing(&hosted.answered("b1").await);
    assert_eq!(jids(&last), [room("bravo"), room("charlie")]);
    let set = last.rsm.expect("the page's <set/>");
    assert_eq!(set.count, Some(3));
    assert_eq!(set.first.and_then(|first| first.index), Some(1));

    hosted
        .peer_sends(remote_get(
            BOB,
            MUC,
            "b2",
            set_query(SetQuery {
                max: Some(1),
                after: None,
                before: Some(room("bravo")),
                index: None,
            }),
        ))
        .await;
    let before = listing(&hosted.answered("b2").await);
    assert_eq!(jids(&before), [room("alpha")]);

    hosted
        .peer_sends(remote_get(
            BOB,
            MUC,
            "b3",
            set_query(SetQuery {
                max: Some(2),
                after: None,
                before: None,
                index: Some(2),
            }),
        ))
        .await;
    let indexed = listing(&hosted.answered("b3").await);
    assert_eq!(jids(&indexed), [room("charlie")]);
    let set = indexed.rsm.expect("the page's <set/>");
    assert_eq!(set.first.and_then(|first| first.index), Some(2));
}

/// An anchor that names a room the asker is not listed (a hidden room, or
/// one not open to other servers) is answered exactly as an anchor at the
/// same place that names no room at all: paging tells nothing of a room
/// that is not listed.
#[tokio::test]
async fn an_anchor_naming_an_unlisted_room_is_answered_as_one_naming_no_room() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    hosted.catalogued_room("alpha", true, true, "text").await;
    hosted.catalogued_room("delta", true, true, "text").await;
    hosted.catalogued_room("charlie", false, true, "text").await;
    hosted
        .catalogued_room("charlie-closed", true, false, "text")
        .await;
    let mut answers = Vec::new();
    for (index, anchor) in ["charlie", "charlie-closed", "charlie-none"]
        .into_iter()
        .enumerate()
    {
        for (direction, set) in [
            (
                "after",
                SetQuery {
                    max: Some(5),
                    after: Some(room(anchor)),
                    before: None,
                    index: None,
                },
            ),
            (
                "before",
                SetQuery {
                    max: Some(5),
                    after: None,
                    before: Some(room(anchor)),
                    index: None,
                },
            ),
        ] {
            let id = format!("anchor-{direction}-{index}");
            hosted
                .peer_sends(remote_get(BOB, MUC, &id, set_query(set)))
                .await;
            answers.push((direction, listing(&hosted.answered(&id).await)));
        }
    }
    for direction in ["after", "before"] {
        let pages: Vec<&DiscoItemsResult> = answers
            .iter()
            .filter(|(asked, _)| *asked == direction)
            .map(|(_, page)| page)
            .collect();
        assert_eq!(pages.len(), 3);
        for page in &pages[1..] {
            assert_eq!(**page, *pages[0], "{direction}: {pages:?}");
        }
    }
    let after = &answers[0].1;
    assert_eq!(jids(after), [room("delta")]);
    let before = &answers[1].1;
    assert_eq!(jids(before), [room("alpha")]);
}

/// A remote asker gets at most 256 rooms in one answer, whatever `max`
/// asks; a listing that does not fit carries its `<set/>` though the
/// request had none (XEP-0059 §1, XEP-0045 §6.3), and the rest is reached
/// by paging from its last item.
#[tokio::test]
async fn a_room_list_beyond_one_answer_carries_its_set_unasked() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let ids: Vec<String> = (0..257).map(|index| format!("room-{index:03}")).collect();
    for id in &ids {
        hosted.catalogued_room(id, true, true, "text").await;
    }
    hosted.peer_sends(remote_get(BOB, MUC, "c1", items())).await;
    let first = listing(&hosted.answered("c1").await);
    assert_eq!(first.items.len(), 256);
    let set = first.rsm.expect("the unasked <set/> of a partial answer");
    assert_eq!(set.count, Some(257));
    let head = set.first.expect("the page's first item");
    assert_eq!((head.index, head.item), (Some(0), room(&ids[0])));
    assert_eq!(set.last, Some(room(&ids[255])));

    hosted
        .peer_sends(remote_get(
            BOB,
            MUC,
            "c2",
            paged(1_000, Some(&room(&ids[255]))),
        ))
        .await;
    let rest = listing(&hosted.answered("c2").await);
    assert_eq!(jids(&rest), [room(&ids[256])]);
}

/// The listing for accounts at other servers reads only the catalog rows it
/// may list: rows of hidden rooms, of rooms closed to other servers and of
/// group conversations take none of its read's places, however many come
/// first in the catalog's order, and a listing that left nothing out tells
/// its count.
#[tokio::test]
async fn rooms_a_remote_asker_may_not_reach_take_no_place_in_its_read() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    // More rows ahead of the open rooms than any listing reads.
    catalogued_rows(&hosted, "hidden", 4_000, false, true, "text").await;
    catalogued_rows(&hosted, "closed", 4_000, true, false, "text").await;
    catalogued_rows(&hosted, "conversation", 4_000, true, true, "group-dm").await;
    hosted.catalogued_room("open-a", true, true, "text").await;
    hosted.catalogued_room("open-b", true, true, "text").await;

    hosted
        .peer_sends(remote_get(BOB, MUC, "r1", paged(10, None)))
        .await;
    let listed = listing(&hosted.answered("r1").await);
    assert_eq!(jids(&listed), [room("open-a"), room("open-b")]);
    let set = listed.rsm.expect("the asked <set/>");
    assert_eq!(set.count, Some(2), "{set:?}");
}

/// A room with a catalog row is listed from its row, never from its
/// actor's configuration: live rooms whose actors say they are public and
/// open to other servers are not listed to a remote asker when their
/// catalog rows say hidden or closed to other servers, though the read for
/// remote askers names neither row. A session is still told of the closed
/// room its catalog row lists, and not of the hidden one.
#[tokio::test]
async fn a_live_room_whose_catalog_row_a_remote_asker_may_not_reach_is_not_listed() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    // Both rooms are live, public and open to other servers by their
    // actors' configuration.
    local_join(&mut alice, ROOM, "alice").await;
    local_join(&mut alice, &room("lobby"), "alice").await;
    // Their catalog rows: `lounge` (ROOM) hidden, `lobby` closed to other
    // servers.
    hosted.catalogued_room("lounge", false, true, "text").await;
    hosted.catalogued_room("lobby", true, false, "text").await;
    hosted.catalogued_room("open", true, true, "text").await;

    hosted
        .peer_sends(remote_get(BOB, MUC, "l1", paged(10, None)))
        .await;
    let listed = listing(&hosted.answered("l1").await);
    assert_eq!(jids(&listed), [room("open")]);
    let set = listed.rsm.expect("the asked <set/>");
    assert_eq!(set.count, Some(1), "{set:?}");

    let local = listing(&local_items(&mut alice, MUC, "l2").await);
    assert_eq!(jids(&local), [room("lobby"), room("open")]);
}

/// A listing for accounts at other servers whose catalog read filled its
/// bound (4,096 rows) may have left rooms out: it is answered with the
/// rooms it read, paged, and tells no count, asked for paging or not.
#[tokio::test]
async fn a_room_list_beyond_its_read_tells_no_count() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    catalogued_rows(&hosted, "open", 4_097, true, true, "text").await;

    hosted.peer_sends(remote_get(BOB, MUC, "b1", items())).await;
    let first = listing(&hosted.answered("b1").await);
    assert_eq!(first.items.len(), 256);
    let set = first.rsm.expect("the <set/> of a partial answer");
    assert_eq!(set.count, None, "{set:?}");
    assert_eq!(set.last, Some(room("open-00255")));

    hosted
        .peer_sends(remote_get(
            BOB,
            MUC,
            "b2",
            paged(10, Some(&room("open-04090"))),
        ))
        .await;
    let last = listing(&hosted.answered("b2").await);
    // Of the 4,097 rows, the 4,096 the read holds are listed.
    assert_eq!(
        jids(&last),
        (4_091..4_096)
            .map(|index| room(&format!("open-{index:05}")))
            .collect::<Vec<_>>()
    );
    let set = last.rsm.expect("the asked <set/>");
    assert_eq!(set.count, None, "{set:?}");
}

/// A live instant room whose actor does not answer within the listing's
/// reading time is left out. An account at another server is answered
/// with the rest and told no count; a session, which takes a listing as
/// the whole of the service's rooms, is answered with
/// `internal-server-error` (type `wait`) instead. Two remote listings asked
/// at once are built from one read of the rooms; once the actor answers
/// again, a listing read after the reuse time names the room again, and
/// a session's listing, read once its request arrived, names it at once.
#[tokio::test]
async fn a_room_not_read_in_time_leaves_a_remote_listing_without_count_and_fails_a_session() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    hosted.catalogued_room("open", true, true, "text").await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    let release = wedge_room(&hosted, ROOM).await;

    for id in ["t1", "t2"] {
        hosted
            .peer_sends(remote_get(BOB, MUC, id, paged(10, None)))
            .await;
    }
    for id in ["t1", "t2"] {
        let listed = listing(&hosted.answered(id).await);
        assert_eq!(jids(&listed), [room("open")], "{id}");
        let set = listed.rsm.expect("the asked <set/>");
        assert_eq!(set.count, None, "{id}: {set:?}");
    }
    assert_eq!(
        metrics.counter_sum("waddle.muc.room_listings.remote_builds", &[]),
        Some(1),
        "two listings asked at once read the rooms once"
    );

    let refused = local_items(&mut alice, MUC, "t3").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("internal-server-error"),
        "{refused:?}"
    );
    assert_eq!(
        error_of(&refused).and_then(|error| error.attr("type").map(str::to_owned)),
        Some("wait".to_owned()),
        "{refused:?}"
    );

    let _ = release.send(());
    let local = listing(&local_items(&mut alice, MUC, "t4").await);
    assert_eq!(jids(&local), [ROOM.to_owned(), room("open")]);

    // The remote listings' read is reused for 2 s after it started.
    tokio::time::sleep(Duration::from_secs(2)).await;
    hosted
        .peer_sends(remote_get(BOB, MUC, "t5", paged(10, None)))
        .await;
    let listed = listing(&hosted.answered("t5").await);
    assert_eq!(jids(&listed), [ROOM.to_owned(), room("open")]);
    let set = listed.rsm.expect("the asked <set/>");
    assert_eq!(set.count, Some(2), "{set:?}");
    assert_eq!(
        metrics.counter_sum("waddle.muc.room_listings.remote_builds", &[]),
        Some(2)
    );
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
