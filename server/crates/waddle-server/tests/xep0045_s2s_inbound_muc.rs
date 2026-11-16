//! XEP-0045 across S2S, inbound: occupants at another server in rooms of
//! this deployment (ADR-0020 D10, with D1 and D8 at the effect boundary).
//!
//! A remote occupant joins, is told who is there, is seen by the local
//! occupants, sends to the room and receives its messages, and leaves
//! (§7.2, §7.4, §7.14). Its seat records what its stream proved, is bounded
//! per room, and ends with status 333 when its stream is lost, when its
//! server answers the room with a delivery error (§17.1.2) and when the
//! room's traffic for it cannot be queued (§11.1). A remote principal
//! creates no room, holds an owner or admin standing only for a trusted
//! home on a `pkix` stream, and reaches nothing outside the reviewed
//! operations: private messages and room administration are refused, and
//! only the text of its messages reaches the room.
//!
//! `example.com` runs the production room host, S2S router and connection
//! manager over a WebSocket state with an in-memory SQLite database;
//! `home-b.test` is a second connection manager that keeps what it
//! receives (`s2s_inbound_muc_support`). The rooms of those cases are rooms
//! no host-grant scope manages.
//!
//! The cases named for a managed room run the same wiring over one replica
//! of the revocation fixture's deployment (`s2s_inbound_muc_support/managed.rs`):
//! a remote member enters on its canonical grant and a stranger does not,
//! messages pass the room's acceptance both ways, and a revoked member is
//! removed with status 321, told over S2S and unseated. They need
//! PostgreSQL only and return early without it; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them instead. No SpiceDB or
//! second process is involved.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[allow(dead_code)]
#[path = "s2s_inbound_muc_support/managed.rs"]
mod managed;
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use std::sync::Arc;
use std::time::Duration;

use host_grant_support::OWNER;
use managed::{Managed, GUEST, GUEST_PHONE, STRANGER_PHONE};
use minidom::Element;
use s2s_inbound_muc_support::{
    body, error_condition, full, item, jid, local_groupchat, local_join, local_receives,
    presence_between, remote_get, remote_groupchat, remote_join, remote_leave,
    room_with_remote_occupant, statuses, Hosted, Wiring, ARRIVES, BOB, HOME_B, MUC, QUIET, ROOM,
};
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::permissions::host_grant::{RoomAffiliation, SeatEvidence, StreamEvidence};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::{
    RouterLimits, Seat, SeatInsert, SeatLimits, SeatStore, SqlSeatStore,
};
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_server::test_support::{sweep_host_grant_room, websocket_state_running_s2s};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const BOB_OCCUPANT: &str = "lounge@muc.example.com/bob";
const ALICE_OCCUPANT: &str = "lounge@muc.example.com/alice";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupant_joins_an_open_room_and_both_sides_see_each_other() {
    let (hosted, mut alice, own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // §7.2.2: the room's own presence names the occupant a participant.
    assert_eq!(
        item(&own),
        Some(("participant".to_owned(), "none".to_owned()))
    );
    // §7.2.3: the occupants already there are presented before it.
    let stanzas = hosted.captured.stanzas();
    let position = |from: &str, own: bool| {
        stanzas.iter().position(|stanza| {
            presence_between(stanza, from, BOB)
                && statuses(stanza).contains(&"110".to_owned()) == own
        })
    };
    let (existing, joined) = (
        position(ALICE_OCCUPANT, false),
        position(BOB_OCCUPANT, true),
    );
    assert!(
        existing.is_some() && existing < joined,
        "{existing:?} {joined:?}"
    );
    // The local occupant is told of the remote one.
    let seen = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "presence" && stanza.attr("from") == Some(BOB_OCCUPANT)
    })
    .await
    .expect("the remote occupant's presence for the local occupant");
    assert_eq!(seen.attr("type"), None);
    // The seat records what the stream that carried the join proved.
    let seat = hosted.seat(ROOM, BOB).expect("the remote occupant's seat");
    assert_eq!(seat.nick, "bob");
    assert_eq!(seat.evidence.strength(), AuthenticationStrength::Pkix);
    assert_eq!(
        seat.evidence.trust_profile(),
        Some(TrustProfileKind::PrivateCa)
    );
    let account = jid("bob@home-b.test").to_bare();
    let room = jid(ROOM).to_bare();
    assert_eq!(
        hosted.router.seats().of_account(&room, &account),
        vec![seat.evidence]
    );
}

