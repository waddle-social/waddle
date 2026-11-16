//! RFC 6121 account traffic across S2S (ADR-0020 D7; gate G18;
//! `server/docs/planning/s2s-account-traffic-2047.md`): a remote sender's
//! messages, presence, subscriptions, probes and IQ requests to local
//! accounts, and local accounts' subscriptions and presence to remote
//! contacts, through the production `/ws` router, the real S2S router and a
//! loopback peer, `home-b.test`.
//!
//! The no-enumeration matrix is table-driven: for a sender that is not the
//! account's contact, every class gets one remote-visible answer whatever
//! the account's state, and the answers observed in the states are
//! compared with each other. A case that asserts that nothing arrived ends
//! with a barrier that does arrive on the same path after it (a stanza on
//! the sender's own queue, then a ping on the same pair, then a ping of the
//! client's own server), and checks every stanza received since, so that
//! "nothing arrived" is neither a slow delivery nor a stanza discarded
//! while waiting for another.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::{BareJid, Jid};
use minidom::Element;
use support::{
    block, client_settle, contact, error_condition, federated, federated_with, full_jid, get,
    has_no_body, holds, is_message_error, is_presence, jid, message, ping, presence, receive,
    receive_until, roster_item, send, Adjust, Federated, Pki, BOB, BOB_PHONE, LOCAL, MALLORY, PEER,
};
use waddle_ws_test_support::iq::round_trip;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::roster::{Item, Roster, Subscription};

const ALICE: &str = "alice@example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";
const ALICE_LAPTOP: &str = "alice@example.com/laptop";

/// The stanza names and ids that would show that `frames` hold `id`.
fn any_with_id(frames: &[Element], id: &str) -> bool {
    frames.iter().any(|frame| frame.attr("id") == Some(id))
}

// --- messages ----------------------------------------------------------------

#[tokio::test]
async fn a_chat_message_to_the_bare_jid_reaches_the_one_most_available_resource() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 1).await;
    let mark = federated.peer_mark();

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "m1",
            "hello alice",
        ))
        .await;

    let delivered = receive(&mut phone, "message", "m1").await;
    assert_eq!(delivered.attr("from"), Some(BOB_PHONE));
    // RFC 6121 §8.5.2.1.1: the 'to' of a bare-JID message is not rewritten.
    assert_eq!(delivered.attr("to"), Some(ALICE));
    // The lower-priority laptop, without Carbons, receives nothing; the
    // sender receives no answer.
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(!holds(&client_settle(&mut laptop).await, "message", "m1"));
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
}

#[tokio::test]
async fn a_chat_message_to_a_bound_full_jid_reaches_that_resource_only() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 1).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE_LAPTOP,
            MessageType::Chat,
            "m2",
            "to the laptop",
        ))
        .await;

    receive(&mut laptop, "message", "m2").await;
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(!holds(&client_settle(&mut phone).await, "message", "m2"));
}

#[tokio::test]
async fn a_chat_message_to_an_unbound_full_jid_is_handled_as_if_sent_to_the_bare_jid() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            "alice@example.com/gone",
            MessageType::Chat,
            "m3",
            "to a resource that left",
        ))
        .await;

    receive(&mut phone, "message", "m3").await;
}

#[tokio::test]
async fn a_normal_message_to_an_unbound_full_jid_is_dropped_unanswered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mark = federated.peer_mark();

    federated
        .from_peer(message(
            BOB_PHONE,
            "alice@example.com/gone",
            MessageType::Normal,
            "m4",
            "normal to a resource that left",
        ))
        .await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;

    assert!(!holds(&client_settle(&mut phone).await, "message", "m4"));
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
}

#[tokio::test]
async fn a_headline_reaches_every_resource_with_a_non_negative_priority() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 0).await;
    let mut away = federated.online("alice", "away", -1).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Headline,
            "h1",
            "news",
        ))
        .await;

    receive(&mut phone, "message", "h1").await;
    receive(&mut laptop, "message", "h1").await;
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(!holds(&client_settle(&mut away).await, "message", "h1"));
}

#[tokio::test]
async fn groupchat_to_an_account_is_refused_with_service_unavailable_without_the_body() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let _phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Groupchat,
            "g1",
            "not a room",
        ))
        .await;

    let refusal = federated.peer_receives("g1").await;
    assert!(is_message_error(&refusal));
    assert_eq!(refusal.attr("from"), Some(ALICE));
    assert_eq!(refusal.attr("to"), Some(BOB_PHONE));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(has_no_body(&refusal));
}

