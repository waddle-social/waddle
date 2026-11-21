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
//!   remote initiator holds at most eight, a local account at most 16 from
//!   the initiators of any one remote domain and 32 from remote initiators
//!   together, and the initiators of one remote domain at most 64 in all,
//!   so that strangers, and a server minting them, cannot crowd the offers
//!   of other accounts and other servers out of the replica's call state.
//!   A proposal beyond the bounds is still delivered. Once the call state
//!   is full, the offer pruned is the oldest of the initiator holding the
//!   most, not the oldest of all.
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
use waddle_server::test_support::{dm_call_state_capacity, seed_active_dm_call};
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

/// The pending call offers to Alice whose initiator is at `domain`.
fn offers_to_alice_from(federated: &Federated, domain: &str) -> usize {
    let alice = bare(ALICE);
    federated
        .loopback
        .state
        .deps
        .protocol
        .pending_dm_call_offers
        .iter()
        .filter(|entry| {
            let key = entry.key();
            entry.value().initiator.domain().as_str() == domain
                && (key.low_peer == alice || key.high_peer == alice)
        })
        .count()
}

/// Record in the replica's call state a pending offer from `initiator` to
/// `recipient` under `sid`, made `age` ago, as an accepted proposal would.
fn seed_offer(
    federated: &Federated,
    initiator: &str,
    recipient: &str,
    sid: &str,
    age: chrono::Duration,
) -> DmCallThreadKey {
    let key = DmCallThreadKey::new(bare(initiator), bare(recipient), SessionId(sid.to_owned()));
    federated
        .loopback
        .state
        .deps
        .protocol
        .pending_dm_call_offers
        .insert(
            key.clone(),
            PendingDmCallOffer {
                media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
                initiator: bare(initiator),
                started: chrono::Utc::now() - age,
            },
        );
    key
}

/// Send Alice a proposal from each of `callers`, each under its own id and
/// session, waiting for each to be delivered and accepted before the next.
async fn propose_from(
    federated: &Federated,
    phone: &mut waddle_ws_test_support::WsXmppClient,
    callers: &[String],
) {
    for caller in callers {
        let id = ["call-from-", caller.split('@').next().unwrap_or("caller")].concat();
        federated
            .peer_sends(jmi(
                caller,
                &id,
                build_propose(SessionId(id.clone()), CallOffer::audio_only()),
            ))
            .await;
        receive(phone, "message", &id).await;
        federated.peer_barrier(caller, ALICE).await;
    }
}

/// `count` callers at the peer, each with a phone resource, named after
/// `prefix`.
fn peer_callers(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| [prefix, &index.to_string(), "@", PEER, "/phone"].concat())
        .collect()
}

/// Wait until `count` reads `expected`, then assert it does.
async fn settles_at(count: impl Fn() -> usize, expected: usize, what: &str) {
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            (count() == expected).then_some(())
        })
        .await
        .is_some(),
        "{what}: {} rather than {expected}",
        count()
    );
    assert_eq!(count(), expected, "{what}");
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
        .peer_sends(jmi(
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

    for index in 0..9 {
        let id = ["bob-calls-", &index.to_string()].concat();
        federated
            .peer_sends(jmi(
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
    settles_at(|| offers_of(&federated, &bob), 8, "Bob's pending offers").await;
}

#[tokio::test]
async fn a_full_call_state_prunes_the_offers_of_the_initiator_holding_most() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let capacity = dm_call_state_capacity();

    // Carol, a local account, made the oldest offer of all. One offer each
    // from other local accounts, and Bob's eight, fill the call state to
    // its capacity.
    let carols = seed_offer(
        &federated,
        "carol@example.com",
        ALICE,
        "carols-call",
        chrono::Duration::minutes(10),
    );
    for index in 0..capacity - 9 {
        let filler = ["filler", &index.to_string(), "@example.com"].concat();
        seed_offer(
            &federated,
            &filler,
            "someone@example.com",
            &["filler-call-", &index.to_string()].concat(),
            chrono::Duration::minutes(5),
        );
    }
    for index in 0..8 {
        let id = ["bob-calls-", &index.to_string()].concat();
        federated
            .peer_sends(jmi(
                BOB_PHONE,
                &id,
                build_propose(
                    SessionId(["bob-call-", &index.to_string()].concat()),
                    CallOffer::audio_only(),
                ),
            ))
            .await;
        receive(&mut phone, "message", &id).await;
        federated.peer_barrier(BOB_PHONE, ALICE).await;
    }
    let bob = bare(BOB);
    settles_at(|| offers_of(&federated, &bob), 8, "Bob's pending offers").await;
    let offers = || {
        federated
            .loopback
            .state
            .deps
            .protocol
            .pending_dm_call_offers
            .len()
    };
    assert_eq!(offers(), capacity, "the call state is full");

    // Dave's proposal takes the call state past its capacity, and Erin's
    // acceptance prunes it at the latest: what is pruned is Bob's oldest
    // offer, Bob holding the most, and never Carol's, the oldest of all.
    let callers = [
        ["dave@", PEER, "/phone"].concat(),
        ["erin@", PEER, "/phone"].concat(),
    ];
    propose_from(&federated, &mut phone, &callers).await;
    for caller in ["dave@", "erin@"] {
        settles_at(
            || offers_of(&federated, &bare(&[caller, PEER].concat())),
            1,
            "a new caller's pending offer",
        )
        .await;
    }
    assert!(
        offers_of(&federated, &bob) < 8,
        "an offer of Bob's, who holds the most, is pruned"
    );
    assert!(
        federated
            .loopback
            .state
            .deps
            .protocol
            .pending_dm_call_offers
            .contains_key(&carols),
        "Carol's offer, the oldest of all, is kept"
    );
    assert!(offers() <= capacity + 1, "{} offers", offers());
}

#[tokio::test]
async fn an_accounts_pending_offers_from_one_remote_domain_are_bounded() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    // One offer from each of 17 accounts at the peer.
    propose_from(&federated, &mut phone, &peer_callers("caller", 17)).await;
    settles_at(
        || offers_to_alice_from(&federated, PEER),
        16,
        "offers to Alice from the peer's accounts",
    )
    .await;
}

#[tokio::test]
async fn an_accounts_pending_offers_from_remote_initiators_are_bounded() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    // Twenty offers to Alice from accounts at a third server are pending:
    // only twelve more from remote initiators fit, fewer than the peer's
    // own bound of sixteen.
    for index in 0..20 {
        seed_offer(
            &federated,
            &["caller", &index.to_string(), "@home-c.test"].concat(),
            ALICE,
            &["home-c-call-", &index.to_string()].concat(),
            chrono::Duration::minutes(1),
        );
    }
    propose_from(&federated, &mut phone, &peer_callers("caller", 13)).await;
    settles_at(
        || offers_to_alice_from(&federated, PEER),
        12,
        "offers to Alice from the peer's accounts",
    )
    .await;
}

#[tokio::test]
async fn one_remote_domains_pending_offers_are_bounded_across_accounts() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    // Sixty offers from the peer's accounts to other local accounts are
    // pending: four more from the peer fit, to Alice too.
    for index in 0..60 {
        seed_offer(
            &federated,
            &["seed", &index.to_string(), "@", PEER].concat(),
            &["someone", &index.to_string(), "@example.com"].concat(),
            &["peer-call-", &index.to_string()].concat(),
            chrono::Duration::minutes(1),
        );
    }
    propose_from(&federated, &mut phone, &peer_callers("caller", 5)).await;
    settles_at(
        || offers_to_alice_from(&federated, PEER),
        4,
        "offers to Alice from the peer's accounts",
    )
    .await;
}
