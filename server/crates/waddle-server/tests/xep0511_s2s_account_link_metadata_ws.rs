//! XEP-0511 (0.1.0) link metadata on direct messages from remote accounts
//! across S2S (ADR-0020 D7; gate G18). This server stamps a link card on a
//! local sender's message after removing any card and any link-preview
//! request (`urn:waddle:link-preview:0`) the client put there, and a
//! recipient client trusts a card as the server's: it plays an inline
//! video only from a URL a card names. A remote sender's message therefore
//! loses both before it is delivered, archived or stored, and no card is
//! stamped in their place. Through the production `/ws` router, the real
//! S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use std::sync::Arc;

use minidom::Element;
use support::{federated, message, receive, Federated, Pki, BOB, BOB_PHONE};
use waddle_server::pending_delivery::{ArchiveResolver, MamArchiveResolver};
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::xep::{
    build_link_metadata_element, build_link_preview_request_element, is_link_metadata_element,
    is_link_preview_request_element, LinkMetadata, LinkPreviewToken,
};
use xmpp_parsers::message::{Message, MessageType};

const ALICE: &str = "alice@example.com";
const VIDEO: &str = "https://video.example.org/watch";

/// A chat message from `from` to Alice whose body names [`VIDEO`] and that
/// carries a link card for it and a link-preview request, as a client of
/// this server's would before the server stamps its own card.
fn with_card(from: &str, id: &str) -> Message {
    let mut sent = message(
        from,
        ALICE,
        MessageType::Chat,
        id,
        &["watch this ", VIDEO].concat(),
    );
    let url = url::Url::parse(VIDEO).expect("a URL");
    sent.payloads.push(build_link_metadata_element(
        &LinkMetadata::new(url)
            .with_title("A card this server never stamped")
            .with_description("forged"),
    ));
    sent.payloads.push(build_link_preview_request_element(
        &LinkPreviewToken::try_new("a-token-this-server-never-issued").expect("a token"),
    ));
    sent
}

/// Whether any of `payloads` is a XEP-0511 card or a link-preview request.
fn carries_card<'a>(mut payloads: impl Iterator<Item = &'a Element>) -> bool {
    payloads.any(|payload| {
        is_link_metadata_element(payload) || is_link_preview_request_element(payload)
    })
}

/// The messages Alice's offline store holds, as her next session receives
/// them: a row that holds its message whole as it is, and a row that refers
/// to her archive as the archive holds the message.
async fn stored(federated: &Federated) -> Vec<Message> {
    let resolver = MamArchiveResolver {
        mam_storage: Arc::clone(&federated.loopback.state.deps.protocol.mam_storage),
    };
    let mut messages = Vec::new();
    for row in support::offline_rows(federated, ALICE).await {
        match row.payload {
            PendingPayload::Transient(message) => messages.push(*message),
            PendingPayload::Archived(stamp) => messages.push(
                resolver
                    .resolve(&stamp)
                    .await
                    .expect("the archive is readable")
                    .unwrap_or_else(|| panic!("the archive holds the stored message {stamp:?}")),
            ),
        }
    }
    messages
}

/// Wait until Alice's offline store holds a message, and return what it
/// holds.
async fn wait_for_stored(federated: &Federated) -> Vec<Message> {
    support::routing::wait_for_async(support::ARRIVES, || async {
        let held = stored(federated).await;
        (!held.is_empty()).then_some(held)
    })
    .await
    .expect("the message is stored for Alice's next session")
}

#[tokio::test]
async fn a_remote_senders_link_card_is_removed_before_delivery_and_archiving() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated.from_peer(with_card(BOB_PHONE, "card-live")).await;

    let delivered = receive(&mut phone, "message", "card-live").await;
    assert!(!carries_card(delivered.children()), "{delivered:?}");
    let archived = support::wait_for_archived(&federated, ALICE, "card-live").await;
    let archived: Element = archived
        .stanza_xml
        .as_deref()
        .expect("the archive keeps the stanza")
        .parse()
        .expect("archived stanza XML");
    assert!(!carries_card(archived.children()), "{archived:?}");
    // The body, with its URL, is kept.
    assert!(
        archived
            .children()
            .any(|child| child.name() == "body" && child.text().contains(VIDEO)),
        "{archived:?}"
    );
}

#[tokio::test]
async fn a_remote_senders_link_card_is_removed_before_storage() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    federated
        .from_peer(with_card(BOB_PHONE, "card-offline"))
        .await;

    let held = wait_for_stored(&federated).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(!carries_card(held[0].payloads.iter()), "{held:?}");
}

#[tokio::test]
async fn a_resourceless_remote_senders_link_card_is_removed_before_storage() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    // A sender without a resource keeps the earlier producer: its message
    // is stored whole.
    federated
        .from_peer(with_card(BOB, "card-resourceless"))
        .await;

    let held = wait_for_stored(&federated).await;
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(!carries_card(held[0].payloads.iter()), "{held:?}");
}