#[tokio::test]
async fn a_message_to_an_account_with_no_resource_is_stored_unanswered_from_contact_and_stranger() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;
    let mark = federated.peer_mark();

    // Bob may see Alice's presence and Mallory may not; neither is told
    // anything (RFC 6121 §8.5.2.2.1: stored for later delivery).
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "o1",
            "are you there",
        ))
        .await;
    federated
        .from_peer(message(
            MALLORY,
            ALICE,
            MessageType::Normal,
            "o2",
            "are you there",
        ))
        .await;
    let mut barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    barrier.extend(federated.peer_barrier(MALLORY, ALICE).await);
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert_eq!(support::offline_count(&federated, ALICE).await, 2);

    // Alice's next session receives both.
    let mut phone = federated.online("alice", "phone", 5).await;
    let frames = receive_until(&mut phone, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("o2")
    })
    .await;
    assert!(holds(&frames, "message", "o1"), "{frames:?}");
}

#[tokio::test]
async fn a_strangers_messages_beyond_its_offline_bound_are_dropped_unanswered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mark = federated.peer_mark();
    let bound = waddle_server::db::s2s_accounts::MAX_OFFLINE_PER_SENDER;

    for index in 0..=bound {
        let id = ["flood-", &index.to_string()].concat();
        federated
            .from_peer(message(MALLORY, ALICE, MessageType::Chat, &id, "hello"))
            .await;
    }
    let barrier = federated.peer_barrier(MALLORY, ALICE).await;
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert_eq!(
        support::offline_count(&federated, ALICE).await,
        u32::try_from(bound).expect("a small bound")
    );

    // A contact is not held to a stranger's bound.
    contact(&federated, ALICE, BOB, "from").await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "contact",
            "hi",
        ))
        .await;
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert_eq!(
        support::offline_count(&federated, ALICE).await,
        u32::try_from(bound + 1).expect("a small bound")
    );
}

#[tokio::test]
async fn a_chat_message_never_reaches_a_resource_with_a_negative_priority() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;
    let mut away = federated.online("alice", "away", -1).await;
    let mark = federated.peer_mark();

    federated
        .from_peer(message(BOB_PHONE, ALICE, MessageType::Chat, "n1", "hello"))
        .await;
    federated
        .from_peer(message(MALLORY, ALICE, MessageType::Chat, "n2", "hello"))
        .await;

    // RFC 6121 §8.5.2.1.1: handled as if no resource were available, so
    // stored, and nobody is answered.
    let mut barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    barrier.extend(federated.peer_barrier(MALLORY, ALICE).await);
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    let seen = client_settle(&mut away).await;
    assert!(!holds(&seen, "message", "n1"));
    assert!(!holds(&seen, "message", "n2"));

    // A session with a non-negative priority receives both.
    let mut desk = federated.online("alice", "desk", 1).await;
    let frames = receive_until(&mut desk, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("n2")
    })
    .await;
    assert!(holds(&frames, "message", "n1"), "{frames:?}");
}

#[tokio::test]
async fn private_payloads_of_a_remote_message_are_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut forged = message(BOB_PHONE, ALICE, MessageType::Chat, "p1", "pinned?");
    forged.payloads.push(
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
    );
    forged
        .payloads
        .push(Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
    federated.from_peer(forged).await;

    let delivered = receive(&mut phone, "message", "p1").await;
    assert!(delivered.children().all(|child| {
        child.ns() != waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0
            && child.ns() != waddle_xmpp::xep::NS_INBOX
    }));
}

#[tokio::test]
async fn room_traffic_from_a_room_never_joined_is_dropped_but_a_mediated_invitation_arrives() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let room = "room@home-b.test";

    let mut stale = presence(
        "room@home-b.test/host",
        ALICE_PHONE,
        PresenceType::None,
        "stale-room",
    );
    stale
        .payloads
        .push(xmpp_parsers::muc::user::MucUser::new().into());
    federated.from_peer(stale).await;

    let mut invitation = message(room, ALICE, MessageType::Normal, "invite", "join us");
    invitation.payloads.push(
        Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .attr(minidom::rxml::xml_ncname!("from").to_owned(), BOB)
                    .build(),
            )
            .build(),
    );
    federated.from_peer(invitation).await;

    // Both come from the room's bare JID and are decided in that order, so
    // the stale presence would have arrived before the invitation.
    let frames = receive_until(&mut phone, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("invite")
    })
    .await;
    assert!(!holds(&frames, "presence", "stale-room"), "{frames:?}");
}

#[tokio::test]
async fn a_chat_message_reaches_a_connected_resource_that_has_not_sent_presence() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut quiet = federated.loopback.client("alice", "quiet").await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "c1",
            "anyone there",
        ))
        .await;

    receive(&mut quiet, "message", "c1").await;
}

// --- no enumeration ------------------------------------------------------------

/// The account states a non-contact must not tell apart.
#[derive(Debug, Clone, Copy)]
enum AccountState {
    Nonexistent,
    Retired,
    Offline,
    Online,
    Blocking,
}

/// What a non-contact observed for one stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Nothing,
    ServiceUnavailable,
    /// Anything else the peer received, as it received it.
    Other(Vec<String>),
}

