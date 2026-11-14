//! XEP-0198 handled count for stanzas the server refuses at parsing, through
//! the production WebSocket server.
//!
//! XEP-0198 §4 defines a handled stanza as one the server has accepted
//! responsibility for, including by processing it directly. A well-formed
//! stanza refused at parsing (an address outside the identity profile, or a
//! payload the typed parser rejects) has been processed: it was answered
//! with an error, or dropped where RFC 6120 forbids an error. It must count,
//! or the client would replay it after resumption and every later
//! acknowledgement would name the wrong stanza. Input that is not a stanza
//! must not count.

use waddle_ws_test_support::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn exchange(client: &mut WsXmppClient, frame: &str, id: &str) -> String {
    client.send(frame).await.expect("send stanza");
    let double = format!("id=\"{id}\"");
    let single = format!("id='{id}'");
    client
        .recv_matching(|response| response.contains(&double) || response.contains(&single))
        .await
        .expect("correlated response")
}

fn is_error(response: &str) -> bool {
    response.contains("type=\"error\"") || response.contains("type='error'")
}

#[tokio::test]
async fn stanzas_refused_at_parsing_count_as_handled() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "sm",
    )
    .await
    .expect("authenticated client");
    client
        .send(r#"<enable xmlns="urn:xmpp:sm:3"/>"#)
        .await
        .expect("enable stream management");
    client
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("stream management enabled");

    // 1: an ordinary request.
    let pong = exchange(
        &mut client,
        r#"<iq type="get" id="sm-ping" to="localhost"><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "sm-ping",
    )
    .await;
    assert!(!is_error(&pong), "{pong}");
    // 2: an IQ request refused by the identity profile, answered with
    // <jid-malformed/>.
    let refused = exchange(
        &mut client,
        r#"<iq type="get" id="sm-case" to="Admin@localhost"><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "sm-case",
    )
    .await;
    assert!(
        is_error(&refused) && refused.contains("jid-malformed"),
        "{refused}"
    );
    // 3: a message refused by the identity profile.
    let bounced = exchange(
        &mut client,
        r#"<message id="sm-msg" to="Bob@localhost" type="chat"><body>hi</body></message>"#,
        "sm-msg",
    )
    .await;
    assert!(is_error(&bounced), "{bounced}");
    // 4: an IQ result refused by the identity profile; RFC 6120 §8.3.1
    // forbids answering it, so it is dropped.
    client
        .send(r#"<iq type="result" id="sm-result" to="Admin@localhost"/>"#)
        .await
        .expect("send IQ result");
    // 5: a well-formed IQ request the typed parser rejects.
    let rejected = exchange(
        &mut client,
        r#"<iq type="get" id="sm-empty" to="localhost"/>"#,
        "sm-empty",
    )
    .await;
    assert!(is_error(&rejected), "{rejected}");

    // Not stanzas: an unknown root element and an `iq` element outside the
    // `jabber:client` namespace.
    client
        .send(r#"<unknown xmlns="urn:example:not-a-stanza"/>"#)
        .await
        .expect("send unknown element");
    client
        .send(r#"<iq xmlns="urn:example:not-a-stanza" type="get" id="sm-foreign"/>"#)
        .await
        .expect("send foreign element");

    client
        .send(r#"<r xmlns="urn:xmpp:sm:3"/>"#)
        .await
        .expect("request acknowledgement");
    let ack = client
        .recv_matching(|frame| frame.contains("<a "))
        .await
        .expect("acknowledgement");
    assert!(
        ack.contains("h=\"5\"") || ack.contains("h='5'"),
        "every stanza and nothing else counts as handled: {ack}"
    );
    client.close().await.expect("close");
}