#[tokio::test]
async fn messages_fan_out_to_and_from_a_remote_occupant() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // A local occupant's message reaches the remote occupant once, from
    // the sender's occupant JID.
    local_groupchat(&mut alice, ROOM, "local-1", "hello from here").await;
    let is_copy = |stanza: &Element| {
        stanza.name() == "message"
            && stanza.attr("from") == Some(ALICE_OCCUPANT)
            && stanza.attr("to") == Some(BOB)
            && body(stanza).as_deref() == Some("hello from here")
    };
    let copy = hosted
        .at_peer(is_copy)
        .await
        .expect("the remote occupant's copy");
    assert_eq!(copy.attr("type"), Some("groupchat"));
    tokio::time::sleep(QUIET).await;
    assert_eq!(hosted.count_at_peer(is_copy), 1, "one stanza for one seat");

    // The remote occupant's message reaches the local occupant and is
    // reflected to its sender (§7.4).
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "remote-1", "hello from there"))
        .await;
    let received = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "message" && body(stanza).as_deref() == Some("hello from there")
    })
    .await
    .expect("the remote occupant's message for the local occupant");
    assert_eq!(received.attr("from"), Some(BOB_OCCUPANT));
    let reflection = hosted
        .at_peer(|stanza| {
            stanza.name() == "message"
                && stanza.attr("from") == Some(BOB_OCCUPANT)
                && stanza.attr("to") == Some(BOB)
        })
        .await
        .expect("the reflection for the remote sender");
    assert_eq!(body(&reflection).as_deref(), Some("hello from there"));
}

#[tokio::test]
async fn only_the_text_of_a_remote_occupants_message_reaches_the_room() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // A payload the room's pipeline has not reviewed for a remote
    // principal rides along with the text.
    let mut carried = remote_groupchat(BOB, ROOM, "carried-1", "text with baggage");
    carried
        .payloads
        .push(Element::builder("pinned", "urn:waddle:pin:0").build());
    carried.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "not a subject change".to_owned(),
    );
    hosted.from_peer(carried).await;
    let received = local_receives(&mut alice, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("text with baggage")
    })
    .await
    .expect("the text for the local occupant");
    assert!(
        received
            .children()
            .all(|child| child.ns() != "urn:waddle:pin:0" && child.name() != "subject"),
        "{received:?}"
    );

    // XEP-0045 §8.1: a subject change is refused.
    let mut subject = Message::new_with_type(MessageType::Groupchat, Some(jid(ROOM)));
    subject.from = Some(jid(BOB));
    subject.id = Some(xmpp_parsers::message::Id("subject-1".to_owned()));
    subject.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "taken over".to_owned(),
    );
    hosted.from_peer(subject).await;
    let refused = hosted.answered("subject-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));

    // A message without text is neither relayed nor answered.
    let mut state = Message::new_with_type(MessageType::Groupchat, Some(jid(ROOM)));
    state.from = Some(jid(BOB));
    state.id = Some(xmpp_parsers::message::Id("state-1".to_owned()));
    state
        .payloads
        .push(Element::builder("composing", "http://jabber.org/protocol/chatstates").build());
    hosted.from_peer(state).await;
    assert!(
        local_receives(&mut alice, QUIET, |stanza| {
            stanza.name() == "message" && stanza.attr("id") == Some("state-1")
        })
        .await
        .is_none(),
        "a message without text reached the room"
    );
    assert!(hosted.captured.with_id("state-1").is_empty());
    assert!(hosted.seat(ROOM, BOB).is_some());
}

#[tokio::test]
async fn a_message_from_a_remote_entity_that_holds_no_seat_is_refused() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    local_join(&mut alice, ROOM, "alice").await;
    hosted
        .from_peer(remote_groupchat(BOB, ROOM, "stranger-1", "let me in"))
        .await;
    let refused = hosted.answered("stranger-1").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("not-acceptable"));
    assert!(
        local_receives(&mut alice, QUIET, |stanza| body(stanza).as_deref()
            == Some("let me in"))
        .await
        .is_none(),
        "a non-occupant's message reached the room"
    );
}