/// The answer the peer observed in `frames`, every stanza it received for
/// one stanza with `id` besides the barrier's.
fn observed(frames: &[Element], id: &str) -> Answer {
    match frames {
        [] => Answer::Nothing,
        [only]
            if only.attr("id") == Some(id)
                && is_error_stanza(only)
                && error_condition(only).as_deref() == Some("service-unavailable") =>
        {
            Answer::ServiceUnavailable
        }
        other => Answer::Other(other.iter().map(String::from).collect()),
    }
}

fn is_error_stanza(stanza: &Element) -> bool {
    stanza.attr("type") == Some("error")
}

/// Set up `localpart` in `state`; an online account's client is returned.
async fn account_in(
    federated: &Federated,
    localpart: &str,
    state: AccountState,
) -> Option<waddle_ws_test_support::WsXmppClient> {
    let account: BareJid = [localpart, "@", LOCAL].concat().parse().expect("a JID");
    match state {
        AccountState::Nonexistent => None,
        AccountState::Retired => {
            federated.account(localpart).await;
            // A retired account's open stream stays open; it must still be
            // treated as nonexistent.
            let client = federated.online(localpart, "phone", 5).await;
            waddle_server::auth::retirement::retire_account(
                federated.loopback.state.deps.app_state.db_pool.global(),
                &account,
            )
            .await
            .expect("the account is retired");
            Some(client)
        }
        AccountState::Offline => {
            federated.account(localpart).await;
            None
        }
        AccountState::Online => {
            federated.account(localpart).await;
            Some(federated.online(localpart, "phone", 5).await)
        }
        AccountState::Blocking => {
            federated.account(localpart).await;
            block(federated, account.as_str(), "mallory@home-b.test").await;
            Some(federated.online(localpart, "phone", 5).await)
        }
    }
}

