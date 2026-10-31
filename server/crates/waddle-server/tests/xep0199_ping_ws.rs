//! XEP-0199 remote routing rejection over a real WebSocket and SCRAM login.

use std::time::Duration;

use jid::Jid;
use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0410::{interpret_self_ping_response, SelfPingResult};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const DOMAIN: &str = "localhost";

async fn round_trip(client: &mut WsXmppClient, request: Iq) -> Iq {
    let id = request.id().to_owned();
    let mut bytes = Vec::new();
    Element::from(request)
        .write_to(&mut bytes)
        .expect("serialize typed IQ");
    client
        .send(&String::from_utf8(bytes).expect("IQ UTF-8"))
        .await
        .expect("send IQ on authenticated WebSocket");
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "iq" && element.attr("id") == Some(id.as_str())
            })
        })
        .await
        .expect("correlated IQ response");
    Iq::try_from(frame.parse::<Element>().expect("response XML")).expect("typed response IQ")
}

#[tokio::test]
async fn foreign_occupant_ping_is_denied_and_same_socket_still_answers_local_ping() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "remote-ping-regression",
    )
    .await
    .expect("normal SCRAM authentication and resource binding");
    let sender: Jid = client
        .full_jid
        .as_deref()
        .expect("bound client JID")
        .parse()
        .expect("typed bound JID");
    let target: Jid = "room@muc.remote.test/admin"
        .parse()
        .expect("foreign occupant");
    let payload: Element = Ping.into();
    let id = "foreign-occupant<&";
    let response = round_trip(
        &mut client,
        Iq::Get {
            from: Some(sender.clone()),
            to: Some(target.clone()),
            id: id.into(),
            payload: payload.clone(),
        },
    )
    .await;
    assert_eq!(response.id(), id);
    assert_eq!(response.from(), Some(&target));
    assert_eq!(response.to(), Some(&sender));
    let Iq::Error {
        error,
        payload: echoed,
        ..
    } = &response
    else {
        panic!("remote ping must not produce a pong: {response:?}")
    };
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::NotAllowed);
    assert_eq!(
        error.by,
        Some(DOMAIN.parse().expect("home error generator"))
    );
    assert_eq!(echoed.as_ref(), Some(&payload));
    assert_eq!(
        interpret_self_ping_response(&response),
        SelfPingResult::Disconnected
    );

    let mut local = Iq::from_get("same-socket-local-ping", Ping);
    *local.from_mut() = Some(sender.clone());
    *local.to_mut() = Some(DOMAIN.parse().expect("home server JID"));
    let response = round_trip(&mut client, local).await;
    assert!(matches!(&response, Iq::Result { payload: None, .. }));
    assert_eq!(
        response.from(),
        Some(&DOMAIN.parse().expect("home server JID"))
    );
    assert_eq!(response.to(), Some(&sender));
    client.close().await.expect("close WebSocket");
}
