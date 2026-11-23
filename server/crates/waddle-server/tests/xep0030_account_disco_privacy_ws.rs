//! XEP-0030 Security Considerations over actual WebSockets: discovery of a
//! hosted account's bare JID is answered only for the account itself and for
//! entities with a presence subscription of type `from` or `both` to it. A
//! missing account and an unauthorized asker get the same
//! `<service-unavailable/>`.

use minidom::Element;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const DOMAIN: &str = "localhost";
const PASSWORD: &str = "account-disco-fixture-password";

fn query(namespace: &str, target: &str, id: &str) -> Iq {
    Iq::Get {
        from: None,
        to: Some(target.parse().expect("discovery target")),
        id: id.to_owned(),
        payload: Element::builder("query", namespace).build(),
    }
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

fn denial(reply: Iq) -> StanzaError {
    match reply {
        Iq::Error { error, .. } => error,
        other => panic!("expected service-unavailable, got {other:?}"),
    }
}

fn assert_service_unavailable(error: &StanzaError) {
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
}

fn result_query(reply: Iq, namespace: &str) -> Element {
    match reply {
        Iq::Result {
            payload: Some(query),
            ..
        } if query.is("query", namespace) => query,
        other => panic!("expected a {namespace} result, got {other:?}"),
    }
}

/// `contact` (bob) subscribes to `owner` (alice) and alice approves, leaving
/// alice with a `from` roster item for bob.
async fn subscribe(owner: &mut WsXmppClient, contact: &mut WsXmppClient) {
    for (client, id) in [
        (&mut *owner, "roster-owner"),
        (&mut *contact, "roster-contact"),
    ] {
        client
            .send(&format!(
                r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
            ))
            .await
            .expect("roster get");
        client
            .recv_matching(|frame| frame.contains(id))
            .await
            .expect("roster result");
    }
    owner
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("owner available");
    contact
        .send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("subscribe");
    owner
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("owner receives subscribe");
    owner
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("approve");
    contact
        .recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("contact receives approval");
    // Everything above is handled once a later request is answered.
    let ping =
        r#"<iq xmlns="jabber:client" type="get" id="sync"><ping xmlns="urn:xmpp:ping"/></iq>"#;
    owner.send(ping).await.expect("ping");
    owner
        .recv_matching(|frame| frame.contains("id='sync'"))
        .await
        .expect("ping result");
}

#[tokio::test]
async fn unsubscribed_and_missing_accounts_are_indistinguishable() {
    let server =
        TestServer::start_with_extra_accounts(&[("alice", PASSWORD), ("mallory", PASSWORD)]);
    let mut mallory = connect(&server, "mallory").await;

    for namespace in [DISCO_INFO_NS, DISCO_ITEMS_NS] {
        let existing =
            denial(round_trip(&mut mallory, query(namespace, "alice@localhost", "a")).await);
        let missing =
            denial(round_trip(&mut mallory, query(namespace, "nobody@localhost", "b")).await);
        assert_service_unavailable(&existing);
        assert_service_unavailable(&missing);
        assert_eq!(existing, missing, "the two denials must be identical");
    }
    mallory.close().await.expect("close");
}

#[tokio::test]
async fn subscribed_contact_and_the_account_itself_are_answered() {
    let server = TestServer::start_with_extra_accounts(&[("alice", PASSWORD), ("bob", PASSWORD)]);
    let mut alice = connect(&server, "alice").await;
    let mut bob = connect(&server, "bob").await;
    subscribe(&mut alice, &mut bob).await;

    let info = result_query(
        round_trip(&mut bob, query(DISCO_INFO_NS, "alice@localhost", "c1")).await,
        DISCO_INFO_NS,
    );
    assert!(info
        .children()
        .any(|child| child.is("identity", DISCO_INFO_NS)
            && child.attr("category") == Some("pubsub")
            && child.attr("type") == Some("pep")));
    result_query(
        round_trip(&mut bob, query(DISCO_ITEMS_NS, "alice@localhost", "c2")).await,
        DISCO_ITEMS_NS,
    );

    // The subscription is one way: alice has no `from` item at bob.
    let reverse = denial(round_trip(&mut alice, query(DISCO_INFO_NS, "bob@localhost", "c3")).await);
    assert_service_unavailable(&reverse);

    // The account itself is always authorized.
    result_query(
        round_trip(&mut alice, query(DISCO_INFO_NS, "alice@localhost", "c4")).await,
        DISCO_INFO_NS,
    );
    result_query(
        round_trip(&mut alice, query(DISCO_ITEMS_NS, "alice@localhost", "c5")).await,
        DISCO_ITEMS_NS,
    );

    alice.close().await.expect("close alice");
    bob.close().await.expect("close bob");
}
