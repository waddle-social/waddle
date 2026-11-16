//! XEP-0045 across S2S, outbound: a local resource joins, uses and leaves a
//! room on a remote domain, and the room's traffic reaches it (ADR-0020
//! D7's room exception). Traffic that is not for or from a room the
//! resource joined stays refused by D7 in both directions; a failed stream
//! to the room's domain removes the occupant with status 333 (XEP-0045
//! §11.1); a resource that ends leaves its rooms (§7.14).
//!
//! `home-a.test` runs the S2S router with a recording delivery in place of
//! WebSocket clients; `home-b.test` serves `muc.home-b.test` with a
//! hand-written room service. No PostgreSQL, SpiceDB or second process is
//! involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_remote_muc_support/mod.rs"]
mod support;

use std::time::Duration;

use minidom::Element;
use support::{
    delivered_messages, delivered_presence, error_condition, full, jid, join, join_presence,
    message, offer, outbound_muc, presence, statuses, wait_for, ALICE_LAPTOP, ALICE_PHONE, ARRIVES,
    BOB, BOB_PHONE, BUSY, QUIET, ROOM, STREAM_LOSS,
};
use waddle_server::destination::{IqOffer, RemoteRoute, StanzaOffer, UnroutableReason};
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::mam::MAM_NS;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::presence::Type as PresenceType;

const ROOM_ALICE: &str = "room@muc.home-b.test/alice";
const ROOM_HOST: &str = "room@muc.home-b.test/host";

/// The reason and reply of a refused offer.
fn refused(offered: StanzaOffer) -> (UnroutableReason, Element) {
    match offered {
        StanzaOffer::Refused {
            reason,
            reply: Some(reply),
        } => (reason, element(*reply)),
        other => panic!("expected a refusal with a reply, got {other:?}"),
    }
}

/// `stanza` as an element.
fn element(stanza: Stanza) -> Element {
    match stanza {
        Stanza::Iq(iq) => Element::from(*iq),
        Stanza::Message(message) => Element::from(message),
        Stanza::Presence(presence) => Element::from(presence),
    }
}

/// D7's outbound refusal: `service-unavailable` from the addressed JID.
fn assert_d7_refusal(offered: StanzaOffer, from: &str) {
    let (reason, reply) = refused(offered);
    assert_eq!(reason, UnroutableReason::RefusedByPolicy);
    assert_eq!(reply.attr("type"), Some("error"));
    assert_eq!(reply.attr("from"), Some(from));
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_local_resource_joins_a_remote_room_and_receives_its_presence() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // The join left with the muc element, from the bound full JID.
    let joins = muc.received_with_id("presence", "join");
    assert_eq!(joins.len(), 1);
    assert_eq!(joins[0].attr("from"), Some(ALICE_PHONE));
    assert_eq!(joins[0].attr("to"), Some(ROOM_ALICE));
    assert!(joins[0]
        .children()
        .any(|child| child.is("x", "http://jabber.org/protocol/muc")));
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alice"));

    // Other occupants' presence arrives too, and only for this resource.
    let host = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_HOST)
            .into_iter()
            .next()
    })
    .await;
    assert!(host.is_some(), "the host's presence reaches the occupant");
    assert!(delivered_presence(&a, ALICE_LAPTOP, ROOM_HOST).is_empty());
    assert_eq!(a.router.remote_rooms(), 1);
}

#[tokio::test]
async fn groupchat_messages_reach_the_room_and_its_reflection_reaches_the_occupant() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "g1",
            "hello room",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let sent = wait_for(ARRIVES, || {
        muc.received_with_id("message", "g1").into_iter().next()
    })
    .await
    .expect("the groupchat message reaches the room");
    assert_eq!(sent.attr("from"), Some(ALICE_PHONE));
    assert_eq!(sent.attr("type"), Some("groupchat"));

    let reflected = wait_for(ARRIVES, || {
        delivered_messages(&a, ALICE_PHONE, "g1").into_iter().next()
    })
    .await
    .expect("the room's reflection reaches the occupant");
    assert_eq!(reflected.from, Some(jid(ROOM_ALICE)));
    assert_eq!(reflected.type_, MessageType::Groupchat);
}

