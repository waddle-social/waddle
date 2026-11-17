//! XEP-0198 (1.6.3) when an account is retired (ADR-0020 D8;
//! `docs/operations/account-retirement.md`): once a replica takes the
//! request to end the retired account's sessions up, a resumable live
//! stream is closed with `<not-authorized/>` (RFC 6120 §4.9.3.12) and its
//! session ended rather than detached for a resumption (§5), a session that
//! waits for a resumption is ended and leaves its rooms, what either held
//! is discarded rather than stored for the account's next session, and a
//! resumption of the account's session is refused: no `<resumed/>` is sent
//! and no resource is bound. Through the production `/ws` router over a
//! loopback socket against the in-memory test state; the replica's work
//! runs one poll at a time. No PostgreSQL, SpiceDB or second process is
//! involved.
#![cfg(feature = "test-support")]

// The fixture serves two suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
#[path = "account_retirement_support/mod.rs"]
mod support;

use std::time::{Duration, Instant};

use jid::FullJid;
use minidom::Element;
use support::{
    enable_resumption, ended_not_authorized, frames_until_closed, occupant_presence, occupied_room,
    receive, send, Server, WAIT,
};
use waddle_server::auth::session_endings::{await_session_endings, EndedSessions, ReplicaEnding};
use waddle_xmpp::stream_management::SM_NS;

#[tokio::test]
async fn a_resumable_live_stream_of_a_retired_account_is_closed_and_not_kept_for_resumption() {
    let server = Server::start().await;
    let (alice, room, mut bobs, mut alices) = occupied_room(&server).await;
    enable_resumption(&mut alices).await;

    let request = server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    // The stream is closed as a replacing bind closes one: its session is
    // ended, not detached for a resumption.
    ended_not_authorized(&frames_until_closed(&mut alices).await);
    assert!(server.detached(&alice).await.is_empty());
    // What the session had not delivered is not kept for a later session:
    // the account has none.
    assert_eq!(server.offline_messages(&alice).await, 0);
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", Some("unavailable"))
    })
    .await;
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert_eq!(
        report.replicas[0].ending,
        ReplicaEnding::Completed(EndedSessions {
            streams_closed: 1,
            detached_sessions_ended: 0,
        })
    );
}

#[tokio::test]
async fn a_detached_session_of_a_retired_account_is_ended_and_leaves_its_room() {
    let server = Server::start().await;
    let (alice, room, mut bobs, mut alices) = occupied_room(&server).await;
    let resource: FullJid = alice.with_resource_str("phone").expect("full JID");

    // XEP-0198 §5: a resumable session whose connection is lost waits for
    // a resumption, and keeps its room.
    enable_resumption(&mut alices).await;
    drop(alices);
    let deadline = Instant::now() + WAIT;
    while !server.detached(&alice).await.contains(&resource) {
        assert!(Instant::now() < deadline, "the session detaches");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let request = server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    // The session is discarded, not moved to the offline store (§5 moves a
    // session that cannot resume there for the account's next session; a
    // retired account has none).
    assert!(server.detached(&alice).await.is_empty());
    assert_eq!(server.offline_messages(&alice).await, 0);
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", Some("unavailable"))
    })
    .await;
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert_eq!(
        report.replicas[0].ending,
        ReplicaEnding::Completed(EndedSessions {
            streams_closed: 0,
            detached_sessions_ended: 1,
        })
    );
}

#[tokio::test]
async fn a_resumption_of_a_retired_accounts_session_is_refused() {
    let server = Server::start().await;
    let alice = server.account("alice").await;
    let resource: FullJid = alice.with_resource_str("phone").expect("full JID");
    let mut phone = server.client("alice", "phone").await;
    let previd = enable_resumption(&mut phone).await;
    // A stream that authenticated before the retirement committed, and
    // asks to resume the session afterwards.
    let mut resumer = server.authenticated("alice").await;
    drop(phone);
    let deadline = Instant::now() + WAIT;
    while !server.detached(&alice).await.contains(&resource) {
        assert!(Instant::now() < deadline, "the session detaches");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    send(
        &mut resumer,
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), previd)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build(),
    )
    .await;
    // §5: a session the server no longer holds is answered with
    // `<failed/>`; a stream of a retired account may also be ended at once.
    let answer = receive(&mut resumer, |frame| {
        frame.is("resumed", SM_NS)
            || frame.is("failed", SM_NS)
            || frame.is("error", waddle_xmpp::ns::STREAM)
    })
    .await;
    assert!(!answer.is("resumed", SM_NS), "{answer:?}");
    assert!(server.detached(&alice).await.is_empty());
    assert!(!server
        .state
        .deps
        .protocol
        .connection_registry
        .is_connected(&resource));
}
