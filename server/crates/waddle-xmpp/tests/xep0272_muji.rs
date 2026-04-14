#![recursion_limit = "256"]
//! XEP-0272: Muji dedicated suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};
use minidom::Element;
use waddle_xmpp::xep::{extract_muji_from_presence, parse_muji_element, MujiStatus};
use xmpp_parsers::presence::Presence;

#[tokio::test]
async fn xep0272_muji_presence_update_broadcast_in_muc() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "muji@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "muji@muc.localhost", "Bob")
        .await
        .expect("bob join");

    alice
        .send(
            "<presence to='muji@muc.localhost/Alice' xmlns='jabber:client'>\
                <muji xmlns='urn:xmpp:muji:0' payload-owner='sfu'>\
                    <service jid='sfu.localhost'/>\
                    <preparing/>\
                </muji>\
            </presence>",
        )
        .await
        .expect("send muji presence");

    let bob_response = bob
        .read_until("urn:xmpp:muji:0", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives muji update");

    assert!(
        bob_response.contains("from='muji@muc.localhost/Alice'")
            || bob_response.contains("from=\"muji@muc.localhost/Alice\""),
        "Expected routed room presence from Alice, got: {}",
        bob_response
    );
    assert!(
        bob_response.contains("<preparing")
            || bob_response.contains("<preparing/>")
            || bob_response.contains("<preparing "),
        "Expected Muji status payload in broadcast, got: {}",
        bob_response
    );
}

#[test]
fn xep0272_validation_wrong_namespace_is_rejected() {
    let elem = Element::builder("muji", "jabber:client").build();
    assert!(parse_muji_element(&elem).is_none());
}

#[test]
fn xep0272_validation_invalid_embedded_content_is_ignored() {
    let xml = "<presence xmlns='jabber:client'>\
                 <muji xmlns='urn:xmpp:muji:0'>\
                   <active/>\
                   <content xmlns='urn:xmpp:jingle:1' creator='invalid' name='audio'/>\
                 </muji>\
               </presence>";
    let presence =
        Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

    let muji = extract_muji_from_presence(&presence).expect("muji payload parses");
    assert_eq!(muji.statuses, vec![MujiStatus::Active]);
    assert!(muji.contents.is_empty());
}
