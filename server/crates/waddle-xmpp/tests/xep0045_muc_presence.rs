#![recursion_limit = "256"]

//! XEP-0045 MUC presence compatibility suite.

mod common;

use common::{
    establish_bound_session, init_test_env, start_server_with_channels, RawXmppClient,
    DEFAULT_TIMEOUT,
};
use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

fn contains_attr_value(xml: &str, attr: &str, value: &str) -> bool {
    xml.contains(&format!("{attr}='{value}'")) || xml.contains(&format!("{attr}=\"{value}\""))
}

fn contains_status_code(xml: &str, code: &str) -> bool {
    xml.contains(&format!("code='{code}'")) || xml.contains(&format!("code=\"{code}\""))
}

fn serialize_element(element: &Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML element");
    String::from_utf8(buf).expect("serialized XML is UTF-8")
}

#[tokio::test]
async fn nonanonymous_self_presence_includes_rawkode_real_jid() {
    init_test_env();
    let server = start_server_with_channels(&["presence-invariant"]).await;
    let mut rawkode = RawXmppClient::connect(server.addr).await.expect("connect");

    establish_bound_session(&mut rawkode, &server, "rawkode", "desktop")
        .await
        .expect("bind rawkode");

    let response = rawkode
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("rawkode self-presence");

    assert!(
        contains_attr_value(&response, "jid", "rawkode@localhost/desktop"),
        "non-anonymous MUC self-presence must expose rawkode's real JID, got: {response}"
    );
    assert!(
        contains_status_code(&response, "100"),
        "non-anonymous MUC self-presence must include status 100, got: {response}"
    );
}

#[tokio::test]
async fn nonanonymous_presence_update_includes_rawkode_real_jid() {
    init_test_env();
    let server = start_server_with_channels(&["presence-update"]).await;
    let room = "presence-update@muc.localhost";

    let mut rawkode = RawXmppClient::connect(server.addr)
        .await
        .expect("connect rawkode");
    establish_bound_session(&mut rawkode, &server, "rawkode", "desktop")
        .await
        .expect("bind rawkode");
    rawkode
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("rawkode self-presence");
    rawkode.clear();

    let mut alice = RawXmppClient::connect(server.addr)
        .await
        .expect("connect alice");
    establish_bound_session(&mut alice, &server, "alice", "laptop")
        .await
        .expect("bind alice");
    alice
        .read_until("110", DEFAULT_TIMEOUT)
        .await
        .expect("alice self-presence");
    alice.clear();
    rawkode.clear();

    let room_jid: BareJid = room.parse().expect("room JID");
    let to = room_jid
        .with_resource_str("rawkode")
        .expect("valid room nick");
    let mut update = Presence::new(PresenceType::None);
    update.to = Some(Jid::from(to));
    update.show = Some(xmpp_parsers::presence::Show::Away);
    update
        .statuses
        .insert(String::new(), "Debugging presence".to_string());
    update.payloads.push(
        Element::builder("occupant-id", "urn:xmpp:occupant-id:0")
            .attr("id", "spoofed")
            .build(),
    );
    let update_xml = serialize_element(&Element::from(update));
    rawkode
        .send(&update_xml)
        .await
        .expect("send presence update");

    let response = alice
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives rawkode presence update");

    assert!(
        response.contains("from='presence-update@muc.localhost/rawkode'")
            || response.contains("from=\"presence-update@muc.localhost/rawkode\""),
        "presence update should be from rawkode's room nick, got: {response}"
    );
    assert!(
        contains_attr_value(&response, "jid", "rawkode@localhost/desktop"),
        "non-anonymous MUC presence update must expose rawkode's real JID, got: {response}"
    );
    assert!(
        contains_status_code(&response, "100"),
        "non-anonymous MUC presence update must include status 100, got: {response}"
    );
    assert!(
        !response.contains("spoofed"),
        "presence update must replace client-supplied occupant-id, got: {response}"
    );
    assert!(
        response.contains("<show>away</show>"),
        "presence update should preserve availability show, got: {response}"
    );
}