/// One stanza class of the matrix: its name, its stanza from `MALLORY` to
/// an account's bare JID (`to`) with `id`, and the answer the design
/// records for it (`s2s-account-traffic-2047.md`, "No enumeration").
type Class = (&'static str, fn(&str, &str) -> Element, Answer);

fn classes() -> Vec<Class> {
    fn full(to: &str) -> String {
        [to, "/phone"].concat()
    }
    vec![
        (
            "chat",
            |to, id| message(MALLORY, to, MessageType::Chat, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "normal",
            |to, id| message(MALLORY, to, MessageType::Normal, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "headline",
            |to, id| message(MALLORY, to, MessageType::Headline, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "groupchat",
            |to, id| message(MALLORY, to, MessageType::Groupchat, id, "hi").into(),
            Answer::ServiceUnavailable,
        ),
        (
            "message-error",
            |to, id| message(MALLORY, to, MessageType::Error, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "chat-full",
            |to, id| message(MALLORY, &full(to), MessageType::Chat, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "message-error-full",
            |to, id| message(MALLORY, &full(to), MessageType::Error, id, "hi").into(),
            Answer::Nothing,
        ),
        (
            "available",
            |to, id| presence(MALLORY, to, PresenceType::None, id).into(),
            Answer::Nothing,
        ),
        (
            "unavailable",
            |to, id| presence(MALLORY, to, PresenceType::Unavailable, id).into(),
            Answer::Nothing,
        ),
        (
            "available-full",
            |to, id| presence(MALLORY, &full(to), PresenceType::None, id).into(),
            Answer::Nothing,
        ),
        (
            "subscribe",
            |to, id| presence(MALLORY, to, PresenceType::Subscribe, id).into(),
            Answer::Nothing,
        ),
        (
            "subscribed",
            |to, id| presence(MALLORY, to, PresenceType::Subscribed, id).into(),
            Answer::Nothing,
        ),
        (
            "unsubscribe",
            |to, id| presence(MALLORY, to, PresenceType::Unsubscribe, id).into(),
            Answer::Nothing,
        ),
        (
            "unsubscribed",
            |to, id| presence(MALLORY, to, PresenceType::Unsubscribed, id).into(),
            Answer::Nothing,
        ),
        (
            "probe",
            |to, id| presence(MALLORY, to, PresenceType::Probe, id).into(),
            Answer::Nothing,
        ),
        (
            "probe-full",
            |to, id| presence(MALLORY, &full(to), PresenceType::Probe, id).into(),
            Answer::Nothing,
        ),
        (
            "presence-error",
            |to, id| presence(MALLORY, to, PresenceType::Error, id).into(),
            Answer::Nothing,
        ),
        (
            "iq-bare",
            |to, id| get(MALLORY, to, id, ping()).into(),
            Answer::ServiceUnavailable,
        ),
        (
            "iq-full",
            |to, id| get(MALLORY, &full(to), id, ping()).into(),
            Answer::ServiceUnavailable,
        ),
    ]
}

#[tokio::test]
async fn a_non_contact_gets_the_same_answer_whatever_the_accounts_state() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let states = [
        ("ghost", AccountState::Nonexistent),
        ("gone", AccountState::Retired),
        ("asleep", AccountState::Offline),
        ("awake", AccountState::Online),
        ("guarded", AccountState::Blocking),
    ];
    let mut clients = Vec::new();
    for (localpart, state) in states {
        clients.push((localpart, account_in(&federated, localpart, state).await));
    }
    let classes = classes();
    // observations[class][state]
    let mut observations: Vec<Vec<(&str, Answer)>> = vec![Vec::new(); classes.len()];
    for (localpart, _) in &clients {
        let to = [*localpart, "@", LOCAL].concat();
        for (index, (class, build, _)) in classes.iter().enumerate() {
            let id = [*localpart, "-", *class].concat();
            let mark = federated.peer_mark();
            federated.from_peer(build(&to, &id)).await;
            // Everything the peer receives for this stanza has arrived once
            // a later stanza of Mallory's, decided after it, is answered.
            let barrier = federated.peer_barrier(MALLORY, &to).await;
            let frames = federated.peer_received_besides(mark, &barrier);
            observations[index].push((*localpart, observed(&frames, &id)));
        }
    }
    for ((class, _, expected), observed) in classes.iter().zip(&observations) {
        let (first_state, first) = &observed[0];
        for (state, answer) in observed {
            assert_eq!(
                answer, first,
                "{class}: {state} is told apart from {first_state}"
            );
        }
        assert_eq!(first, expected, "{class}");
    }
    // Nothing from Mallory reached the retired account's open stream or the
    // blocking account.
    for (localpart, client) in clients.iter_mut() {
        if let (Some(client), "gone" | "guarded") = (client.as_mut(), *localpart) {
            let seen = client_settle(client).await;
            let from_mallory: Vec<&Element> = seen
                .iter()
                .filter(|frame| {
                    frame
                        .attr("from")
                        .is_some_and(|from| from.starts_with("mallory@"))
                })
                .collect();
            assert!(from_mallory.is_empty(), "{localpart}: {from_mallory:?}");
        }
    }
}

#[tokio::test]
async fn a_contacts_message_gets_nothing_back_whether_the_account_is_online_or_offline() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("asleep").await;
    contact(&federated, "asleep@example.com", BOB, "both").await;
    federated.account("awake").await;
    contact(&federated, "awake@example.com", BOB, "both").await;
    let mut phone = federated.online("awake", "phone", 5).await;

    let mut answers = Vec::new();
    for to in ["asleep@example.com", "awake@example.com"] {
        let id = [to, "-contact-chat"].concat();
        let mark = federated.peer_mark();
        federated
            .from_peer(message(BOB_PHONE, to, MessageType::Chat, &id, "hi"))
            .await;
        let barrier = federated.peer_barrier(BOB_PHONE, to).await;
        answers.push(observed(
            &federated.peer_received_besides(mark, &barrier),
            &id,
        ));
    }
    assert_eq!(answers, vec![Answer::Nothing, Answer::Nothing]);
    let frames = receive_until(&mut phone, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("awake@example.com-contact-chat")
    })
    .await;
    assert!(!frames.is_empty());
    assert_eq!(
        support::offline_count(&federated, "asleep@example.com").await,
        1
    );
}

// --- presence ------------------------------------------------------------------

#[tokio::test]
async fn presence_to_the_bare_jid_reaches_available_resources_only_from_a_subscribed_contact() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "to").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 1).await;

    federated
        .from_peer(presence(BOB_PHONE, ALICE, PresenceType::None, "pr1"))
        .await;
    federated
        .from_peer(presence(MALLORY, ALICE, PresenceType::None, "pr2"))
        .await;

    receive(&mut phone, "presence", "pr1").await;
    receive(&mut laptop, "presence", "pr1").await;
    federated.peer_barrier(MALLORY, ALICE).await;
    assert!(!holds(&client_settle(&mut phone).await, "presence", "pr2"));
}

#[tokio::test]
async fn directed_presence_to_a_full_jid_reaches_that_resource() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(presence(MALLORY, ALICE_PHONE, PresenceType::None, "dp1"))
        .await;

    let delivered = receive(&mut phone, "presence", "dp1").await;
    assert_eq!(delivered.attr("from"), Some(MALLORY));
}