#[tokio::test]
async fn a_remote_occupant_leaves_and_its_seat_ends() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    hosted
        .from_peer(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    let own = hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && stanza.attr("type") == Some("unavailable")
        })
        .await
        .expect("the room's answer to the leave");
    assert!(statuses(&own).contains(&"110".to_owned()), "{own:?}");
    let seen = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "presence"
            && stanza.attr("from") == Some(BOB_OCCUPANT)
            && stanza.attr("type") == Some("unavailable")
    })
    .await
    .expect("the remote occupant's departure for the local occupant");
    assert!(!statuses(&seen).contains(&"333".to_owned()), "{seen:?}");
    assert!(hosted.unseated(ROOM, BOB).await);
    // Nothing of the room leaves for the JID once its seat ended.
    local_groupchat(&mut alice, ROOM, "after-leave", "anyone there").await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        hosted.count_at_peer(|stanza| body(stanza).as_deref() == Some("anyone there")),
        0
    );
}

#[tokio::test]
async fn a_remote_principal_creates_no_room() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let missing = "nowhere@muc.example.com";
    hosted
        .from_peer(remote_join(BOB, missing, "bob", "create-1"))
        .await;
    let refused = hosted.answered("create-1").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("item-not-found"));
    assert!(hosted.unseated(missing, BOB).await);
}

#[tokio::test]
async fn an_owner_or_admin_standing_is_not_issued_to_a_home_that_is_not_trusted() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // ADR-0020 D8 at issuance: `home-b.test` is admitted, not trusted.
    for (id, affiliation) in [("grant-admin", "admin"), ("grant-owner", "owner")] {
        let answer = local_set(
            &mut alice,
            ROOM,
            id,
            affiliation_item(affiliation, "bob@home-b.test"),
        )
        .await;
        assert_eq!(answer.attr("type"), Some("error"), "{answer:?}");
        assert_eq!(error_condition(&answer).as_deref(), Some("not-allowed"));
    }
    // The moderator role is room moderation: refused for the same occupant.
    let answer = local_set(&mut alice, ROOM, "role-1", role_item("moderator", "bob")).await;
    assert_eq!(answer.attr("type"), Some("error"), "{answer:?}");
    assert_eq!(error_condition(&answer).as_deref(), Some("not-allowed"));
    // A membership is no room moderation and is granted.
    let answer = local_set(
        &mut alice,
        ROOM,
        "grant-member",
        affiliation_item("member", "bob@home-b.test"),
    )
    .await;
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    let told = hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(_, affiliation)| affiliation == "member")
        })
        .await;
    assert!(told.is_some(), "the remote occupant was not told");
}

#[tokio::test]
async fn a_trusted_homes_occupant_on_a_pkix_stream_may_be_made_moderator() {
    // `home-b.test` is a trusted home and authenticates by certificate.
    let (hosted, mut alice, _own) = room_with_remote_occupant(RouterLimits::default(), |spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    let answer = local_set(&mut alice, ROOM, "role-2", role_item("moderator", "bob")).await;
    assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    // The occupant learns its new role over S2S.
    let changed = hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(role, _)| role == "moderator")
        })
        .await;
    assert!(
        changed.is_some(),
        "the remote occupant was not told of its role"
    );
}

#[tokio::test]
async fn private_messages_and_room_administration_are_refused_to_remote_occupants() {
    let (hosted, _alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    let mut private = Message::new_with_type(MessageType::Chat, Some(jid(ALICE_OCCUPANT)));
    private.from = Some(jid(BOB));
    private.id = Some(xmpp_parsers::message::Id("pm-1".to_owned()));
    hosted.from_peer(private).await;
    let refused = hosted.answered("pm-1").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("feature-not-implemented")
    );
    let list = remote_get(
        BOB,
        ROOM,
        "admin-1",
        Element::builder("query", NS_MUC_ADMIN).build(),
    );
    hosted.from_peer(list).await;
    let refused = hosted.answered("admin-1").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("service-unavailable")
    );
    assert!(hosted.seat(ROOM, BOB).is_some(), "a refusal ended the seat");
}

