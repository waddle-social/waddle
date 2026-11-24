//! XEP-0030 §3.1: a disco#info `node` the server does not know is answered
//! with `item-not-found`; a known node is mirrored in the result.

use minidom::Element;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::disco::DISCO_INFO_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";
const COMMANDS: &str = "http://jabber.org/protocol/commands";

fn info_query(node: Option<&str>, id: &str) -> Iq {
    let mut payload = Element::builder("query", DISCO_INFO_NS);
    if let Some(node) = node {
        payload = payload.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Iq::Get {
        from: None,
        to: Some(DOMAIN.parse().expect("server target")),
        id: id.to_owned(),
        payload: payload.build(),
    }
}

#[tokio::test]
async fn server_disco_info_node_handling() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "disco-node",
    )
    .await
    .expect("SCRAM/bind");

    // Unknown node: item-not-found, not the root answer.
    let reply = round_trip(
        &mut client,
        info_query(Some("urn:example:unknown#abc"), "n1"),
    )
    .await;
    let Iq::Error { error, .. } = reply else {
        panic!("unknown node must be an error: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::ItemNotFound);

    // Known node: answered, with the node mirrored.
    let reply = round_trip(&mut client, info_query(Some(COMMANDS), "n2")).await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("commands node must be answered: {reply:?}")
    };
    assert_eq!(query.attr("node"), Some(COMMANDS));

    // No node: the root answer, with no node attribute.
    let reply = round_trip(&mut client, info_query(None, "n3")).await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("root disco#info must be answered: {reply:?}")
    };
    assert_eq!(query.attr("node"), None);
    assert!(query
        .children()
        .any(|c| c.name() == "identity" && c.attr("category") == Some("server")));

    client.close().await.expect("close");
}