#[tokio::test]
async fn a_probe_from_a_contact_with_from_is_answered_with_each_available_resource() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let _phone = federated.online("alice", "phone", 5).await;
    let _laptop = federated.online("alice", "laptop", 1).await;
    // Bob becomes a contact after the initial presence, so the only
    // presence he receives answers his probe.
    contact(&federated, ALICE, BOB, "both").await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "pb1"))
        .await;

    let answered = support::wait_for(support::ARRIVES, || {
        let answers = federated
            .peer_received(|stanza| is_presence(stanza, None) && stanza.attr("to") == Some(BOB));
        (answers.len() >= 2).then_some(answers)
    })
    .await
    .expect("each available resource answers");
    let mut from: Vec<&str> = answered
        .iter()
        .filter_map(|stanza| stanza.attr("from"))
        .collect();
    from.sort_unstable();
    assert_eq!(from, vec![ALICE_LAPTOP, ALICE_PHONE]);
}

#[tokio::test]
async fn a_probe_from_a_contact_of_an_account_with_no_resource_is_answered_unavailable() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "pb2"))
        .await;

    let answered = support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, Some("unavailable")) && stanza.attr("from") == Some(ALICE)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("the probe is answered");
    assert_eq!(answered.attr("to"), Some(BOB));
}

#[tokio::test]
async fn a_probe_from_a_sender_without_from_is_not_answered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let _phone = federated.online("alice", "phone", 5).await;
    // Added after the initial presence, so no login probe reaches Bob.
    contact(&federated, ALICE, BOB, "to").await;
    let mark = federated.peer_mark();

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "pb3"))
        .await;
    let barrier = federated.peer_barrier(BOB, ALICE).await;

    // Nothing at all, whatever its id: an answer would be presence the
    // server builds, without the probe's id.
    let received = federated.peer_received_besides(mark, &barrier);
    assert!(received.is_empty(), "{received:?}");
}

// --- subscriptions, inbound ----------------------------------------------------------

#[tokio::test]
async fn a_subscription_request_reaches_available_resources_without_a_roster_item() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(presence(BOB_PHONE, ALICE, PresenceType::Subscribe, "s1"))
        .await;

    let request = receive(&mut phone, "presence", "s1").await;
    assert_eq!(request.attr("type"), Some("subscribe"));
    // RFC 6121 §3.1.3: the request names the bare JIDs, and nothing is
    // added to the roster until it is approved.
    assert_eq!(request.attr("from"), Some(BOB));
    assert_eq!(request.attr("to"), Some(ALICE));
    assert_eq!(roster_item(&federated, ALICE, BOB).await, None);
}

#[tokio::test]
async fn a_subscription_request_to_an_offline_account_is_delivered_at_each_new_session() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "s2"))
        .await;
    tokio::time::sleep(support::QUIET).await;

    let mut phone = federated.online("alice", "phone", 5).await;
    receive(&mut phone, "presence", "s2").await;
    drop(phone);
    // Not answered yet: the next session receives it again.
    let mut laptop = federated.online("alice", "laptop", 5).await;
    receive(&mut laptop, "presence", "s2").await;
}

#[tokio::test]
async fn approving_a_remote_request_routes_subscribed_and_current_presence() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "s3"))
        .await;
    receive(&mut phone, "presence", "s3").await;

    let mut approval = Presence::new(PresenceType::Subscribed);
    approval.to = Some(jid(BOB));
    approval.id = Some("s3-approve".to_owned());
    send(&mut phone, approval).await;

    let routed = federated.peer_receives("s3-approve").await;
    assert_eq!(routed.attr("type"), Some("subscribed"));
    assert_eq!(routed.attr("from"), Some(ALICE));
    support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, None) && stanza.attr("from") == Some(ALICE_PHONE)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("the contact receives the account's current presence");
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item.map(|(subscription, _, _)| subscription).as_deref(),
        Some("from")
    );
}

#[tokio::test]
async fn denying_a_remote_request_routes_unsubscribed_and_forgets_it() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "s-deny"))
        .await;
    receive(&mut phone, "presence", "s-deny").await;
    assert!(support::pending_request(&federated, ALICE, BOB).await);

    let mut denial = Presence::new(PresenceType::Unsubscribed);
    denial.to = Some(jid(BOB));
    denial.id = Some("s-deny-no".to_owned());
    send(&mut phone, denial).await;

    let routed = federated.peer_receives("s-deny-no").await;
    assert_eq!(routed.attr("type"), Some("unsubscribed"));
    assert_eq!(routed.attr("from"), Some(ALICE));
    assert!(!support::pending_request(&federated, ALICE, BOB).await);
    assert_eq!(roster_item(&federated, ALICE, BOB).await, None);
}

#[tokio::test]
async fn a_request_from_a_contact_already_approved_is_answered_on_the_accounts_behalf() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "s4"))
        .await;

    let answered = support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| is_presence(stanza, Some("subscribed")))
            .into_iter()
            .next()
    })
    .await
    .expect("the request is answered");
    assert_eq!(answered.attr("from"), Some(ALICE));
    assert_eq!(answered.attr("to"), Some(BOB));
}

