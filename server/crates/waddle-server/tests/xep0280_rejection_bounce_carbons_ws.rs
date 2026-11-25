//! XEP-0280 section 6.1: "it is of type 'error' and it was sent in response
//! to a MESSAGE that was eligible for carbons delivery." The service-
//! unavailable bounce of a chat message to a nonexistent local account is
//! therefore carbon-copied to the sender's other Carbons-enabled resources,
//! while the sending resource receives the error itself and no carbon.

use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::carbons::CARBONS_NS;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";

async fn send(client: &mut WsXmppClient, xml: &str) {
    client.send(xml).await.expect("send stanza");
}

async fn enable_carbons(client: &mut WsXmppClient, id: &str) {
    send(
        client,
        &format!(
            "<iq xmlns='jabber:client' type='set' id='{id}'><enable xmlns='{CARBONS_NS}'/></iq>"
        ),
    )
    .await;
    client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("carbons enable result");
}

async fn connect(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("{resource}-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect");
    enable_carbons(&mut client, &format!("enable-{resource}")).await;
    client
}

fn parse(frame: &str) -> Option<Element> {
    frame.parse::<Element>().ok()
}

fn is_error_for(element: &Element, id: &str) -> bool {
    element.name() == "message"
        && element.attr("type") == Some("error")
        && element.attr("id") == Some(id)
}

#[tokio::test]
async fn bounce_to_nonexistent_account_is_received_carbon_on_other_resource() {
    let server = TestServer::start_with_extra_accounts(&[]);
    let mut sender = connect(&server, "sender").await;
    let mut sibling = connect(&server, "sibling").await;

    send(
        &mut sender,
        &format!(
            "<message xmlns='jabber:client' to='ghost@{DOMAIN}' type='chat' id='bounce-1'>\
             <body>anyone there?</body></message>"
        ),
    )
    .await;

    // The sending resource receives the error itself.
    let direct = sender
        .recv_matching_within(Duration::from_secs(10), |frame| {
            parse(frame).is_some_and(|element| is_error_for(&element, "bounce-1"))
        })
        .await
        .expect("sender receives the error bounce");
    assert!(direct.contains("service-unavailable"), "{direct}");

    // The sibling receives it wrapped in <received/>.
    let carbon = sibling
        .recv_matching_within(Duration::from_secs(10), |frame| {
            parse(frame).is_some_and(|element| {
                element.name() == "message" && element.has_child("received", CARBONS_NS)
            })
        })
        .await
        .expect("sibling receives a received carbon of the bounce");
    let wrapper = parse(&carbon).expect("carbon parses");
    let received = wrapper
        .get_child("received", CARBONS_NS)
        .expect("received element");
    let forwarded = received
        .get_child("forwarded", "urn:xmpp:forward:0")
        .expect("forwarded element");
    let inner = forwarded
        .get_child("message", "jabber:client")
        .expect("forwarded message");
    assert!(is_error_for(inner, "bounce-1"), "{carbon}");
    assert!(carbon.contains("service-unavailable"), "{carbon}");

    // The sending resource gets no carbon of its own bounce.
    let mut extra = Vec::new();
    while let Ok(frame) = sender.recv_timeout(Duration::from_millis(900)).await {
        extra.push(frame);
    }
    assert!(
        !extra.iter().any(|frame| frame.contains("<received")),
        "the sending resource must not receive a carbon: {extra:?}"
    );

    let _ = sender.close().await;
    let _ = sibling.close().await;
}

#[tokio::test]
async fn bounce_of_non_eligible_message_is_not_carbon_copied() {
    let server = TestServer::start_with_extra_accounts(&[]);
    let mut sender = connect(&server, "sender").await;
    let mut sibling = connect(&server, "sibling").await;

    // `<private/>` keeps the offered message, and so its bounce, out of
    // Carbons (section 6.1 recommended rules).
    send(
        &mut sender,
        &format!(
            "<message xmlns='jabber:client' to='ghost@{DOMAIN}' type='chat' id='bounce-2'>\
             <body>secret</body><private xmlns='{CARBONS_NS}'/></message>"
        ),
    )
    .await;
    sender
        .recv_matching_within(Duration::from_secs(10), |frame| {
            parse(frame).is_some_and(|element| is_error_for(&element, "bounce-2"))
        })
        .await
        .expect("sender receives the error bounce");
    let mut frames = Vec::new();
    while let Ok(frame) = sibling.recv_timeout(Duration::from_millis(900)).await {
        frames.push(frame);
    }
    assert!(
        !frames.iter().any(|frame| frame.contains("<received")),
        "bounce of a private message must not be carbon-copied: {frames:?}"
    );
    let _ = sender.close().await;
    let _ = sibling.close().await;
}
