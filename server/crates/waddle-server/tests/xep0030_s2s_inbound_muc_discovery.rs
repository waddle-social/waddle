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
//! to its own sessions. `disco#items` lists neither rooms nor occupants.
//! Ping is answered for the service and a room; a self-ping (XEP-0410) is
//! answered from the occupant's seat: a result while joined,
//! `not-acceptable` otherwise.
//!
//! The deployments are those of `s2s_inbound_muc_support`: no PostgreSQL,
//! SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use minidom::Element;
use s2s_inbound_muc_support::{
    error_condition, local_join, remote_get, remote_leave, room_with_remote_occupant, Hosted, BOB,
    HOME_B, MUC, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
const NS_PING: &str = "urn:xmpp:ping";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

fn info() -> Element {
    Element::builder("query", DISCO_INFO).build()
}

fn ping() -> Element {
    Element::builder("ping", NS_PING).build()
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
        hosted.from_peer(remote_get(from, MUC, id, info())).await;
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
        .from_peer(remote_get(HOME_B, ROOM, "r1", info()))
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
        .from_peer(remote_get(HOME_B, "nowhere@muc.example.com", "r2", info()))
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
    hosted.from_peer(remote_get(BOB, ROOM, "r3", node)).await;
    let refused = hosted.answered("r3").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("item-not-found"));
}

#[tokio::test]
async fn disco_items_lists_neither_rooms_nor_occupants_to_remote_askers() {
    let (hosted, _alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    for (to, id) in [(MUC, "i1"), (ROOM, "i2")] {
        let items = Element::builder("query", DISCO_ITEMS).build();
        hosted.from_peer(remote_get(BOB, to, id, items)).await;
        let answer = hosted.answered(id).await;
        assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
        let listed = answer
            .children()
            .filter(|child| child.is("query", DISCO_ITEMS))
            .flat_map(Element::children)
            .count();
        assert_eq!(listed, 0, "{answer:?}");
    }
}

#[tokio::test]
async fn ping_is_answered_for_the_service_and_a_room() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    for (to, id) in [(MUC, "p1"), (ROOM, "p2")] {
        hosted.from_peer(remote_get(BOB, to, id, ping())).await;
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
    hosted.from_peer(remote_get(BOB, own, "s1", ping())).await;
    let joined = hosted.answered("s1").await;
    assert_eq!(joined.attr("type"), Some("result"), "{joined:?}");
    // A ping for another occupant would be relayed to an account.
    hosted
        .from_peer(remote_get(
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
        .from_peer(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    assert!(hosted.unseated(ROOM, BOB).await);
    hosted.from_peer(remote_get(BOB, own, "s3", ping())).await;
    let left = hosted.answered("s3").await;
    assert_eq!(left.attr("type"), Some("error"));
    assert_eq!(error_condition(&left).as_deref(), Some("not-acceptable"));
}
