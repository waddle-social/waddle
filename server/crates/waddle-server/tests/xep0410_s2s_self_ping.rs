//! XEP-0410 self-ping to a room on a remote domain: the ping is an IQ
//! request to the occupant's own JID, routed and correlated like any
//! request to a remote entity, and the client receives the room's answer
//! as the room gave it. A room that reflects the ping to the occupant
//! (§3.2) reaches the client once the router has an account side, which
//! admits IQ requests from a room the resource occupies (gate G18), and
//! the client's answer goes back through the room. A router without an
//! account side answers the reflection itself with `service-unavailable`
//! (ADR-0020 D7), which XEP-0410 §3.2 reads as "still joined".
//!
//! `home-a.test` runs the S2S router with a recording delivery in place of
//! WebSocket clients; `home-b.test` serves `muc.home-b.test` with a
//! hand-written room service. No PostgreSQL, SpiceDB or second process is
//! involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_remote_muc_support/mod.rs"]
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use jid::{FullJid, Jid};
use minidom::Element;
use support::{
    error_condition, full, jid, join, outbound_muc, wait_for, ALICE_PHONE, ARRIVES, ROOM,
};
use waddle_server::destination::IqOffer;
use waddle_server::s2s::route::{
    AccountWork, LocalAccounts, LocalOrigin, RemotePrincipal, RouterLimits,
};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

const ROOM_ALICE: &str = "room@muc.home-b.test/alice";

fn self_ping(id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(ALICE_PHONE)),
        to: Some(jid(ROOM_ALICE)),
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

/// The one answer delivered to the pinging resource for `id`.
async fn answer(a: &support::Routed, id: &str) -> Iq {
    let (to, iq) = wait_for(ARRIVES, || a.delivery.iqs(id).into_iter().next())
        .await
        .expect("the self-ping is answered");
    assert_eq!(to, full(ALICE_PHONE));
    assert_eq!(iq.from(), Some(&jid(ROOM_ALICE)));
    iq
}

#[tokio::test]
async fn a_self_ping_while_joined_is_answered_by_the_room() {
    let pki = support::Pki::new();
    let (a, _b, _muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = a.offer_iq(ROOM_ALICE, self_ping("sp1")).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    let iq = answer(&a, "sp1").await;
    assert!(matches!(iq, Iq::Result { .. }), "{iq:?}");
    assert_eq!(a.delivery.origins("sp1"), vec![LocalOrigin::Remote]);
}

#[tokio::test]
async fn a_self_ping_the_room_no_longer_seats_reports_not_acceptable() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    join(&a, ALICE_PHONE, ROOM_ALICE).await;
    // The room lost the occupant without telling it: this server still
    // records the occupancy, and the room's answer says otherwise.
    muc.forget_occupant(ROOM, ALICE_PHONE);
    assert_eq!(a.router.remote_rooms().await, 1);

    let offered = a.offer_iq(ROOM_ALICE, self_ping("sp2")).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    let iq = answer(&a, "sp2").await;
    assert!(matches!(iq, Iq::Error { .. }), "{iq:?}");
    assert_eq!(
        error_condition(&Element::from(iq)).as_deref(),
        Some("not-acceptable")
    );
    assert_eq!(a.delivery.origins("sp2"), vec![LocalOrigin::Remote]);
}

#[tokio::test]
async fn a_self_ping_to_a_room_never_joined_is_routed_and_answered() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    let offered = a.offer_iq(ROOM_ALICE, self_ping("sp3")).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    let iq = answer(&a, "sp3").await;
    assert_eq!(
        error_condition(&Element::from(iq)).as_deref(),
        Some("not-acceptable")
    );
    // A self-ping is a request, not a join: it neither opens an occupancy
    // nor asks the room's domain whether it is a multi-user chat service.
    assert_eq!(a.router.remote_rooms().await, 0);
    assert!(muc.disco_requests().is_empty());
}

#[tokio::test]
async fn a_reflected_self_ping_is_refused_so_the_room_reports_the_occupant_joined() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    muc.reflect_pings();
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = a.offer_iq(ROOM_ALICE, self_ping("sp4")).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    // The reflected request reaches no client: the router refuses it as
    // it refuses every IQ request to a local account.
    let refusal = wait_for(ARRIVES, || {
        muc.received_with_id("iq", "reflected-sp4")
            .into_iter()
            .next()
    })
    .await
    .expect("the reflected ping is refused");
    assert_eq!(refusal.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(a.delivery.iqs("reflected-sp4").is_empty());
    // The room relays that answer, which XEP-0410 §3.2 reads as joined.
    let iq = answer(&a, "sp4").await;
    assert_eq!(
        error_condition(&Element::from(iq)).as_deref(),
        Some("service-unavailable")
    );
}

/// An account side that admits IQ requests from a remote room the resource
/// occupies, and nothing else.
struct RoomOccupantsOnly;

#[async_trait]
impl LocalAccounts for RoomOccupantsOnly {
    fn admits(&self, _principal: &RemotePrincipal) -> bool {
        true
    }

    async fn receive(
        &self,
        _principal: &RemotePrincipal,
        _to: &Jid,
        _stanza: Stanza,
        _work: &AccountWork,
    ) -> Vec<Stanza> {
        Vec::new()
    }

    async fn admits_request(
        &self,
        _principal: &RemotePrincipal,
        _to: &FullJid,
        room_occupant: bool,
    ) -> bool {
        room_occupant
    }

    async fn may_send(&self, _account: &jid::BareJid) -> bool {
        true
    }
}

#[tokio::test]
async fn a_reflected_self_ping_reaches_the_client_once_an_account_side_is_attached() {
    let pki = support::Pki::new();
    let (a, _b, muc) = outbound_muc(&pki, RouterLimits::default()).await;
    assert!(a.router.attach_accounts(Arc::new(RoomOccupantsOnly)));
    muc.reflect_pings();
    join(&a, ALICE_PHONE, ROOM_ALICE).await;

    let offered = a.offer_iq(ROOM_ALICE, self_ping("sp5")).await;
    assert!(matches!(offered, IqOffer::Accepted), "{offered:?}");
    // The room's reflection reaches the occupant, as a request from the
    // room it occupies.
    let (to, reflected) = wait_for(ARRIVES, || {
        a.delivery.iqs("reflected-sp5").into_iter().next()
    })
    .await
    .expect("the reflected ping reaches the client");
    assert_eq!(to, full(ALICE_PHONE));
    assert!(matches!(reflected, Iq::Get { .. }), "{reflected:?}");
    assert_eq!(
        a.delivery.origins("reflected-sp5"),
        vec![LocalOrigin::Remote]
    );

    // The client answers; the room relays the answer to the self-ping.
    let answered = a
        .offer_iq(
            ROOM_ALICE,
            Iq::Result {
                from: Some(jid(ALICE_PHONE)),
                to: Some(jid(ROOM_ALICE)),
                id: "reflected-sp5".to_owned(),
                payload: None,
            },
        )
        .await;
    assert!(matches!(answered, IqOffer::Accepted), "{answered:?}");
    let iq = answer(&a, "sp5").await;
    assert!(matches!(iq, Iq::Result { .. }), "{iq:?}");
    let _ = ROOM;
}
