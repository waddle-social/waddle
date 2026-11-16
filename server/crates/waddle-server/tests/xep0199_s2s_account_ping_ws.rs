//! XEP-0199 (2.0.1) client-to-client pings across S2S (§4.4; ADR-0020 D7;
//! gate G18): a remote contact's ping to a local resource's full JID
//! reaches that resource when the account shares presence with the
//! contact, and only that resource's answer leaves; a ping to the bare JID
//! is answered by the server on the account's behalf with
//! `service-unavailable` (RFC 6121 §8.5.2.1.3), as is a ping from anybody
//! the account does not share presence with. Through the production `/ws`
//! router, the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{
    client_settle, contact, error_condition, federated, get, holds, jid, ping, receive, send, Pki,
    BOB, BOB_PHONE, MALLORY,
};
use xmpp_parsers::iq::Iq;

const ALICE: &str = "alice@example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";

#[tokio::test]
async fn a_contacts_ping_to_a_full_jid_reaches_the_resource_and_its_answer_leaves() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    contact(&federated, ALICE, BOB, "from").await;

    federated
        .from_peer(get(BOB_PHONE, ALICE_PHONE, "ping-1", ping()))
        .await;
    let request = receive(&mut phone, "iq", "ping-1").await;
    assert_eq!(request.attr("from"), Some(BOB_PHONE));
    assert_eq!(request.attr("type"), Some("get"));

    send(
        &mut phone,
        Iq::Result {
            from: None,
            to: Some(jid(BOB_PHONE)),
            id: "ping-1".to_owned(),
            payload: None,
        },
    )
    .await;
    let answered = federated.peer_receives("ping-1").await;
    assert_eq!(answered.attr("type"), Some("result"));
    assert_eq!(answered.attr("from"), Some(ALICE_PHONE));
    assert_eq!(answered.attr("to"), Some(BOB_PHONE));
}

#[tokio::test]
async fn a_ping_to_the_bare_jid_is_answered_on_the_accounts_behalf() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    contact(&federated, ALICE, BOB, "both").await;

    federated
        .from_peer(get(BOB_PHONE, ALICE, "ping-2", ping()))
        .await;

    let answered = federated.peer_receives("ping-2").await;
    assert_eq!(answered.attr("type"), Some("error"));
    assert_eq!(answered.attr("from"), Some(ALICE));
    assert_eq!(
        error_condition(&answered).as_deref(),
        Some("service-unavailable")
    );
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(!holds(&client_settle(&mut phone).await, "iq", "ping-2"));
}

#[tokio::test]
async fn a_strangers_ping_to_a_full_jid_is_answered_as_if_no_resource_matched() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(get(MALLORY, ALICE_PHONE, "ping-3", ping()))
        .await;

    let answered = federated.peer_receives("ping-3").await;
    assert_eq!(
        error_condition(&answered).as_deref(),
        Some("service-unavailable")
    );
    federated.peer_barrier(MALLORY, ALICE).await;
    assert!(!holds(&client_settle(&mut phone).await, "iq", "ping-3"));
}