#[tokio::test]
async fn a_pre_approval_routes_nothing_and_approves_the_later_request() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mark = federated.peer_mark();
    let mut pre_approval = Presence::new(PresenceType::Subscribed);
    pre_approval.to = Some(jid(BOB));
    pre_approval.id = Some("pre".to_owned());
    send(&mut phone, pre_approval).await;
    // Whatever left for the pre-approval would leave before the client's
    // next stanza on the same pair.
    let barrier = federated.client_barrier(&mut phone, BOB).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(item.map(|(_, _, approved)| approved), Some(true));

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "s5"))
        .await;
    support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| is_presence(stanza, Some("subscribed")))
            .into_iter()
            .next()
    })
    .await
    .expect("the pre-approved request is approved");
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item.map(|(subscription, _, approved)| (subscription, approved)),
        Some(("from".to_owned(), false))
    );
}

#[tokio::test]
async fn subscribed_without_a_pending_request_is_ignored() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribed, "s6"))
        .await;
    federated.peer_barrier(BOB, ALICE).await;

    assert!(!holds(&client_settle(&mut phone).await, "presence", "s6"));
    assert_eq!(roster_item(&federated, ALICE, BOB).await, None);
}

#[tokio::test]
async fn unsubscribe_from_a_contact_with_from_removes_it_and_sends_unavailable() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Unsubscribe, "s7"))
        .await;

    receive(&mut phone, "presence", "s7").await;
    support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, Some("unavailable")) && stanza.attr("from") == Some(ALICE_PHONE)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("the contact receives unavailable");
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item.map(|(subscription, _, _)| subscription).as_deref(),
        Some("to")
    );
}

#[tokio::test]
async fn unsubscribed_from_a_contact_with_to_removes_the_subscription() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Unsubscribed, "s8"))
        .await;

    receive(&mut phone, "presence", "s8").await;
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item.map(|(subscription, _, _)| subscription).as_deref(),
        Some("from")
    );
}

// --- subscriptions, outbound ------------------------------------------------------------

#[tokio::test]
async fn a_subscription_request_to_a_remote_contact_is_routed_from_the_bare_jid_and_pending() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut request = Presence::new(PresenceType::Subscribe);
    request.to = Some(jid(BOB_PHONE));
    request.id = Some("out1".to_owned());
    send(&mut phone, request).await;

    let routed = federated.peer_receives("out1").await;
    assert_eq!(routed.attr("from"), Some(ALICE));
    // RFC 6121 §3.1.2: a request to a full JID is for the bare JID.
    assert_eq!(routed.attr("to"), Some(BOB));
    let item = support::wait_for_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item,
        ("none".to_owned(), Some("subscribe".to_owned()), false)
    );

    // The contact approves: the account is subscribed to it.
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribed, "out1-ok"))
        .await;
    receive(&mut phone, "presence", "out1-ok").await;
    let item = roster_item(&federated, ALICE, BOB).await;
    assert_eq!(
        item.map(|(subscription, ask, _)| (subscription, ask)),
        Some(("to".to_owned(), None))
    );
}

#[tokio::test]
async fn the_roster_set_accepts_a_remote_contact_with_s2s_enabled() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let set = Iq::from_set(
        "roster-bob",
        Roster {
            ver: None,
            items: vec![Item {
                jid: BOB.parse().expect("a bare JID"),
                name: Some("Bob".to_owned()),
                subscription: Subscription::None,
                ask: Default::default(),
                groups: Vec::new(),
            }],
        },
    );
    let reply = round_trip(&mut phone, set).await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");
    assert!(roster_item(&federated, ALICE, BOB).await.is_some());
}

#[tokio::test]
async fn removing_a_remote_contact_sends_it_unsubscribe_and_unsubscribed() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let remove = Iq::from_set(
        "roster-remove-bob",
        Roster {
            ver: None,
            items: vec![Item {
                jid: BOB.parse().expect("a bare JID"),
                name: None,
                subscription: Subscription::Remove,
                ask: Default::default(),
                groups: Vec::new(),
            }],
        },
    );
    let reply = round_trip(&mut phone, remove).await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");

    for type_ in ["unsubscribe", "unsubscribed"] {
        support::wait_for(support::ARRIVES, || {
            federated
                .peer_received(|stanza| {
                    is_presence(stanza, Some(type_)) && stanza.attr("from") == Some(ALICE)
                })
                .into_iter()
                .next()
        })
        .await
        .unwrap_or_else(|| panic!("the contact receives {type_}"));
    }
}

// --- presence, outbound -----------------------------------------------------------