#[tokio::test]
async fn remote_occupants_of_one_room_are_bounded() {
    let limits = RouterLimits {
        seats_per_room: 1,
        ..RouterLimits::default()
    };
    let (hosted, _alice, _own) = room_with_remote_occupant(limits, no_change).await;
    let carol = "carol@home-b.test/laptop";
    hosted
        .from_peer(remote_join(carol, ROOM, "carol", "join-2"))
        .await;
    let refused = hosted.answered("join-2").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    assert!(hosted.seat(ROOM, carol).is_none());
    assert!(hosted.seat(ROOM, BOB).is_some());
}

/// The departure of the remote occupant as the local one sees it.
async fn removed_with_333(alice: &mut waddle_ws_test_support::WsXmppClient) -> Element {
    let seen = local_receives(alice, ARRIVES, |stanza| {
        stanza.name() == "presence"
            && stanza.attr("from") == Some(BOB_OCCUPANT)
            && stanza.attr("type") == Some("unavailable")
    })
    .await
    .expect("the remote occupant's removal for the local occupant");
    let codes = statuses(&seen);
    assert!(codes.contains(&"333".to_owned()), "{codes:?}");
    assert!(!codes.contains(&"307".to_owned()), "{codes:?}");
    seen
}

#[tokio::test]
async fn a_delivery_error_from_the_occupants_server_removes_it_with_status_333() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // XEP-0045 §17.1.2: the occupant's server answers the room's traffic
    // with a delivery-related error.
    let mut bounce = Presence::new(PresenceType::Error);
    bounce.from = Some(jid(BOB));
    bounce.to = Some(jid(BOB_OCCUPANT));
    bounce.payloads.push(Element::from(StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::RecipientUnavailable,
        "en",
        "Gone.",
    )));
    hosted.from_peer(bounce).await;
    removed_with_333(&mut alice).await;
    assert!(hosted.unseated(ROOM, BOB).await);
}

#[tokio::test]
async fn a_lost_stream_removes_the_occupants_of_its_domain_with_status_333() {
    let (mut hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // The peer shuts down: its streams end with a stream error, so stanzas
    // either way may be lost (XEP-0045 §11.1).
    hosted.stop_peer().await;
    removed_with_333(&mut alice).await;
    assert!(hosted.unseated(ROOM, BOB).await);
}

#[tokio::test]
async fn a_rooms_stanza_leaves_only_for_a_seat_and_one_that_finds_no_place_ends_it() {
    let limits = RouterLimits {
        seat_traffic_per_peer: 1,
        ..RouterLimits::default()
    };
    let (hosted, mut alice, _own) = room_with_remote_occupant(limits, no_change).await;
    let from_room = |to: &str| {
        let mut presence = Presence::new(PresenceType::None);
        presence.from = Some(jid(ALICE_OCCUPANT));
        presence.to = Some(jid(to));
        Stanza::Presence(presence)
    };
    // A JID that holds no seat is not a recipient, whatever names it.
    let stranger = full("mallory@home-b.test/away");
    assert!(!hosted
        .router
        .seats_recipient(&stranger, &from_room(stranger.as_str())));
    assert!(hosted
        .router
        .offer_to_seat(&stranger, from_room(stranger.as_str()))
        .is_err());
    // Nor is a private message a room's traffic to a seat.
    let bob = full(BOB);
    let mut private = Message::new_with_type(MessageType::Chat, Some(jid(BOB)));
    private.from = Some(jid(ALICE_OCCUPANT));
    assert!(hosted
        .router
        .offer_to_seat(&bob, Stanza::Message(private))
        .is_err());
    // With one place for the remote domain, a burst cannot be queued: the
    // seat ends rather than leaving the occupant seated behind a gap.
    for _ in 0..8 {
        let _ = hosted.router.offer_to_seat(&bob, from_room(BOB));
    }
    removed_with_333(&mut alice).await;
    assert!(hosted.unseated(ROOM, BOB).await);
}

#[tokio::test]
async fn a_domain_this_replica_no_longer_admits_is_refused_where_the_room_decides() {
    // The connection manager admitted the stream under one snapshot; the
    // replica that hosts the rooms runs one that dropped the domain. The
    // decision is taken where the effect happens (ADR-0020 D1), not from
    // the stream's own admission.
    let wiring = Wiring::new(no_change).await;
    let delisted = wiring.without_peer().await;
    let state = websocket_state_running_s2s(&delisted).await;
    let hosted = Hosted::serve(wiring, state, RouterLimits::default()).await;
    hosted
        .from_peer(remote_join(BOB, ROOM, "bob", "join-1"))
        .await;
    let refused = hosted.answered("join-1").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(hosted.unseated(ROOM, BOB).await);
    let ping = Element::builder("ping", "urn:xmpp:ping").build();
    hosted.from_peer(remote_get(BOB, MUC, "ping-1", ping)).await;
    let refused = hosted.answered("ping-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
}

/// An IQ `set` from a local client to `to`, and its answer.
async fn local_set(
    client: &mut waddle_ws_test_support::WsXmppClient,
    to: &str,
    id: &str,
    payload: Element,
) -> Element {
    let request = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(payload)
        .build();
    client.send(&String::from(&request)).await.expect("send");
    local_receives(client, ARRIVES, |stanza| {
        stanza.name() == "iq" && stanza.attr("id") == Some(id)
    })
    .await
    .expect("the answer to the request")
}

fn role_item(role: &str, nick: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), role)
                .attr(minidom::rxml::xml_ncname!("nick").to_owned(), nick)
                .build(),
        )
        .build()
}

