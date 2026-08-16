//! XEP-0045 configuration and destroy websocket ordering coverage.
//!
//! These tests deliberately use the store-less websocket server fixture. It
//! exercises the standard owner-config path, where members-only enforcement
//! sends its `322` presences before the post-enforcement config audience is
//! broadcast, and the inline §10.9 destroy response path.

use tokio::sync::Mutex;
use waddle_ws_test_support as ws_common;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::{self, Element};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_XDATA: &str = "jabber:x:data";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

fn xml(element: Element) -> String {
    String::from(&element)
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    client.send(&xml(element)).await.expect("send stanza");
}

async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    send_element(
        client,
        Element::builder("presence", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/{nick}"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

fn data_form_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn owner_config_iq(room: &str, id: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append("http://jabber.org/protocol/muc#roomconfig")
                        .build(),
                )
                .build(),
        );
    for (var, value) in fields {
        form = form.append(data_form_field(var, value));
    }
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(form.build())
                .build(),
        )
        .build()
}

fn owner_destroy_iq(room: &str, id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(
                    Element::builder("destroy", NS_MUC_OWNER)
                        .append(
                            Element::builder("reason", NS_MUC_OWNER)
                                .append("ordering coverage")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

async fn expect_iq_result(client: &mut WsXmppClient, id: &str) -> String {
    let frame = client.recv().await.expect("IQ result frame");
    assert!(
        frame.contains("<iq") && frame.contains(id),
        "expected IQ result for {id}, got: {frame}"
    );
    assert!(
        frame.contains("type='result'") || frame.contains(r#"type="result""#),
        "expected successful IQ result for {id}, got: {frame}"
    );
    frame
}

fn parse_frame(frame: &str) -> Element {
    frame
        .parse::<Element>()
        .unwrap_or_else(|error| panic!("frame must parse as XML: {error}; frame={frame}"))
}

async fn observe_existing_occupant_join(
    existing_occupant: &mut WsXmppClient,
    room: &str,
    joining_nick: &str,
) {
    let frame = existing_occupant
        .recv()
        .await
        .expect("existing occupant observes join presence");
    let presence = parse_frame(&frame);
    assert_eq!(
        presence.name(),
        "presence",
        "expected join presence: {frame}"
    );
    let expected_from = format!("{room}/{joining_nick}");
    assert_eq!(
        presence.attr("from"),
        Some(expected_from.as_str()),
        "unexpected join presence: {frame}"
    );
}

fn find_descendant<'a>(root: &'a Element, name: &str, ns: &str) -> Option<&'a Element> {
    for child in root.children() {
        if child.name() == name && child.ns() == ns {
            return Some(child);
        }
        if let Some(found) = find_descendant(child, name, ns) {
            return Some(found);
        }
    }
    None
}

fn status_codes(frame: &str) -> Vec<String> {
    let element = parse_frame(frame);
    find_descendant(&element, "x", NS_MUC_USER)
        .into_iter()
        .flat_map(|x| x.children())
        .filter(|child| child.name() == "status" && child.ns() == NS_MUC_USER)
        .filter_map(|status| status.attr("code"))
        .map(ToOwned::to_owned)
        .collect()
}

fn assert_config_message(frame: &str, expected_codes: &[&str]) {
    let element = parse_frame(frame);
    assert_eq!(
        element.name(),
        "message",
        "expected config <message>: {frame}"
    );
    assert_eq!(
        status_codes(frame),
        expected_codes
            .iter()
            .map(|code| (*code).to_owned())
            .collect::<Vec<_>>(),
        "unexpected config status-code set: {frame}"
    );
}

fn assert_members_only_removal(frame: &str, expected_from: &str, self_presence: bool) {
    let element = parse_frame(frame);
    assert_eq!(
        element.name(),
        "presence",
        "expected removal presence: {frame}"
    );
    assert_eq!(
        element.attr("type"),
        Some("unavailable"),
        "members-only removal must be unavailable: {frame}"
    );
    assert_eq!(
        element.attr("from"),
        Some(expected_from),
        "unexpected sender: {frame}"
    );
    let codes = status_codes(frame);
    assert!(
        codes.iter().any(|code| code == "322"),
        "expected members-only status 322: {frame}"
    );
    if self_presence {
        assert!(
            codes.iter().any(|code| code == "110"),
            "self removal needs status 110: {frame}"
        );
    }
}

fn is_destroy_presence(frame: &str) -> bool {
    let element = parse_frame(frame);
    element.name() == "presence"
        && element.attr("type") == Some("unavailable")
        && find_descendant(&element, "destroy", NS_MUC_USER).is_some()
}

#[tokio::test]
async fn xep_0045_owner_config_returns_iq_before_initiator_104() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "config-order-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "config-order-alice").await;
    let room = format!("config-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    observe_existing_occupant_join(&mut admin, &room, ALICE).await;

    let config_id = format!("config-order-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        owner_config_iq(
            &room,
            &config_id,
            &[("muc#roomconfig_roomname", "Renamed by owner")],
        ),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &config_id).await;
    let initiator_config = admin.recv().await.expect("initiator config notification");
    assert_config_message(&initiator_config, &["104"]);

    let other_config = alice
        .recv()
        .await
        .expect("other occupant config notification");
    assert_config_message(&other_config, &["104"]);

    let _ = admin.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep_0045_unmanaged_members_only_removes_before_config_and_excludes_removed_occupant() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "members-only-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "members-only-alice").await;
    let room = format!("members-only-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    observe_existing_occupant_join(&mut admin, &room, ALICE).await;

    let config_id = format!("members-only-{}", uuid::Uuid::new_v4());
    send_element(
        &mut admin,
        owner_config_iq(&room, &config_id, &[("muc#roomconfig_membersonly", "1")]),
    )
    .await;

    let _ = expect_iq_result(&mut admin, &config_id).await;

    let expected_from = format!("{room}/{ALICE}");
    let alice_removal = alice.recv().await.expect("alice members-only removal");
    assert_members_only_removal(&alice_removal, &expected_from, true);

    let admin_removal = admin.recv().await.expect("admin sees members-only removal");
    assert_members_only_removal(&admin_removal, &expected_from, false);
    let admin_config = admin
        .recv()
        .await
        .expect("post-removal config notification");
    assert_config_message(&admin_config, &["104"]);

    let _ = admin.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep_0045_destroy_presence_precedes_owner_iq_result() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "destroy-order-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "destroy-order-alice").await;
    let room = format!("destroy-order-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    observe_existing_occupant_join(&mut admin, &room, ALICE).await;

    let destroy_id = format!("destroy-order-{}", uuid::Uuid::new_v4());
    send_element(&mut admin, owner_destroy_iq(&room, &destroy_id)).await;

    let owner_presence = admin.recv().await.expect("owner destroy presence");
    assert!(
        is_destroy_presence(&owner_presence),
        "destroy presence must precede IQ result: {owner_presence}"
    );
    let _ = expect_iq_result(&mut admin, &destroy_id).await;

    let other_presence = alice.recv().await.expect("other occupant destroy presence");
    assert!(
        is_destroy_presence(&other_presence),
        "other occupant must receive destroy presence: {other_presence}"
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
}
