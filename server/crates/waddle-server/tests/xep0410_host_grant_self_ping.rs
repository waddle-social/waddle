//! XEP-0410 self-ping in a room an enrolled host-grant scope manages
//! (ADR-0020 D5).
//!
//! The service advertises the self-ping optimization, so its answer to a
//! ping of the occupant's own nick is authoritative: a result says the
//! client is joined and `<not-acceptable/>` says it is not (XEP-0410 §4). In
//! a managed room that answer comes from the canonical grants before it
//! comes from occupancy: an account whose grant was revoked is not joined,
//! whether or not the room actor has removed its seat yet and on whichever
//! replica it asks. While the grants cannot be read the answer is
//! `<remote-server-timeout/>`, the error XEP-0410 has a client take no
//! decision from, so nobody leaves and rejoins a room over a database that
//! did not answer.
//!
//! The cases need PostgreSQL only; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them without it.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
mod ws_loopback_support;

use std::time::Duration;

use host_grant_revocation_support::{error_condition, join, Deployment, NS_PING};
use host_grant_support::{OWNER, PEER};
use waddle_server::permissions::host_grant::RoomAffiliation;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{get, is_error, is_result};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// A ping of an occupant's own nick.
fn self_ping(id: &str, room: &jid::BareJid, nick: &str) -> xmpp_parsers::iq::Iq {
    get(
        id,
        &format!("{room}/{nick}"),
        Element::builder("ping", NS_PING).build(),
    )
}

#[tokio::test]
async fn a_revoked_occupant_is_told_it_is_not_joined_on_every_replica() {
    let Some(deployment) = Deployment::start("hg_ping_revoked", 2).await else {
        return;
    };
    let room = deployment.room().clone();
    let second = &deployment.replicas[1];
    let mut alice = second.client("alice", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    let reply = round_trip(&mut alice, self_ping("joined", &room, "alice")).await;
    assert!(is_result(&reply), "a seated member's self-ping: {reply:?}");

    let generation = deployment
        .grants
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke")
        .record()
        .generation;
    let acknowledged = deployment
        .grants
        .store
        .await_revocation_fence(
            deployment.grants.scope.lifecycle,
            generation,
            Duration::from_secs(10),
        )
        .await
        .expect("fence reading");
    assert!(acknowledged.acknowledged());
    // `<not-acceptable/>` says the client is not joined. It is the answer
    // from the grants, whether or not the room actor has removed the seat
    // yet.
    let reply = round_trip(&mut alice, self_ping("revoked", &room, "alice")).await;
    assert!(
        is_error(&reply, DefinedCondition::NotAcceptable),
        "a revoked member's self-ping: {reply:?}"
    );
    drop(alice);
    deployment.close().await;
}

#[tokio::test]
async fn a_self_ping_takes_no_decision_while_the_grants_cannot_be_read() {
    let Some(deployment) = Deployment::start("hg_ping_unreadable", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let server = &deployment.replicas[0];
    let mut alice = server.client("alice", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");

    // The replica stops renewing its revocation-fence lease, as one that
    // lost its database would: once the lease has run out it decides
    // nothing for the room.
    let fence = deployment.fence(0);
    fence.suspend_for_test(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fence.leased() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the lease did not run out"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reply = round_trip(&mut alice, self_ping("unreadable", &room, "alice")).await;
    assert!(
        is_error(&reply, DefinedCondition::RemoteServerTimeout),
        "a self-ping while the grants cannot be read: {reply:?}"
    );

    // Once the replica has registered again the seated member is joined.
    fence.suspend_for_test(false);
    let reply = round_trip(&mut alice, self_ping("readable", &room, "alice")).await;
    assert!(is_result(&reply), "a seated member's self-ping: {reply:?}");
    drop(alice);
    deployment.close().await;
}
