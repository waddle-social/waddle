//! XEP-0030 §3.1: a disco#info `node` the server does not know is answered
//! with `item-not-found`; a known node is mirrored in the result.

use minidom::Element;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";
const COMMANDS: &str = "http://jabber.org/protocol/commands";

const PASSWORD: &str = "unknown-node-fixture-password";
const PEP_NODE: &str = "urn:example:pep-node";

fn query_to(namespace: &str, target: &str, node: Option<&str>, id: &str) -> Iq {
    let mut payload = Element::builder("query", namespace);
    if let Some(node) = node {
        payload = payload.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Iq::Get {
        from: None,
        to: Some(target.parse().expect("discovery target")),
        id: id.to_owned(),
        payload: payload.build(),
    }
}

fn info_query(node: Option<&str>, id: &str) -> Iq {
    query_to(DISCO_INFO_NS, DOMAIN, node, id)
}

/// The item-not-found error `reply` must be, coming from `target`.
fn assert_item_not_found_from(reply: &Iq, target: &str) {
    assert_eq!(reply.from().map(|jid| jid.as_str()), Some(target));
    let Iq::Error { error, .. } = reply else {
        panic!("{target} must answer item-not-found: {reply:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::ItemNotFound);
}

fn assert_empty_items_result(reply: &Iq, target: &str) {
    assert_eq!(reply.from().map(|jid| jid.as_str()), Some(target));
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("{target} must answer with an items result: {reply:?}")
    };
    assert!(query.is("query", DISCO_ITEMS_NS));
    assert_eq!(query.children().count(), 0, "{target}: {query:?}");
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

async fn connect(server: &TestServer, user: &str) -> WsXmppClient {
    let password = if user == "admin" {
        server.fixed_account_password().to_owned()
    } else {
        PASSWORD.to_owned()
    };
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, &password, user)
        .await
        .expect("SCRAM/bind")
}

#[tokio::test]
async fn account_disco_info_node_is_mirrored_or_item_not_found() {
    let server = TestServer::start();
    let mut client = connect(&server, "admin").await;
    let account = format!("admin@{DOMAIN}");

    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="pub"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="{PEP_NODE}"><item id="1"><x xmlns="urn:example:payload"/></item></publish></pubsub></iq>"#
        ))
        .await
        .expect("publish");
    client
        .recv_matching(|frame| frame.contains("id='pub'") && frame.contains("result"))
        .await
        .expect("publish result");

    // A hosted PEP node: pubsub/leaf identity, node mirrored.
    let reply = round_trip(
        &mut client,
        query_to(DISCO_INFO_NS, &account, Some(PEP_NODE), "a1"),
    )
    .await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("a hosted PEP node must be answered: {reply:?}")
    };
    assert_eq!(query.attr("node"), Some(PEP_NODE));
    assert!(query.children().any(|c| c.name() == "identity"
        && c.attr("category") == Some("pubsub")
        && c.attr("type") == Some("leaf")));

    // A node the account does not host is item-not-found.
    let reply = round_trip(
        &mut client,
        query_to(DISCO_INFO_NS, &account, Some("urn:example:missing"), "a2"),
    )
    .await;
    assert_item_not_found_from(&reply, &account);

    // No node: the account's own root answer, with no node attribute.
    let reply = round_trip(&mut client, query_to(DISCO_INFO_NS, &account, None, "a3")).await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("account root disco#info must be answered: {reply:?}")
    };
    assert_eq!(query.attr("node"), None);

    client.close().await.expect("close");
}

#[tokio::test]
async fn upload_and_calls_services_host_no_nodes() {
    let server = TestServer::start();
    let mut client = connect(&server, "admin").await;
    let upload = format!("upload.{DOMAIN}");
    let calls = format!("calls.{DOMAIN}");

    for (target, prefix) in [(&upload, "u"), (&calls, "c")] {
        // disco#info with a node: item-not-found from the queried service.
        let reply = round_trip(
            &mut client,
            query_to(
                DISCO_INFO_NS,
                target,
                Some("urn:example:node"),
                &format!("{prefix}1"),
            ),
        )
        .await;
        assert_item_not_found_from(&reply, target);

        // disco#info without a node still answers.
        let reply = round_trip(
            &mut client,
            query_to(DISCO_INFO_NS, target, None, &format!("{prefix}2")),
        )
        .await;
        assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");

        // disco#items: an empty result describing the queried entity, not
        // the server's service list.
        let reply = round_trip(
            &mut client,
            query_to(DISCO_ITEMS_NS, target, None, &format!("{prefix}3")),
        )
        .await;
        assert_empty_items_result(&reply, target);

        // disco#items with a node: item-not-found.
        let reply = round_trip(
            &mut client,
            query_to(
                DISCO_ITEMS_NS,
                target,
                Some("urn:example:node"),
                &format!("{prefix}4"),
            ),
        )
        .await;
        assert_item_not_found_from(&reply, target);
    }
    client.close().await.expect("close");
}

#[tokio::test]
async fn community_disco_nodes_must_exist_and_errors_come_from_the_service() {
    let server = TestServer::start();
    let mut client = connect(&server, "admin").await;
    let community = format!("community.{DOMAIN}");
    let feed = waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED;

    let reply = round_trip(
        &mut client,
        query_to(DISCO_INFO_NS, &community, Some("urn:example:missing"), "m1"),
    )
    .await;
    assert_item_not_found_from(&reply, &community);

    let reply = round_trip(
        &mut client,
        query_to(
            DISCO_ITEMS_NS,
            &community,
            Some("urn:example:missing"),
            "m2",
        ),
    )
    .await;
    assert_item_not_found_from(&reply, &community);

    let reply = round_trip(
        &mut client,
        query_to(DISCO_ITEMS_NS, &community, Some(feed), "m3"),
    )
    .await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = reply
    else {
        panic!("a hosted community node must be answered: {reply:?}")
    };
    assert_eq!(query.attr("node"), Some(feed));
    client.close().await.expect("close");
}

#[tokio::test]
async fn push_disco_items_node_must_exist() {
    let server = TestServer::start();
    let mut client = connect(&server, "admin").await;
    let push = format!("push.{DOMAIN}");

    let reply = round_trip(
        &mut client,
        query_to(DISCO_ITEMS_NS, &push, Some("urn:example:missing"), "p1"),
    )
    .await;
    assert_item_not_found_from(&reply, &push);

    let reply = round_trip(
        &mut client,
        query_to(DISCO_ITEMS_NS, &push, Some(COMMANDS), "p2"),
    )
    .await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");
    client.close().await.expect("close");
}