fn affiliation_item(affiliation: &str, account: &str) -> Element {
    Element::builder("query", NS_MUC_ADMIN)
        .append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    affiliation,
                )
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                .build(),
        )
        .build()
}

#[tokio::test]
async fn a_trusted_homes_account_made_admin_enters_with_that_standing_over_pkix() {
    let (hosted, mut alice, _own) = room_with_remote_occupant(RouterLimits::default(), |spec| {
        spec.trusted_homes = vec![HOME_B];
    })
    .await;
    // ADR-0020 D8 at issuance: a trusted home's account may be made admin.
    let granted = local_set(
        &mut alice,
        ROOM,
        "grant-2",
        affiliation_item("admin", "bob@home-b.test"),
    )
    .await;
    assert_eq!(granted.attr("type"), Some("result"), "{granted:?}");
    let told = hosted
        .at_peer(|stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && item(stanza).is_some_and(|(_, affiliation)| affiliation == "admin")
        })
        .await;
    assert!(told.is_some(), "the remote occupant was not told");
    // And where the seat is taken: it enters again with that standing,
    // because a `pkix` stream of its trusted home carries the join.
    hosted
        .from_peer(remote_leave(BOB, ROOM, "bob", "leave-1"))
        .await;
    assert!(hosted.unseated(ROOM, BOB).await);
    let before = hosted.received();
    hosted
        .from_peer(remote_join(BOB, ROOM, "bob", "join-2"))
        .await;
    // The room's own presence carries no id: it is the first one for the
    // occupant, after the join was sent, that is available and its own.
    let own = hosted
        .at_peer_after(before, |stanza| {
            presence_between(stanza, BOB_OCCUPANT, BOB)
                && stanza.attr("type").is_none()
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the occupant that entered again");
    assert_eq!(
        item(&own),
        Some(("moderator".to_owned(), "admin".to_owned()))
    );
    assert!(hosted.seat(ROOM, BOB).is_some());
}

/// The owner joined to the managed room, and the remote member's own
/// presence after its join.
async fn managed_room_with_remote_member(
    managed: &Managed,
) -> (waddle_ws_test_support::WsXmppClient, Element) {
    let room = managed.room();
    let mut owner = managed.deployment.replicas[0].client("owner", "desk").await;
    let joined = local_join(&mut owner, room.as_str(), "owner").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    managed
        .hosted
        .from_peer(remote_join(GUEST_PHONE, room.as_str(), "guest", "join-1"))
        .await;
    let occupant = managed.occupant("guest");
    let own = managed
        .hosted
        .at_peer(|stanza| {
            presence_between(stanza, &occupant, GUEST_PHONE)
                && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the remote member");
    (owner, own)
}

#[tokio::test]
async fn a_remote_member_enters_a_managed_room_on_its_grant_and_a_stranger_does_not() {
    let Some(managed) = Managed::start("s2s_in_member", no_change).await else {
        return;
    };
    let room = managed.room();
    let (mut owner, own) = managed_room_with_remote_member(&managed).await;
    assert_eq!(
        item(&own),
        Some(("participant".to_owned(), "member".to_owned()))
    );
    let seat = managed
        .hosted
        .seat(room.as_str(), GUEST_PHONE)
        .expect("the remote member's seat");
    assert_eq!(seat.evidence.strength(), AuthenticationStrength::Pkix);

    // An account of the same admitted home that the grants do not name is
    // refused as any account without a membership is (XEP-0045 §7.2.6).
    managed
        .hosted
        .from_peer(remote_join(
            STRANGER_PHONE,
            room.as_str(),
            "mallory",
            "join-2",
        ))
        .await;
    let refused = managed.hosted.answered("join-2").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("registration-required")
    );
    assert!(managed.hosted.seat(room.as_str(), STRANGER_PHONE).is_none());

    // Messages pass the managed room's acceptance both ways: the remote
    // sender is decided on its seat's stream evidence, and the remote
    // recipient is in the audience the acceptance freezes.
    managed
        .hosted
        .from_peer(remote_groupchat(
            GUEST_PHONE,
            room.as_str(),
            "remote-1",
            "from the guest",
        ))
        .await;
    let received = local_receives(&mut owner, ARRIVES, |stanza| {
        body(stanza).as_deref() == Some("from the guest")
    })
    .await
    .expect("the remote member's message for the owner");
    assert_eq!(
        received.attr("from"),
        Some(managed.occupant("guest").as_str())
    );
    local_groupchat(&mut owner, room.as_str(), "local-1", "from the owner").await;
    let from_owner = managed.occupant("owner");
    let is_copy = |stanza: &Element| {
        stanza.name() == "message"
            && stanza.attr("from") == Some(from_owner.as_str())
            && stanza.attr("to") == Some(GUEST_PHONE)
            && body(stanza).as_deref() == Some("from the owner")
    };
    assert!(managed.hosted.at_peer(is_copy).await.is_some());
    tokio::time::sleep(QUIET).await;
    assert_eq!(managed.hosted.count_at_peer(is_copy), 1);
    // A stranger's message is not an occupant's.
    managed
        .hosted
        .from_peer(remote_groupchat(
            STRANGER_PHONE,
            room.as_str(),
            "stranger-1",
            "from a stranger",
        ))
        .await;
    let refused = managed.hosted.answered("stranger-1").await;
    assert_eq!(error_condition(&refused).as_deref(), Some("not-acceptable"));
    managed.deployment.close().await;
}

#[tokio::test]
async fn a_revoked_remote_member_is_removed_with_status_321_and_told_over_s2s() {
    let Some(managed) = Managed::start("s2s_in_revoke", no_change).await else {
        return;
    };
    let room = managed.room();
    let (mut owner, _own) = managed_room_with_remote_member(&managed).await;
    // The remote member's arrival, so that the next presence the owner
    // reads from its occupant JID is its removal.
    let guest = managed.occupant("guest");
    local_receives(&mut owner, ARRIVES, |stanza| {
        stanza.name() == "presence" && stanza.attr("from") == Some(guest.as_str())
    })
    .await
    .expect("the remote member's arrival for the owner");

    // The owner's revoke is acknowledged by the revocation fence, and the
    // replica that hosts the room sweeps it: the seated remote occupant is
    // decided on the evidence its seat was admitted under.
    let grants = &managed.deployment.grants;
    let receipt = grants
        .set(OWNER, GUEST, RoomAffiliation::None)
        .await
        .expect("the revoke");
    let acknowledged = grants
        .store
        .await_revocation_fence(
            grants.scope.lifecycle,
            receipt.record().generation,
            Duration::from_secs(10),
        )
        .await
        .expect("fence reading");
    assert!(acknowledged.acknowledged(), "{acknowledged:?}");
    assert!(sweep_host_grant_room(&managed.hosted.state, &room).await);

    // XEP-0045 §9.4: those who stay and the removed occupant itself are
    // told with status 321; the occupant's copy leaves over S2S and ends
    // its seat.
    let seen = local_receives(&mut owner, ARRIVES, |stanza| {
        stanza.name() == "presence"
            && stanza.attr("from") == Some(guest.as_str())
            && stanza.attr("type") == Some("unavailable")
    })
    .await
    .expect("the removal for the owner");
    assert!(statuses(&seen).contains(&"321".to_owned()), "{seen:?}");
    let told = managed
        .hosted
        .at_peer(|stanza| {
            presence_between(stanza, &guest, GUEST_PHONE)
                && stanza.attr("type") == Some("unavailable")
        })
        .await
        .expect("the removal for the removed occupant");
    let codes = statuses(&told);
    assert!(codes.contains(&"110".to_owned()), "{codes:?}");
    assert!(codes.contains(&"321".to_owned()), "{codes:?}");
    assert!(managed.hosted.unseated(room.as_str(), GUEST_PHONE).await);

    // It does not enter again, and nothing of the room reaches it.
    managed
        .hosted
        .from_peer(remote_join(GUEST_PHONE, room.as_str(), "guest", "join-3"))
        .await;
    let refused = managed.hosted.answered("join-3").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("registration-required")
    );
    local_groupchat(&mut owner, room.as_str(), "after", "members only").await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        managed
            .hosted
            .count_at_peer(|stanza| body(stanza).as_deref() == Some("members only")),
        0
    );
    managed.deployment.close().await;
}

