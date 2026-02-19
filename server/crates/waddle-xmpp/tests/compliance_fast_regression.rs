//! Fast compliance regression tests.
//!
//! These tests are intentionally narrow and fast. They are not a replacement
//! for CAAS/XEP-0479 runs; they provide early PR feedback for high-risk
//! routing and session-establishment behavior.

mod common;

use std::time::Duration;

use common::{encode_sasl_plain, extract_bound_jid, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

fn init_test() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        common::install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();
    });
}

async fn establish_authenticated_session(
    client: &mut RawXmppClient,
    server: &TestServer,
    username: &str,
    resource: &str,
    advertise_presence: bool,
    priority: i8,
) -> String {
    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send initial stream header");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read initial features");
    assert!(
        features.contains("<starttls"),
        "server should advertise STARTTLS pre-auth, got: {}",
        features
    );
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send starttls");
    client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("read proceed");
    client.clear();

    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("upgrade tls");

    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send post-tls stream header");
    let post_tls_features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read post-tls features");
    assert!(
        post_tls_features.contains("PLAIN"),
        "server should advertise SASL PLAIN after TLS, got: {}",
        post_tls_features
    );
    client.clear();

    let auth_data = encode_sasl_plain(&format!("{username}@localhost"), "token123");
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send sasl auth");
    client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("read sasl success");
    client.clear();

    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send post-auth stream header");
    let post_auth_features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read post-auth features");
    assert!(
        post_auth_features.contains("<bind"),
        "server should advertise resource binding after auth, got: {}",
        post_auth_features
    );
    client.clear();

    client
        .send(&format!(
            "<iq type='set' id='bind_1' xmlns='jabber:client'>\
            <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
                <resource>{}</resource>\
            </bind>\
        </iq>",
            resource
        ))
        .await
        .expect("send bind iq");
    let bind_result = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("read bind result");
    assert!(
        bind_result.contains("type='result'") || bind_result.contains("type=\"result\""),
        "bind result should be iq type=result, got: {}",
        bind_result
    );
    client.clear();

    let full_jid = extract_bound_jid(&bind_result).expect("bound jid should be present");

    if advertise_presence {
        client
            .send(&format!(
                "<presence xmlns='jabber:client'>\
                  <show>chat</show>\
                  <status>online-{}</status>\
                  <priority>{}</priority>\
                </presence>",
                resource, priority
            ))
            .await
            .expect("send initial presence");
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.clear();
    }

    full_jid
}

fn bare_jid(full_jid: &str) -> &str {
    full_jid.split('/').next().unwrap_or(full_jid)
}

#[tokio::test]
async fn test_starttls_auth_bind_regression_guard() {
    init_test();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr)
        .await
        .expect("connect test client");

    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send stream header");
    let features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read stream features");
    assert!(
        features.contains("<starttls"),
        "pre-auth features must include STARTTLS, got: {}",
        features
    );
    assert!(
        features.contains("<required"),
        "pre-auth STARTTLS should be required, got: {}",
        features
    );
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .expect("send starttls");
    let proceed = client
        .read_until("<proceed", DEFAULT_TIMEOUT)
        .await
        .expect("read proceed");
    assert!(
        proceed.contains("<proceed"),
        "server should return proceed for STARTTLS, got: {}",
        proceed
    );
    client.clear();

    client
        .upgrade_tls(server.tls_connector(), "localhost")
        .await
        .expect("upgrade to tls");

    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send post-tls stream");
    let post_tls_features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read post-tls features");
    assert!(
        post_tls_features.contains("PLAIN"),
        "post-tls features must include PLAIN mechanism, got: {}",
        post_tls_features
    );
    client.clear();

    let auth_data = encode_sasl_plain("startup@localhost", "token123");
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await
        .expect("send auth");
    client
        .read_until("<success", DEFAULT_TIMEOUT)
        .await
        .expect("read auth success");
    client.clear();

    client
        .send(
            "<?xml version='1.0'?>\
        <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
        to='localhost' version='1.0'>",
        )
        .await
        .expect("send post-auth stream");
    let post_auth_features = client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await
        .expect("read post-auth features");
    assert!(
        post_auth_features.contains("<bind"),
        "post-auth features must include resource binding, got: {}",
        post_auth_features
    );
    client.clear();

    client
        .send(
            "<iq type='set' id='bind_check' xmlns='jabber:client'>\
           <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
             <resource>startup-check</resource>\
           </bind>\
         </iq>",
        )
        .await
        .expect("send bind");
    let bind_result = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("read bind result");
    let jid = extract_bound_jid(&bind_result).expect("extract bound jid");
    assert!(
        jid.ends_with("/startup-check"),
        "bound jid should include requested resource, got: {}",
        jid
    );

    client.send("</stream:stream>").await.ok();
}

