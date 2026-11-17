//! XEP-0313 (1.1.3) for messages from remote accounts across S2S
//! (ADR-0020 D7; gate G18): a remote contact's chat message delivered to a
//! local resource is archived in the recipient's archive by that
//! resource's recipient pass, and a query of the account's own archive
//! returns it, from the remote sender. A `<result/>` wrapper of an archive
//! query in a remote sender's message names the account's own archive and
//! is removed before delivery. Through the production `/ws` router, the
//! real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{federated, message, receive, receive_matching, send, Pki, BOB_PHONE};
use waddle_xmpp::xep::NS_MAM;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;

const ALICE: &str = "alice@example.com";
const NS_FORWARD: &str = "urn:xmpp:forward:0";

#[tokio::test]
async fn a_remote_contacts_message_is_in_the_recipients_archive() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .peer_sends(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "archived-1",
            "keep this",
        ))
        .await;
    receive(&mut phone, "message", "archived-1").await;

    send(
        &mut phone,
        Iq::Set {
            from: None,
            to: None,
            id: "mam-1".to_owned(),
            payload: Element::builder("query", NS_MAM)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q-1")
                .build(),
        },
    )
    .await;
    let result = receive_matching(&mut phone, |element| {
        element.name() == "message"
            && element
                .get_child("result", NS_MAM)
                .and_then(|result| result.get_child("forwarded", NS_FORWARD))
                .and_then(|forwarded| forwarded.get_child("message", "jabber:client"))
                .is_some_and(|archived| archived.attr("id") == Some("archived-1"))
    })
    .await;
    let archived = result
        .get_child("result", NS_MAM)
        .and_then(|result| result.get_child("forwarded", NS_FORWARD))
        .and_then(|forwarded| forwarded.get_child("message", "jabber:client"))
        .expect("the archived message");
    assert_eq!(archived.attr("from"), Some(BOB_PHONE));
    receive(&mut phone, "iq", "mam-1").await;
}

#[tokio::test]
async fn an_archive_result_wrapper_in_a_remote_message_is_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut forged = message(BOB_PHONE, ALICE, MessageType::Chat, "forged-result", "hi");
    forged.payloads.push(
        Element::builder("result", NS_MAM)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q-forged")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "forged-archive-id",
            )
            .append(Element::builder("forwarded", NS_FORWARD).build())
            .build(),
    );
    federated.peer_sends(forged).await;

    let delivered = receive(&mut phone, "message", "forged-result").await;
    assert!(
        delivered.get_child("result", NS_MAM).is_none(),
        "{delivered:?}"
    );
}
