//! XEP-0115 (1.6.0) entity capabilities between local accounts and remote
//! contacts across S2S (ADR-0020 D7; gate G18): the presence a remote
//! contact receives in answer to its probe carries the resource's own
//! `<c/>` element, never one the server rebuilt, and the contact's
//! `disco#info` query for the advertised node reaches that resource's
//! full JID and is answered by the client (§6.2). Through the production
//! `/ws` router, the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    contact, federated, get, is_presence, jid, presence, receive, send, Pki, BOB, BOB_PHONE,
};
use waddle_xmpp::disco::DISCO_INFO_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ALICE: &str = "alice@example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NODE: &str = "https://client.example";
const VER: &str = "QgayPKawpkPSDYmwT/WM94uAlu0=";

fn caps() -> Element {
    Element::builder("c", NS_CAPS)
        .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE)
        .attr(minidom::rxml::xml_ncname!("ver").to_owned(), VER)
        .build()
}

#[tokio::test]
async fn a_probe_answer_carries_the_resources_own_caps_and_the_caps_query_reaches_it() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.loopback.client("alice", "phone").await;
    let mut available = Presence::new(PresenceType::None);
    available.id = Some("caps-presence".to_owned());
    available.payloads.push(caps());
    send(&mut phone, available).await;
    contact(&federated, ALICE, BOB, "from").await;

    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "caps-probe"))
        .await;
    let answer = support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, None)
                    && stanza.attr("from") == Some(ALICE_PHONE)
                    && stanza.attr("to") == Some(BOB)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("the probe is answered");
    let advertised = answer
        .children()
        .find(|child| child.is("c", NS_CAPS))
        .expect("the resource's caps");
    assert_eq!(advertised.attr("node"), Some(NODE));
    assert_eq!(advertised.attr("ver"), Some(VER));

    let query = Element::builder("query", DISCO_INFO_NS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            [NODE, "#", VER].concat(),
        )
        .build();
    federated
        .from_peer(get(BOB_PHONE, ALICE_PHONE, "caps-query", query))
        .await;
    let request = receive(&mut phone, "iq", "caps-query").await;
    assert_eq!(request.attr("from"), Some(BOB_PHONE));
    send(
        &mut phone,
        Iq::Result {
            from: None,
            to: Some(jid(BOB_PHONE)),
            id: "caps-query".to_owned(),
            payload: Some(Element::builder("query", DISCO_INFO_NS).build()),
        },
    )
    .await;
    let answered = federated.peer_receives("caps-query").await;
    assert_eq!(answered.attr("type"), Some("result"));
}