#[tokio::test]
async fn subject_and_private_messages_from_the_room_reach_the_occupant_only() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // XEP-0045 §7.2.15: the subject, from the room's bare JID.
    let mut subject = message(ROOM, ALICE_PHONE, MessageType::Groupchat, "s1", "");
    subject.bodies.clear();
    subject.subjects.insert(
        xmpp_parsers::message::Lang(String::new()),
        "Today".to_owned(),
    );
    muc.send(subject);
    // XEP-0045 §7.5: a private message from another occupant.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "pm1",
        "psst",
    ));
    // The same to a resource of the account that never joined.
    muc.send(message(
        ROOM_HOST,
        ALICE_LAPTOP,
        MessageType::Chat,
        "pm2",
        "psst",
    ));

    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "s1")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the subject reaches the occupant"
    );
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "pm1")
            .into_iter()
            .next())
        .await
        .is_some(),
        "the private message reaches the occupant"
    );
    // The resource that never joined is refused as D7 refuses any account
    // traffic, and nothing reaches it.
    let refusal = wait_for(ARRIVES, || {
        muc.received_with_id("message", "pm2").into_iter().next()
    })
    .await
    .expect("the room is told");
    assert_eq!(refusal.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(delivered_messages(&a, ALICE_LAPTOP, "pm2").is_empty());
}

#[tokio::test]
async fn leaving_sends_unavailable_and_ends_the_occupancy_when_the_room_answers() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            ROOM_ALICE,
            PresenceType::Unavailable,
            "leave",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let own = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Unavailable)
    })
    .await
    .expect("the room's answer to the leave reaches the occupant");
    assert!(statuses(&own).contains(&"110".to_owned()));
    assert!(
        wait_for(ARRIVES, || (a.router.remote_rooms() == 0).then_some(()))
            .await
            .is_some(),
        "the occupancy ends"
    );
    assert!(muc.nick_of(ROOM, ALICE_PHONE).is_none());

    // The room is no longer one the resource occupies.
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            ROOM,
            MessageType::Groupchat,
            "after",
            "anyone?",
        )),
    )
    .await;
    assert_d7_refusal(offered, ROOM);
}

#[tokio::test]
async fn a_nickname_change_keeps_the_occupancy_under_the_new_nickname() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // XEP-0045 §7.6: presence without the muc element to the new occupant
    // JID.
    let offered = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            "room@muc.home-b.test/alicia",
            PresenceType::None,
            "nick",
        )),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let changed = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| statuses(presence).contains(&"303".to_owned()))
    })
    .await;
    assert!(changed.is_some(), "the old nickname's 303 arrives");
    assert!(
        wait_for(ARRIVES, || delivered_presence(
            &a,
            ALICE_PHONE,
            "room@muc.home-b.test/alicia"
        )
        .into_iter()
        .next())
        .await
        .is_some(),
        "the new nickname's own presence arrives"
    );
    assert_eq!(muc.nick_of(ROOM, ALICE_PHONE).as_deref(), Some("alicia"));
    // Still an occupant: room traffic keeps flowing.
    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Chat,
        "pm",
        "hi",
    ));
    assert!(
        wait_for(ARRIVES, || delivered_messages(&a, ALICE_PHONE, "pm")
            .into_iter()
            .next())
        .await
        .is_some()
    );
    assert_eq!(a.router.remote_rooms(), 1);
}

#[tokio::test]
async fn a_join_the_room_refuses_is_delivered_and_forgotten() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    let occupant = "busy@muc.home-b.test/alice";
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, occupant, "busy-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    let error = wait_for(ARRIVES, || {
        delivered_presence(&a, ALICE_PHONE, occupant)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Error)
    })
    .await
    .expect("the room's refusal reaches the resource");
    assert_eq!(
        error_condition(&Element::from(error)).as_deref(),
        Some("conflict")
    );
    assert!(
        wait_for(ARRIVES, || (a.router.remote_rooms() == 0).then_some(()))
            .await
            .is_some(),
        "the refused join is forgotten"
    );
    let offered = offer(
        &a,
        Stanza::Message(message(
            ALICE_PHONE,
            BUSY,
            MessageType::Groupchat,
            "busy-message",
            "hi",
        )),
    )
    .await;
    assert_d7_refusal(offered, BUSY);
}