#[tokio::test]
async fn initial_presence_probes_remote_contacts_and_reaches_remote_subscribers() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    contact(&federated, ALICE, "carol@home-b.test", "from").await;

    let _phone = federated.online("alice", "phone", 5).await;

    support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, Some("probe"))
                    && stanza.attr("from") == Some(ALICE)
                    && stanza.attr("to") == Some(BOB)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("a contact the account is subscribed to is probed");
    for subscriber in [BOB, "carol@home-b.test"] {
        support::wait_for(support::ARRIVES, || {
            federated
                .peer_received(|stanza| {
                    is_presence(stanza, None)
                        && stanza.attr("from") == Some(ALICE_PHONE)
                        && stanza.attr("to") == Some(subscriber)
                })
                .into_iter()
                .next()
        })
        .await
        .unwrap_or_else(|| panic!("{subscriber} receives the broadcast"));
    }
    // Carol, to whom Alice is not subscribed, is not probed.
    assert!(federated
        .peer_received(|stanza| {
            is_presence(stanza, Some("probe")) && stanza.attr("to") == Some("carol@home-b.test")
        })
        .is_empty());
}

#[tokio::test]
async fn a_chat_message_to_a_remote_contact_leaves_from_the_bound_full_jid() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut chat = xmpp_parsers::message::Message::chat(Some(jid(BOB)));
    chat.id = Some(xmpp_parsers::message::Id("out-chat".to_owned()));
    chat.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello bob".to_owned());
    send(&mut phone, chat).await;

    let routed = federated.peer_receives("out-chat").await;
    assert_eq!(routed.attr("from"), Some(full_jid(&phone).as_str()));
    assert_eq!(routed.attr("to"), Some(BOB));
}

#[tokio::test]
async fn a_message_carrying_the_dm_pin_marker_never_leaves() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mark = federated.peer_mark();

    let mut pin = xmpp_parsers::message::Message::chat(Some(jid(BOB)));
    pin.id = Some(xmpp_parsers::message::Id("out-pin".to_owned()));
    pin.payloads.push(
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
    );
    send(&mut phone, pin).await;

    let refusal = receive(&mut phone, "message", "out-pin").await;
    assert!(is_message_error(&refusal));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    let barrier = federated.client_barrier(&mut phone, BOB).await;
    assert!(!any_with_id(
        &federated.peer_received_besides(mark, &[barrier]),
        "out-pin"
    ));
}

// --- IQ ------------------------------------------------------------------------------

#[tokio::test]
async fn an_iq_request_to_a_bare_jid_is_answered_service_unavailable_for_a_contact_too() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let _phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(get(BOB_PHONE, ALICE, "iq2", ping()))
        .await;

    let refusal = federated.peer_receives("iq2").await;
    assert_eq!(refusal.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_contacts_request_to_an_unbound_full_jid_is_answered_service_unavailable() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;

    federated
        .from_peer(get(BOB_PHONE, "alice@example.com/gone", "iq3", ping()))
        .await;

    let refusal = federated.peer_receives("iq3").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
}

#[tokio::test]
async fn a_response_from_another_resource_does_not_complete_a_remote_request() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 5).await;

    let mark = federated.peer_mark();
    federated
        .from_peer(get(BOB_PHONE, ALICE_PHONE, "iq4", ping()))
        .await;
    receive(&mut phone, "iq", "iq4").await;
    // The laptop answers in the phone's place: the answer is dropped.
    send(
        &mut laptop,
        Iq::Result {
            from: None,
            to: Some(jid(BOB_PHONE)),
            id: "iq4".to_owned(),
            payload: None,
        },
    )
    .await;
    // The laptop's answer would leave before its next stanza on the pair.
    let barrier = federated.client_barrier(&mut laptop, BOB).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(!any_with_id(&received, "iq4"), "{received:?}");
}

// --- D1 at the effect boundary ------------------------------------------------------

#[tokio::test]
async fn a_domain_the_replica_no_longer_admits_reaches_no_account() {
    let pki = Pki::new();
    let federated = federated_with(
        &pki,
        Adjust {
            effect_snapshot: Some(|spec| {
                spec.allow = Vec::new();
                spec.private = Vec::new();
                spec.deny = vec![PEER];
            }),
            ..Adjust::default()
        },
    )
    .await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    // Added after the initial presence, so no login probe or broadcast
    // reaches Bob.
    contact(&federated, ALICE, BOB, "both").await;
    let mark = federated.peer_mark();

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "d1",
            "queued before",
        ))
        .await;
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "d1-probe"))
        .await;
    federated
        .from_peer(get(BOB_PHONE, ALICE_PHONE, "d1-iq", ping()))
        .await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Groupchat,
            "d1-groupchat",
            "not a room",
        ))
        .await;

    // The account side answers nothing at all for the domain, not even
    // the groupchat every admitted sender is refused: after a quiet
    // period, a ping on the same pair is the barrier.
    tokio::time::sleep(support::QUIET).await;
    let barrier = federated.peer_ping_barrier(BOB_PHONE).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
    let seen = client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "d1"));
    assert!(!holds(&seen, "iq", "d1-iq"));
    assert_eq!(support::offline_count(&federated, ALICE).await, 0);
}

// --- retired accounts ------------------------------------------------------------------

