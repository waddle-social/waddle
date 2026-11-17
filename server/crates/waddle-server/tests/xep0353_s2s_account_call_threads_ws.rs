//! XEP-0353 (0.8.0) call threads for direct messages from remote accounts
//! across S2S (ADR-0020 D7; gate G18). A remote sender's message is
//! accepted through the ingress authority as the recipient's half of a
//! local direct message, and the replica's call state follows its JMI
//! payloads for the conversation between the two:
//!
//! - a `finish` (§3.7) of an active call ends the call thread in the local
//!   account's inbox alone, and is not refused for the remote peer's
//!   inbox, which is its own server's;
//! - a `propose` (§3.1) records a pending offer only within bounds: a
//!   remote initiator holds at most eight, and a local account at most 32
//!   from remote initiators together, so that strangers cannot crowd the
//!   offers of other accounts out of the replica's call state. A proposal
//!   beyond the bounds is still delivered.
//!
//! Through the production `/ws` router, the real S2S router and a loopback
//! peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::BareJid;
use support::{federated, jid, receive, Federated, Pki, BOB, BOB_PHONE, PEER};
use waddle_server::test_support::seed_active_dm_call;
use waddle_xmpp::ingress::{DmCallThreadKey, PendingDmCallOffer};
use waddle_xmpp::xep::xep0334::{add_hint, Hint};
use waddle_xmpp::xep::xep0353::{build_finish, build_propose, CallOffer};
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::{Id, Message, MessageType};

const ALICE: &str = "alice@example.com";

fn bare(raw: &str) -> BareJid {
    raw.parse().expect("a bare JID")
}

/// A stored `chat` message from `from` to Alice carrying `payload`, as a
/// client sends a JMI message (XEP-0353 §3: with a `<store/>` hint).
fn jmi(from: &str, id: &str, payload: minidom::Element) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(jid(ALICE)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(payload);
    add_hint(&mut message, Hint::Store);
    message
}

/// The pending call offers of the replica whose initiator is `initiator`.
fn offers_of(federated: &Federated, initiator: &BareJid) -> usize {
    federated
        .loopback
        .state
        .deps
        .protocol
        .pending_dm_call_offers
        .iter()
        .filter(|entry| entry.value().initiator == *initiator)
        .count()
}

#[tokio::test]
async fn a_remote_finish_ends_the_call_thread_on_the_local_side_only() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let key = DmCallThreadKey::new(
        bare(ALICE),
        bare(BOB),
        SessionId("call-with-bob".to_owned()),
    );
    // Bob called Alice and she accepted: the call thread is in her inbox.
    seed_active_dm_call(
        &federated.loopback.state,
        key.clone(),
        bare(BOB),
        &[bare(ALICE)],
    );

    federated
        .from_peer(jmi(
            BOB_PHONE,
            "bob-hangs-up",
            build_finish(SessionId("call-with-bob".to_owned()), None),
        ))
        .await;

    // Accepted, not refused for the remote peer's inbox: Alice receives it,
    // her archive holds it, and the call is over in the replica's state.
    receive(&mut phone, "message", "bob-hangs-up").await;
    support::wait_for_archived(&federated, ALICE, "bob-hangs-up").await;
    let protocol = &federated.loopback.state.deps.protocol;
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            (!protocol.dm_call_threads.contains_key(&key)).then_some(())
        })
        .await
        .is_some(),
        "the remote finish ends the active call"
    );
    assert!(!protocol
        .dm_call_thread_projections
        .contains(&(bare(ALICE), key.clone())));
}

#[tokio::test]
async fn a_remote_initiators_pending_offers_are_bounded() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    // An offer of a local account, which strangers' offers do not crowd
    // out.
    let carols = DmCallThreadKey::new(
        bare("carol@example.com"),
        bare(ALICE),
        SessionId("carols-call".to_owned()),
    );
    federated
        .loopback
        .state
        .deps
        .protocol
        .pending_dm_call_offers
        .insert(
            carols.clone(),
            PendingDmCallOffer {
                media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
                initiator: bare("carol@example.com"),
                started: chrono::Utc::now(),
            },
        );

    for index in 0..9 {
        let id = ["bob-calls-", &index.to_string()].concat();
        federated
            .from_peer(jmi(
                BOB_PHONE,
                &id,
                build_propose(
                    SessionId(["bob-call-", &index.to_string()].concat()),
                    CallOffer::audio_only(),
                ),
            ))
            .await;
        // Every proposal is delivered, the ninth too, and its acceptance
        // has run before the next is sent.
        receive(&mut phone, "message", &id).await;
        federated.peer_barrier(BOB_PHONE, ALICE).await;
    }
    let bob = bare(BOB);
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            (offers_of(&federated, &bob) == 8).then_some(())
        })
        .await
        .is_some(),
        "eight of Bob's offers are pending"
    );
    assert_eq!(offers_of(&federated, &bob), 8);
    assert!(federated
        .loopback
        .state
        .deps
        .protocol
        .pending_dm_call_offers
        .contains_key(&carols));
}

#[tokio::test]
async fn an_accounts_pending_offers_from_remote_initiators_are_bounded() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    // One offer from each of 33 accounts at the peer.
    for index in 0..33 {
        let caller = ["caller", &index.to_string(), "@", PEER, "/phone"].concat();
        let id = ["call-", &index.to_string()].concat();
        federated
            .from_peer(jmi(
                &caller,
                &id,
                build_propose(SessionId(id.clone()), CallOffer::audio_only()),
            ))
            .await;
        receive(&mut phone, "message", &id).await;
        federated.peer_barrier(&caller, ALICE).await;
    }
    let remote_offers = || {
        federated
            .loopback
            .state
            .deps
            .protocol
            .pending_dm_call_offers
            .iter()
            .filter(|entry| entry.value().initiator.domain().as_str() == PEER)
            .count()
    };
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            (remote_offers() == 32).then_some(())
        })
        .await
        .is_some(),
        "32 offers from remote initiators are pending for Alice"
    );
    assert_eq!(remote_offers(), 32);
}