async fn seat_database(name: &str) -> Database {
    let database = Database::in_memory(name)
        .await
        .expect("an in-memory database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("the deployment migrations");
    database
}

fn seat_of(owner: &str, occupant: &str, updated_at_ms: i64) -> Seat {
    Seat {
        room: jid(ROOM).to_bare(),
        occupant: full(occupant),
        nick: "bob".to_owned(),
        session: OccupancySessionGeneration::mint(),
        evidence: StreamEvidence::for_test(AuthenticationStrength::Dialback, None),
        snapshot: uuid::Uuid::new_v4(),
        owner: owner.to_owned(),
        lost_at_ms: None,
        updated_at_ms,
    }
}

#[tokio::test]
async fn seats_are_bounded_and_a_stopped_replicas_seats_are_taken_over_or_removed() {
    let store = Arc::new(SqlSeatStore::new(seat_database("inbound-muc-seats").await));
    let limits = SeatLimits {
        per_room: 8,
        per_peer: 8,
        per_account: 1,
        total: 8,
    };
    let seat = seat_of("replica-a", BOB, 1_000);
    assert_eq!(
        store.insert(&seat, &limits).await.expect("insert"),
        SeatInsert::Inserted
    );
    assert_eq!(
        store.insert(&seat, &limits).await.expect("insert again"),
        SeatInsert::Exists
    );
    // One account, however many resources, within its bound.
    let second = seat_of("replica-a", "bob@home-b.test/laptop", 1_000);
    assert_eq!(
        store
            .insert(&second, &limits)
            .await
            .expect("second resource"),
        SeatInsert::Bounds
    );
    // The row keeps what the stream proved, and nothing else decides it.
    let stored = store
        .get(&seat.room, &seat.occupant)
        .await
        .expect("read")
        .expect("the seat");
    assert_eq!(stored.evidence, seat.evidence);
    assert_eq!(stored.session, seat.session);

    // Another replica that loses a stream of the pair marks the seat for
    // its owner.
    assert_eq!(
        store
            .mark_lost(MUC, HOME_B, "replica-b", 2_000)
            .await
            .expect("mark"),
        1
    );
    assert_eq!(store.lost("replica-a", 8).await.expect("lost").len(), 1);
    assert!(store.lost("replica-b", 8).await.expect("lost").is_empty());

    // No replica records that it is alive in this database, so the seat's
    // owner counts as stopped: the seat is orphaned, and a join that
    // reaches another replica takes it over.
    assert_eq!(store.orphaned(5_000, 8).await.expect("orphaned").len(), 1);
    let adopted = store
        .adopt(&seat.room, &seat.occupant, "replica-b", 5_000, 6_000)
        .await
        .expect("adopt")
        .expect("the adopted seat");
    assert_eq!(adopted.owner, "replica-b");
    assert_eq!(adopted.lost_at_ms, None);
    assert_eq!(adopted.evidence, seat.evidence);
    // The owner it was taken from no longer removes it.
    assert!(!store.remove(&seat).await.expect("stale removal"));
    assert_eq!(store.remove_owned("replica-b").await.expect("owned"), 1);
    assert_eq!(store.count().await.expect("count"), 0);
}
