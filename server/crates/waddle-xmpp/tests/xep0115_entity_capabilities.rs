#![recursion_limit = "256"]

mod common;

use common::{
    encode_sasl_plain, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0115_caps_are_advertised_in_presence_not_bind_features() {
    init_test_env();
    let server = TestServer::start().await;

    let mut sender = RawXmppClient::connect(server.addr)
        .await
        .expect("connect sender");
    let mut recipient = RawXmppClient::connect(server.addr)
        .await
        .expect("connect recipient");

    sender
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send initial stream");
    sender
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("initial features");
    sender.clear();

    sender
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("starttls");
    sender
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("proceed");
    sender.clear();
    sender
        .upgrade_tls(server.tls_connector(), &server.domain)
        .await
        .expect("upgrade tls");

    sender
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send post-tls stream");
    sender
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("post-tls features");
    sender.clear();

    let sender_token = format!("test-token-{}", uuid::Uuid::new_v4());
    let auth_data = encode_sasl_plain("sender@localhost", &sender_token);
    sender
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send auth");
    sender
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("auth success");
    sender.clear();

    sender
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await
        .expect("send bind stream");
    let bind_features = sender
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("bind features");
    assert!(
        !bind_features.contains("http://jabber.org/protocol/caps"),
        "bind stream features must not advertise XEP-0115 caps, got: {}",
        bind_features
    );
    sender.clear();

    sender
        .send(
            "<iq type='set' id='bind-1' xmlns='jabber:client'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
                    <resource>sender</resource>\
                </bind>\
            </iq>",
        )
        .await
        .expect("send bind");
    sender
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("bind response");
    sender.clear();

    let recipient_jid = establish_bound_session(&mut recipient, &server, "recipient", "reader")
        .await
        .expect("bind recipient");

    sender
        .send(&format!(
            "<presence to='{recipient_jid}' xmlns='jabber:client'>\
                <show>chat</show>\
                <status>Ready</status>\
            </presence>"
        ))
        .await
        .expect("send directed presence");

    let received_presence = recipient
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("receive directed presence");

    assert!(
        received_presence.contains("http://jabber.org/protocol/caps"),
        "directed presence must advertise XEP-0115 caps, got: {}",
        received_presence
    );
    assert!(
        received_presence.contains("node='https://waddle.social/caps'")
            || received_presence.contains("node=\"https://waddle.social/caps\""),
        "directed presence must use the Waddle caps node, got: {}",
        received_presence
    );
}