#[tokio::test]
async fn traffic_from_a_room_the_resource_never_joined_is_refused() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;

    muc.send(message(
        ROOM_HOST,
        ALICE_PHONE,
        MessageType::Groupchat,
        "unjoined",
        "hello",
    ));
    let mut stray = presence(ROOM_HOST, ALICE_PHONE, PresenceType::None, "stray");
    stray.payloads.push(Element::from(
        xmpp_parsers::muc::user::MucUser::new().with_items(vec![
            xmpp_parsers::muc::user::Item::new(
                xmpp_parsers::muc::user::Affiliation::Owner,
                xmpp_parsers::muc::user::Role::Moderator,
            ),
        ]),
    ));
    muc.send(stray);
    let refusal = wait_for(ARRIVES, || {
        muc.received_with_id("message", "unjoined")
            .into_iter()
            .next()
    })
    .await
    .expect("the message is refused");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    tokio::time::sleep(QUIET).await;
    // Presence is ignored without an answer (RFC 6121 §8.5.1).
    assert!(muc.received_with_id("presence", "stray").is_empty());
    assert!(a.delivery.delivered().is_empty(), "nothing reached alice");
    assert_eq!(a.router.remote_rooms(), 0);
}

#[tokio::test]
async fn account_traffic_to_and_from_remote_jids_stays_refused() {
    let pki = support::Pki::new();
    let (a, b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // Outbound: a direct message, directed presence and a subscription
    // request to an account, and presence or messages for a room the
    // resource never joined.
    let refusals = [
        (
            Stanza::Message(message(ALICE_PHONE, BOB, MessageType::Chat, "d1", "hi")),
            BOB,
        ),
        (
            Stanza::Presence(presence(ALICE_PHONE, BOB_PHONE, PresenceType::None, "d2")),
            BOB_PHONE,
        ),
        (
            Stanza::Presence(presence(ALICE_PHONE, BOB, PresenceType::Subscribe, "d3")),
            BOB,
        ),
        (
            Stanza::Presence(presence(
                ALICE_PHONE,
                "other@muc.home-b.test/alice",
                PresenceType::None,
                "d4",
            )),
            "other@muc.home-b.test/alice",
        ),
        (
            Stanza::Message(message(
                ALICE_PHONE,
                "other@muc.home-b.test",
                MessageType::Groupchat,
                "d5",
                "hi",
            )),
            "other@muc.home-b.test",
        ),
        // A subscription to the joined room's own JID is account traffic.
        (
            Stanza::Presence(presence(ALICE_PHONE, ROOM, PresenceType::Subscribe, "d6")),
            ROOM,
        ),
        // A resource of the account that did not join the room.
        (
            Stanza::Message(message(
                ALICE_LAPTOP,
                ROOM,
                MessageType::Groupchat,
                "d7",
                "hi",
            )),
            ROOM,
        ),
    ];
    for (stanza, from) in refusals {
        assert_d7_refusal(offer(&a, stanza).await, from);
    }

    // Inbound: an account at the room's home domain is still refused.
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "d8", "hi"),
    )
    .await;
    tokio::time::sleep(QUIET).await;
    assert!(delivered_messages(&a, ALICE_PHONE, "d8").is_empty());
    assert!(muc.received_with_id("message", "d1").is_empty());
}

#[tokio::test]
async fn a_joined_jid_that_never_answers_as_a_room_gets_no_messages_through() {
    let pki = support::Pki::new();
    let (a, b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    // A "join" of an account's full JID is routed: nothing tells the
    // router what a remote JID is before it answers.
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(ALICE_PHONE, BOB_PHONE, "fake-join")),
    )
    .await;
    assert!(matches!(offered, StanzaOffer::Accepted), "{offered:?}");
    // The account's messages and plain presence are still refused: only
    // room presence shows a room.
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        message(BOB_PHONE, ALICE_PHONE, MessageType::Chat, "chat", "hi"),
    )
    .await;
    support::routing::send(
        &b,
        support::HOME_B,
        support::HOME_A,
        presence(BOB_PHONE, ALICE_PHONE, PresenceType::None, "plain"),
    )
    .await;
    tokio::time::sleep(QUIET).await;
    assert!(a.delivery.delivered().is_empty(), "nothing reached alice");
}