#[tokio::test]
async fn test_chat_to_bare_jid_fans_out_to_all_highest_priority_resources() {
    init_test();

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_phone = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_desktop = RawXmppClient::connect(server.addr).await.unwrap();

    let _alice_jid =
        establish_authenticated_session(&mut alice, &server, "alice", "sender", true, 5).await;
    let bob_phone_jid =
        establish_authenticated_session(&mut bob_phone, &server, "bob", "phone", true, 5).await;
    let bob_desktop_jid =
        establish_authenticated_session(&mut bob_desktop, &server, "bob", "desktop", true, 5).await;

    assert_eq!(bare_jid(&bob_phone_jid), "bob@localhost");
    assert_eq!(bare_jid(&bob_desktop_jid), "bob@localhost");

    bob_phone.clear();
    bob_desktop.clear();

    let body = "fast-regression-bare-fanout";
    alice
        .send(&format!(
            "<message to='bob@localhost' type='chat' id='fanout-msg-1' xmlns='jabber:client'>\
               <body>{}</body>\
             </message>",
            body
        ))
        .await
        .expect("send chat message to bare jid");

    let phone_msg = bob_phone
        .read_until(body, Duration::from_secs(2))
        .await
        .expect("phone should receive bare-jid message");
    let desktop_msg = bob_desktop
        .read_until(body, Duration::from_secs(2))
        .await
        .expect("desktop should receive bare-jid message");

    assert!(
        phone_msg.contains("alice@localhost"),
        "phone message should have alice sender, got: {}",
        phone_msg
    );
    assert!(
        desktop_msg.contains("alice@localhost"),
        "desktop message should have alice sender, got: {}",
        desktop_msg
    );

    alice.send("</stream:stream>").await.ok();
    bob_phone.send("</stream:stream>").await.ok();
    bob_desktop.send("</stream:stream>").await.ok();
}

#[tokio::test]
async fn test_iq_to_full_jid_targets_only_addressed_resource() {
    init_test();

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_phone = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_tablet = RawXmppClient::connect(server.addr).await.unwrap();

    let _alice_jid =
        establish_authenticated_session(&mut alice, &server, "alice", "sender", true, 5).await;
    let _bob_phone_jid =
        establish_authenticated_session(&mut bob_phone, &server, "bob", "phone", true, 5).await;
    let bob_tablet_jid =
        establish_authenticated_session(&mut bob_tablet, &server, "bob", "tablet", true, 5).await;

    bob_phone.clear();
    bob_tablet.clear();

    alice
        .send(&format!(
            "<iq type='get' id='iq-route-full-target' to='{}' xmlns='jabber:client'>\
               <ping xmlns='urn:xmpp:ping'/>\
             </iq>",
            bob_tablet_jid
        ))
        .await
        .expect("send iq to full jid");

    let tablet_iq = bob_tablet
        .read_until("iq-route-full-target", Duration::from_secs(2))
        .await
        .expect("targeted resource should receive iq");
    assert!(
        tablet_iq.contains("<ping"),
        "targeted iq should include ping payload, got: {}",
        tablet_iq
    );

    let phone_iq = bob_phone
        .read_until("iq-route-full-target", Duration::from_millis(750))
        .await;
    assert!(
        phone_iq.is_err(),
        "non-targeted resource should not receive full-jid iq, got: {:?}",
        phone_iq
    );

    alice.send("</stream:stream>").await.ok();
    bob_phone.send("</stream:stream>").await.ok();
    bob_tablet.send("</stream:stream>").await.ok();
}

#[tokio::test]
async fn test_directed_presence_to_bare_jid_fans_out_to_available_resources() {
    init_test();

    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_phone = RawXmppClient::connect(server.addr).await.unwrap();
    let mut bob_desktop = RawXmppClient::connect(server.addr).await.unwrap();

    let _alice_jid =
        establish_authenticated_session(&mut alice, &server, "alice", "presence", true, 5).await;
    let _bob_phone_jid =
        establish_authenticated_session(&mut bob_phone, &server, "bob", "phone", true, 5).await;
    let _bob_desktop_jid =
        establish_authenticated_session(&mut bob_desktop, &server, "bob", "desktop", true, 5).await;

    bob_phone.clear();
    bob_desktop.clear();

    let status_text = "directed-presence-fanout-check";
    alice
        .send(&format!(
            "<presence to='bob@localhost' xmlns='jabber:client'>\
               <show>chat</show>\
               <status>{}</status>\
               <priority>0</priority>\
             </presence>",
            status_text
        ))
        .await
        .expect("send directed presence");

    let phone_presence = bob_phone
        .read_until(status_text, Duration::from_secs(2))
        .await
        .expect("phone should receive directed presence");
    let desktop_presence = bob_desktop
        .read_until(status_text, Duration::from_secs(2))
        .await
        .expect("desktop should receive directed presence");

    assert!(
        phone_presence.contains("alice@localhost"),
        "phone presence should have alice sender, got: {}",
        phone_presence
    );
    assert!(
        desktop_presence.contains("alice@localhost"),
        "desktop presence should have alice sender, got: {}",
        desktop_presence
    );

    alice.send("</stream:stream>").await.ok();
    bob_phone.send("</stream:stream>").await.ok();
    bob_desktop.send("</stream:stream>").await.ok();
}
