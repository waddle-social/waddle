//! XEP-0425 across S2S, inbound: a moderator at another server retracts
//! another occupant's message in a room of this deployment (ADR-0020 D10,
//! with D8 at the effect boundary).
//!
//! Moderation is room moderation: a remote occupant holds the moderator
//! role only for a trusted home on a `pkix` stream, and the room decides a
//! moderation request on the evidence of the stream that carried it. An
//! occupant without that standing is refused with `forbidden` (§5), and
//! retracts only its own messages (XEP-0424).
//!
//! The deployments are those of `s2s_inbound_muc_support`: `example.com`
//! runs the production room host, S2S router and connection manager over a
//! WebSocket state with an in-memory SQLite database, and `home-b.test` is
//! a second connection manager that keeps what it receives. No PostgreSQL,
//! SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use minidom::Element;
use s2s_inbound_muc_support::{
    body, error_condition, item, local_groupchat, local_receives, local_set, naming,
    presence_between, remote_set, room_with_remote_occupant, stanza_id, ARRIVES, BOB, BOB_OCCUPANT,
    HOME_B, QUIET, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_MODERATE: &str = "urn:xmpp:message-moderate:1";
const NS_RETRACT: &str = "urn:xmpp:message-retract:1";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

/// A request to retract the message the room identified as `target` (§5).
fn moderate(target: &str) -> Element {
    let mut request = naming("moderate", NS_MODERATE, target);
    request.append_child(Element::builder("retract", NS_RETRACT).build());
    request.append_child(
        Element::builder("reason", NS_MODERATE)
            .append("off topic")
            .build(),
    );
    request
}

/// Whether `stanza` is the room's announcement that `target` was retracted
/// by a moderator (§5: a `retract` with a `moderated` child, from the
/// room).
fn moderated(stanza: &Element, target: &str) -> bool {
    stanza.name() == "message"
        && stanza.attr("from") == Some(ROOM)
        && stanza.children().any(|child| {
            child.is("retract", NS_RETRACT)
                && child.attr("id") == Some(target)
                && child
                    .children()
                    .any(|inner| inner.is("moderated", NS_MODERATE))
        })
}

/// The identifier the room gave a message the local occupant sends, as the
/// remote occupant's copy carries it.
async fn local_message(
    hosted: &s2s_inbound_muc_support::Hosted,
    alice: &mut waddle_ws_test_support::WsXmppClient,
    id: &str,
    text: &str,
) -> String {
    local_groupchat(alice, ROOM, id, text).await;
    let copy = hosted
        .at_peer(|stanza| body(stanza).as_deref() == Some(text))
        .await
        .expect("the remote occupant's copy");
    stanza_id(&copy, ROOM).expect("the room's stanza-id")
}

#[tokio::test]
async fn a_moderator_of_a_trusted_home_retracts_another_occupants_message() {
    let (hosted, mut alice, _own) = room_with_remote_occupant(RouterLimits::default(), |spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    let role = Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
                .build(),
        )
        .build();
    let granted = local_set(&mut alice, ROOM, "role-1", role).await;
    assert_eq!(granted.attr("type"), Some("result"), "{granted:?}");
    hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(role, _)| role == "moderator")
        })
        .await
        .expect("the remote occupant is told of its role");
    let target = local_message(&hosted, &mut alice, "spam-1", "buy now").await;

    hosted
        .peer_sends(remote_set(BOB, ROOM, "moderate-1", moderate(&target)))
        .await;
    let done = hosted.answered("moderate-1").await;
    assert_eq!(done.attr("type"), Some("result"), "{done:?}");
    assert_eq!(done.attr("to"), Some(BOB));
    // Every occupant is told, the remote moderator over S2S.
    let told = local_receives(&mut alice, ARRIVES, |stanza| moderated(stanza, &target)).await;
    assert!(told.is_some(), "the local occupant was not told");
    let told = hosted.at_peer(|stanza| moderated(stanza, &target)).await;
    assert!(told.is_some(), "the remote moderator was not told");
}

#[tokio::test]
async fn an_occupant_without_the_moderator_role_moderates_nothing() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let target = local_message(&hosted, &mut alice, "kept-1", "stays here").await;
    hosted
        .peer_sends(remote_set(BOB, ROOM, "moderate-2", moderate(&target)))
        .await;
    let refused = hosted.answered("moderate-2").await;
    assert_eq!(refused.attr("type"), Some("error"), "{refused:?}");
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(
        local_receives(&mut alice, QUIET, |stanza| moderated(stanza, &target))
            .await
            .is_none(),
        "a refused moderation reached the room"
    );
    // A moderation element that asks to retract nothing is answered too.
    hosted
        .peer_sends(remote_set(
            BOB,
            ROOM,
            "moderate-3",
            naming("moderate", NS_MODERATE, &target),
        ))
        .await;
    let malformed = hosted.answered("moderate-3").await;
    assert_eq!(malformed.attr("type"), Some("error"), "{malformed:?}");
    assert_eq!(error_condition(&malformed).as_deref(), Some("bad-request"));
    assert!(hosted.seat(ROOM, BOB).is_some());
}