#[tokio::test]
async fn joins_beyond_the_bound_per_resource_are_refused_with_resource_constraint() {
    let pki = support::Pki::new();
    let limits = RouterLimits {
        rooms_per_resource: 1,
        ..RouterLimits::default()
    };
    let (a, _b, _muc) = outbound_muc(&pki, limits).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    let offered = offer(
        &a,
        Stanza::Presence(join_presence(
            ALICE_PHONE,
            "second@muc.home-b.test/alice",
            "second",
        )),
    )
    .await;
    let (reason, reply) = refused(offered);
    assert_eq!(reason, UnroutableReason::EgressBusy);
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("resource-constraint")
    );
    // Another resource has its own share.
    join(&a, ALICE_LAPTOP, "second@muc.home-b.test/alice2").await;
    assert_eq!(a.router.remote_rooms(), 2);
}

#[tokio::test]
async fn a_mam_query_to_a_joined_room_is_answered_with_its_results() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let query = Iq::Set {
        from: Some(jid(ALICE_PHONE)),
        to: Some(jid(ROOM)),
        id: "mam1".into(),
        payload: Element::builder("query", MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
            .build(),
    };
    let offered = a.offer_iq(ROOM, query).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    // XEP-0313: the result arrives as a message from the room.
    let result = wait_for(ARRIVES, || {
        a.delivery
            .delivered()
            .into_iter()
            .find_map(|(to, stanza)| match stanza {
                Stanza::Message(message)
                    if to == full(ALICE_PHONE)
                        && message
                            .payloads
                            .iter()
                            .any(|payload| payload.is("result", MAM_NS)) =>
                {
                    Some(message)
                }
                _ => None,
            })
    })
    .await;
    assert!(result.is_some(), "the archived message arrives");
    let fin = wait_for(ARRIVES, || a.delivery.iqs("mam1").into_iter().next())
        .await
        .expect("the query is answered");
    assert_eq!(fin.0, full(ALICE_PHONE));
    assert!(matches!(fin.1, Iq::Result { .. }));
}

#[tokio::test]
async fn a_failed_stream_to_the_rooms_domain_removes_the_occupant_with_status_333() {
    let pki = support::Pki::new();
    let (a, b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    // The peer shuts down: its streams end with `system-shutdown`, a
    // stream error, so stanzas in flight may have been lost.
    let _ = b.service.shutdown(Duration::from_secs(5)).await;
    let removed = wait_for(STREAM_LOSS, || {
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .find(|presence| presence.type_ == PresenceType::Unavailable)
    })
    .await
    .expect("the occupant is told it left the room");
    let codes = statuses(&removed);
    assert!(codes.contains(&"110".to_owned()), "{codes:?}");
    assert!(codes.contains(&"333".to_owned()), "{codes:?}");
    assert!(!codes.contains(&"307".to_owned()), "{codes:?}");
    assert_eq!(a.router.remote_rooms(), 0);
    // Only once: the second stream of the pair finds nothing left.
    tokio::time::sleep(QUIET).await;
    assert_eq!(
        delivered_presence(&a, ALICE_PHONE, ROOM_ALICE)
            .into_iter()
            .filter(|presence| presence.type_ == PresenceType::Unavailable)
            .count(),
        1
    );
}

#[tokio::test]
async fn a_resource_that_ends_leaves_its_remote_rooms() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    assert_eq!(a.router.remote_rooms(), 1);

    a.router.resource_ended(&full(ALICE_PHONE)).await;
    assert_eq!(a.router.remote_rooms(), 0);
    let left = wait_for(ARRIVES, || {
        muc.received().into_iter().find(|stanza| {
            stanza.name() == "presence" && stanza.attr("type") == Some("unavailable")
        })
    })
    .await
    .expect("the room receives the leave");
    assert_eq!(left.attr("from"), Some(ALICE_PHONE));
    assert_eq!(left.attr("to"), Some(ROOM_ALICE));
    assert!(
        wait_for(ARRIVES, || muc
            .nick_of(ROOM, ALICE_PHONE)
            .is_none()
            .then_some(()))
        .await
        .is_some(),
        "the room no longer seats the resource"
    );
}