#[tokio::test]
async fn a_retired_accounts_open_stream_sends_nothing_to_remote_jids() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let account = federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    waddle_server::auth::retirement::retire_account(
        federated.loopback.state.deps.app_state.db_pool.global(),
        &account,
    )
    .await
    .expect("the account is retired");
    let mark = federated.peer_mark();

    let mut chat = xmpp_parsers::message::Message::chat(Some(jid(BOB)));
    chat.id = Some(xmpp_parsers::message::Id("retired-chat".to_owned()));
    chat.bodies
        .insert(xmpp_parsers::message::Lang::new(), "still here".to_owned());
    send(&mut phone, chat).await;
    let refusal = receive(&mut phone, "message", "retired-chat").await;
    assert!(is_message_error(&refusal), "{refusal:?}");

    // A presence update would be broadcast to Bob, a contact with `both`.
    let mut update = Presence::new(PresenceType::None).with_priority(5);
    update.id = Some("retired-update".to_owned());
    send(&mut phone, update).await;
    let mut request = Presence::new(PresenceType::Subscribe);
    request.to = Some(jid("carol@home-b.test"));
    request.id = Some("retired-subscribe".to_owned());
    send(&mut phone, request).await;
    // The client's stanzas were handled once its own ping is answered; a
    // ping on the pair then follows anything they queued.
    client_settle(&mut phone).await;
    let barrier = federated.peer_ping_barrier(BOB_PHONE).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
    assert_eq!(
        roster_item(&federated, ALICE, "carol@home-b.test").await,
        None
    );
}

// --- S2S off ---------------------------------------------------------------------------

#[tokio::test]
async fn with_s2s_off_the_startup_prune_keeps_remote_contacts_and_drops_unknown_local_ones() {
    let state = waddle_server::test_support::websocket_state().await;
    let loopback = support::ws::Loopback::serve(state).await;
    loopback.account("alice").await;
    let storage = waddle_server::db::roster::DatabaseRosterStorage::new(
        loopback.state.deps.app_state.db_pool.global().clone(),
    );
    let account: BareJid = ALICE.parse().expect("a JID");
    for (contact, subscription) in [(BOB, "both"), ("nobody@example.com", "none")] {
        storage
            .apply_roster_change(
                &account,
                waddle_server::db::roster::RosterRowChange::Upsert(
                    waddle_server::db::roster::RosterItemRow {
                        contact_jid: contact.to_owned(),
                        name: None,
                        subscription: subscription.to_owned(),
                        ask: None,
                        approved: false,
                        groups: Vec::new(),
                    },
                ),
            )
            .await
            .expect("a roster item");
    }

    waddle_server::test_support::reconcile_local_accounts(&loopback.state)
        .await
        .expect("the reconciliation runs");

    let bob: BareJid = BOB.parse().expect("a JID");
    let kept = storage
        .get_roster_item(&account, &bob)
        .await
        .expect("the roster is readable")
        .expect("the remote contact stays, dormant while S2S is off");
    assert_eq!(kept.subscription, "both");
    let nobody: BareJid = "nobody@example.com".parse().expect("a JID");
    assert!(storage
        .get_roster_item(&account, &nobody)
        .await
        .expect("the roster is readable")
        .is_none());
}

#[tokio::test]
async fn with_s2s_off_a_remote_contact_is_refused_and_no_roster_state_changes() {
    let state = waddle_server::test_support::websocket_state().await;
    let loopback = support::ws::Loopback::serve(state).await;
    loopback.account("alice").await;
    let mut phone = loopback.client("alice", "phone").await;

    let set = Iq::from_set(
        "roster-bob",
        Roster {
            ver: None,
            items: vec![Item {
                jid: BOB.parse().expect("a bare JID"),
                name: None,
                subscription: Subscription::None,
                ask: Default::default(),
                groups: Vec::new(),
            }],
        },
    );
    let reply = round_trip(&mut phone, set).await;
    assert!(
        matches!(&reply, Iq::Error { error, .. } if error.defined_condition == xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound),
        "{reply:?}"
    );

    let mut request = Presence::new(PresenceType::Subscribe);
    request.to = Some(jid(BOB));
    request.id = Some("off-subscribe".to_owned());
    send(&mut phone, request).await;
    let refusal = receive(&mut phone, "presence", "off-subscribe").await;
    assert_eq!(refusal.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("remote-server-not-found")
    );
    let account: BareJid = ALICE.parse().expect("a JID");
    let contact_jid: BareJid = BOB.parse().expect("a JID");
    let storage = waddle_server::db::roster::DatabaseRosterStorage::new(
        loopback.state.deps.app_state.db_pool.global().clone(),
    );
    assert!(storage
        .get_roster_item(&account, &contact_jid)
        .await
        .expect("the roster is readable")
        .is_none());
    let _ = Jid::from(contact_jid);
}
