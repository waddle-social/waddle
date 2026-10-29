//! XEP-0280 destination rejection over real WebSockets and SCRAM sessions.

use std::time::Duration;

use jid::Jid;
use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::carbons::CARBONS_NS;
use xmpp_parsers::carbons::{Disable, Enable, Sent};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize stanza");
    client
        .send(&String::from_utf8(bytes).expect("stanza UTF-8"))
        .await
        .expect("send stanza");
}

async fn round_trip(client: &mut WsXmppClient, request: Iq) -> Iq {
    let id = request.id().to_owned();
    send_element(client, request.into()).await;
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "iq" && element.attr("id") == Some(id.as_str())
            })
        })
        .await
        .expect("correlated IQ response");
    Iq::try_from(frame.parse::<Element>().expect("IQ XML")).expect("typed IQ response")
}

fn full_jid(client: &WsXmppClient) -> Jid {
    client
        .full_jid
        .as_deref()
        .expect("bound client")
        .parse()
        .expect("bound JID")
}

fn assert_remote_error(response: &Iq, request: &Iq, bound: &Jid) {
    assert_eq!(response.id(), request.id());
    assert_eq!(response.from(), request.to());
    assert_eq!(response.to(), Some(bound));
    let Iq::Error { error, payload, .. } = response else {
        panic!("foreign Carbons must be denied: {response:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(error.by, Some(DOMAIN.parse().expect("home domain")));
    let Iq::Set {
        payload: original, ..
    } = request
    else {
        panic!("Carbons control")
    };
    assert_eq!(payload.as_ref(), Some(original));
}

async fn assert_sent_carbon(
    sender: &mut WsXmppClient,
    peer: &mut WsXmppClient,
    observer: &mut WsXmppClient,
    id: &str,
) {
    let mut message = Message::chat(Some(full_jid(peer)));
    message.id = Some(Id(id.to_owned()));
    message
        .bodies
        .insert(Lang::new(), "carbons routing regression".into());
    send_element(sender, message.into()).await;
    let original = peer
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "message" && element.attr("id") == Some(id))
        })
        .await
        .expect("actual peer received original message");
    let original = Message::try_from(original.parse::<Element>().expect("original XML"))
        .expect("original message");
    assert_eq!(original.from, Some(full_jid(sender)));
    let frame = observer
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.get_child("sent", CARBONS_NS).is_some())
        })
        .await
        .expect("enabled resource receives actual carbon");
    let copy =
        Message::try_from(frame.parse::<Element>().expect("carbon XML")).expect("carbon message");
    assert_eq!(copy.from, Some(Jid::from(full_jid(observer).to_bare())));
    assert_eq!(copy.to, Some(full_jid(observer)));
    let sent = copy
        .payloads
        .into_iter()
        .find_map(|payload| Sent::try_from(payload).ok())
        .expect("typed sent carbon");
    assert_eq!(sent.forwarded.message.id, original.id);
    assert_eq!(sent.forwarded.message.from, original.from);
    assert_eq!(sent.forwarded.message.to, original.to);
    assert_eq!(sent.forwarded.message.bodies, original.bodies);
}

#[tokio::test]
async fn foreign_carbons_controls_are_denied_without_disabling_real_carbon_delivery() {
    let server = TestServer::start_with_extra_accounts(&[("peer", "carbons-fixture-password")]);
    let mut observer = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "carbons-observer",
    )
    .await
    .expect("observer authenticated and bound");
    let bound = full_jid(&observer);
    let target: Jid = "bob@remote.test/phone".parse().expect("foreign target");
    let mut foreign_enable = Iq::from_set("foreign-enable<&", Enable);
    *foreign_enable.to_mut() = Some(target.clone());
    let response = round_trip(&mut observer, foreign_enable.clone()).await;
    assert_remote_error(&response, &foreign_enable, &bound);

    let enabled = round_trip(&mut observer, Iq::from_set("local-enable", Enable)).await;
    assert!(matches!(enabled, Iq::Result { payload: None, .. }));
    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "carbons-sender",
    )
    .await
    .expect("sender authenticated and bound");
    let mut peer = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "peer",
        "carbons-fixture-password",
        "carbons-peer",
    )
    .await
    .expect("peer authenticated and bound");
    assert_sent_carbon(
        &mut sender,
        &mut peer,
        &mut observer,
        "before-foreign-disable",
    )
    .await;

    let mut foreign_disable = Iq::from_set("foreign-disable<&", Disable);
    *foreign_disable.to_mut() = Some(target);
    *foreign_disable.from_mut() = Some(
        "forged@elsewhere.test/resource"
            .parse()
            .expect("forged sender"),
    );
    let response = round_trip(&mut observer, foreign_disable.clone()).await;
    assert_remote_error(&response, &foreign_disable, &bound);
    assert_sent_carbon(
        &mut sender,
        &mut peer,
        &mut observer,
        "after-foreign-disable",
    )
    .await;

    let disabled = round_trip(&mut observer, Iq::from_set("local-disable", Disable)).await;
    assert!(matches!(disabled, Iq::Result { payload: None, .. }));
    let response = round_trip(&mut observer, foreign_enable.clone()).await;
    assert_remote_error(&response, &foreign_enable, &bound);
    let enabled = round_trip(&mut observer, Iq::from_set("local-enable-again", Enable)).await;
    assert!(matches!(enabled, Iq::Result { payload: None, .. }));
    let disabled = round_trip(&mut observer, Iq::from_set("local-disable-again", Disable)).await;
    assert!(matches!(disabled, Iq::Result { payload: None, .. }));
    peer.close().await.expect("close peer");
    sender.close().await.expect("close sender");
    observer.close().await.expect("close observer");
}
