//! XEP-0359 for messages from remote accounts across S2S (ADR-0020 D7;
//! gate G18): a `<stanza-id/>` this deployment could have assigned, one
//! whose `by` names the recipient account, another address at a served
//! domain or nothing readable, is removed from a remote sender's message
//! before it is delivered or stored, since only this deployment assigns
//! those and a client takes one in its account's name for its archive's
//! (§3: an entity that assigns stanza ids deletes one whose `by` it would
//! set). A `<stanza-id/>` a remote entity assigned in the name of a third
//! party is left, as a routing entity leaves it (§3), but the one in the
//! sender's own account name is its archive's: XEP-0313 §4.1.1 withholds
//! it from the recipient. A stored message is the recipient's archived
//! copy, which carries the one id in the account's name that this
//! deployment assigned: its archive's. Through the
//! production `/ws` router, the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use std::sync::Arc;

use minidom::Element;
use support::{federated, jid, message, receive, Federated, Pki, BOB, BOB_PHONE};
use waddle_server::pending_delivery::{ArchiveResolver, MamArchiveResolver};
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp_core::xep0359::StanzaId;
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

/// The messages Alice's offline store holds, as her next session receives
/// them: a row that holds its message whole as it is, and a row that refers
/// to her archive as the archive holds the message, with the archive's id
/// the row names.
async fn stored(federated: &Federated) -> Vec<(Option<StanzaId>, Message)> {
    let resolver = MamArchiveResolver {
        mam_storage: Arc::clone(&federated.loopback.state.deps.protocol.mam_storage),
    };
    let mut messages = Vec::new();
    for row in support::offline_rows(federated, ALICE).await {
        match row.payload {
            PendingPayload::Transient(message) => messages.push((None, *message)),
            PendingPayload::Archived(stamp) => {
                let archived = resolver
                    .resolve(&stamp)
                    .await
                    .expect("the archive is readable")
                    .unwrap_or_else(|| panic!("the archive holds the stored message {stamp:?}"));
                messages.push((Some(stamp), archived));
            }
        }
    }
    messages
}

#[tokio::test]
async fn stanza_ids_in_the_deployments_name_are_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated.peer_sends(with_ids("ids-online")).await;

    // The recipient pass may stamp the id Alice's archive gives the
    // message; none of the sender's ids in the deployment's name is left,
    // and neither is the one in the name of the sender's own archive.
    let delivered = receive(&mut phone, "message", "ids-online").await;
    let delivered_ids = ids(delivered.children());
    assert!(
        delivered_ids.iter().all(|id| !id.starts_with("forged-")),
        "{delivered:?}"
    );
    assert!(
        !delivered_ids.contains(&"bobs-archive".to_owned()),
        "{delivered:?}"
    );
}

#[tokio::test]
async fn stanza_ids_in_the_deployments_name_are_removed_before_storage() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    federated.peer_sends(with_ids("ids-offline")).await;

    let held = support::routing::wait_for_async(support::ARRIVES, || async {
        let rows = stored(&federated).await;
        (!rows.is_empty()).then_some(rows)
    })
    .await
    .expect("the message is stored for Alice's next session");
    assert_eq!(held.len(), 1, "{held:?}");
    let (stamp, message) = &held[0];
    let stamp = stamp
        .as_ref()
        .expect("the stored row refers to Alice's archive");
    assert_eq!(stamp.by, jid(ALICE));
    // Of the ids in the deployment's name only the one Alice's archive
    // assigned is left; Bob's own archive's id is withheld (XEP-0313 §4.1.1).
    let stamps = support::stanza_ids(message.payloads.iter());
    assert_eq!(
        stamps,
        vec![(ALICE.to_owned(), stamp.id.clone())],
        "{message:?}"
    );
}
