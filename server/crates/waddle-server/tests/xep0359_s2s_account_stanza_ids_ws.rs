//! XEP-0359 for messages from remote accounts across S2S (ADR-0020 D7;
//! gate G18): a `<stanza-id/>` this deployment could have assigned, one
//! whose `by` names the recipient account, another address at a served
//! domain or nothing readable, is removed from a remote sender's message
//! before it is delivered or stored, since only this deployment assigns
//! those and a client takes one in its account's name for its archive's
//! (§3: an entity that assigns stanza ids deletes one whose `by` it would
//! set). A `<stanza-id/>` a remote entity assigned in its own name is
//! left, as a routing entity leaves it (§3). Through the production `/ws`
//! router, the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{federated, message, receive, Federated, Pki, BOB, BOB_PHONE};
use waddle_xmpp::pending_delivery::PendingPayload;
use xmpp_parsers::message::{Message, MessageType};

const ALICE: &str = "alice@example.com";
const NS_SID: &str = "urn:xmpp:sid:0";

fn stanza_id(by: &str, id: &str) -> Element {
    Element::builder("stanza-id", NS_SID)
        .attr(minidom::rxml::xml_ncname!("by").to_owned(), by)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .build()
}

/// A chat message from Bob's phone to Alice carrying ids in the name of
/// Alice's account, her server, an unreadable address, and Bob's own
/// account.
fn with_ids(id: &str) -> Message {
    let mut sent = message(BOB_PHONE, ALICE, MessageType::Chat, id, "hello");
    sent.payloads.push(stanza_id(ALICE, "forged-account"));
    sent.payloads
        .push(stanza_id("example.com", "forged-server"));
    sent.payloads.push(stanza_id("@@", "forged-unreadable"));
    sent.payloads.push(stanza_id(BOB, "bobs-archive"));
    sent
}

/// The ids of the `<stanza-id/>` elements of `payloads`.
fn ids<'a>(payloads: impl Iterator<Item = &'a Element>) -> Vec<String> {
    payloads
        .filter(|payload| payload.is("stanza-id", NS_SID))
        .filter_map(|payload| payload.attr("id").map(ToOwned::to_owned))
        .collect()
}

async fn stored(federated: &Federated) -> Vec<Message> {
    let account: jid::BareJid = ALICE.parse().expect("an account JID");
    federated
        .loopback
        .state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&account)
        .await
        .expect("the offline store is readable")
        .into_iter()
        .filter_map(|row| match row.payload {
            PendingPayload::Transient(message) => Some(*message),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn stanza_ids_in_the_deployments_name_are_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated.from_peer(with_ids("ids-online")).await;

    let delivered = receive(&mut phone, "message", "ids-online").await;
    assert_eq!(
        ids(delivered.children()),
        vec!["bobs-archive".to_owned()],
        "{delivered:?}"
    );
}

#[tokio::test]
async fn stanza_ids_in_the_deployments_name_are_removed_before_storage() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    federated.from_peer(with_ids("ids-offline")).await;

    let held = support::routing::wait_for_async(support::ARRIVES, || async {
        let rows = stored(&federated).await;
        (!rows.is_empty()).then_some(rows)
    })
    .await
    .expect("the message is stored for Alice's next session");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(
        ids(held[0].payloads.iter()),
        vec!["bobs-archive".to_owned()],
        "{held:?}"
    );
}
