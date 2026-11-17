//! XEP-0198 (1.6.3) when an account is retired (ADR-0020 D8;
//! `docs/operations/account-retirement.md`): once a replica takes the
//! request to end the retired account's sessions up, a resumable live
//! stream is closed with `<not-authorized/>` (RFC 6120 §4.9.3.12) and its
//! session ended rather than detached for a resumption (§5), a session that
//! waits for a resumption is ended and leaves its rooms, what either held
//! is discarded rather than stored for the account's next session, and a
//! resumption of the account's session is refused: no `<resumed/>` is sent
//! and no resource is bound. The ingress custody of what a detached session
//! held ends with it: the recovery of custody whose session is gone hands
//! none of it to the offline store, which refuses every row for the
//! account from the retirement on. Through the production `/ws` router over a
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
use waddle_server::test_support::run_ingress_custody_sweep;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

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

/// The ingress custody the session registry holds, not yet ended, of
/// stanzas accepted for `resource`.
async fn pending_custody(server: &Server, resource: &FullJid) -> usize {
    server
        .state
        .deps
        .protocol
        .sm_session_registry
        .list_pending_ingress_appends_after(None, 1024)
        .await
        .expect("the ingress custody is readable")
        .iter()
        .filter(|append| append.key.resource == *resource)
        .count()
}

/// A stored chat message from Bob's desk to `to`.
fn direct_message(to: &FullJid, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(to.clone().into()));
    message.id = Some(Id(id.to_owned()));
    message
        .bodies
        .insert(Lang::new(), "for when you are back".to_owned());
    message
}

#[tokio::test]
async fn a_retired_accounts_ingress_custody_is_discarded_and_never_reaches_its_offline_store() {
    let server = Server::start_with_registry_sessions().await;
    let alice = server.account("alice").await;
    server.account("bob").await;
    let resource: FullJid = alice.with_resource_str("phone").expect("full JID");

    // A resumable session that is available, then loses its connection and
    // waits for a resumption (XEP-0198 §5).
    let mut phone = server.client("alice", "phone").await;
    enable_resumption(&mut phone).await;
    send(&mut phone, Presence::new(PresenceType::None)).await;
    drop(phone);
    let deadline = Instant::now() + WAIT;
    while !server.detached(&alice).await.contains(&resource) {
        assert!(Instant::now() < deadline, "the session detaches");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Bob's messages reach the detached session through the ingress
    // authority, which keeps custody of each until the session acknowledges
    // it or the session ends.
    let mut bobs = server.client("bob", "desk").await;
    for id in ["held-1", "held-2"] {
        send(&mut bobs, direct_message(&resource, id)).await;
    }
    let deadline = Instant::now() + WAIT;
    while pending_custody(&server, &resource).await < 2 {
        assert!(
            Instant::now() < deadline,
            "the detached session holds Bob's messages in ingress custody"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);
    assert!(server.detached(&alice).await.is_empty());
    // The session's end ended its custody itself, before any recovery ran.
    assert_eq!(pending_custody(&server, &resource).await, 0);
    assert_eq!(server.offline_messages(&alice).await, 0);

    // The recovery of custody whose session is gone settles every candidate
    // and finds nothing of the account's to hand on.
    assert!(
        run_ingress_custody_sweep(&server.state).await,
        "the recovery settles every candidate"
    );
    assert_eq!(pending_custody(&server, &resource).await, 0);
    assert_eq!(server.offline_messages(&alice).await, 0);

    // The store refuses any later row for the account.
    let mut late = direct_message(&resource, "late");
    late.from = Some(
        "bob@example.com/desk"
            .parse::<jid::Jid>()
            .expect("Bob's resource"),
    );
    let inserted = server
        .state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: alice.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(late)),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("the offline store is writable");
    assert_eq!(inserted, InsertOutcome::RecipientRetired);
    assert_eq!(server.offline_messages(&alice).await, 0);
}
