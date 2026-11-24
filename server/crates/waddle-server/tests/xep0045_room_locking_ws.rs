//! XEP-0045 §10.1 room creation and locking over WebSocket.
//!
//! A room its first occupant creates through MUC presence is "locked":
//! the creator is its owner and only the creator may enter until the owner
//! unlocks it by submitting an instant-room request (§10.1.2) or a
//! reserved-room configuration form (§10.1.3). Entry by anyone else while
//! the room is locked is `<item-not-found/>` (§7.2.10). A cancelled initial
//! configuration, or the owner leaving before configuring, destroys the
//! room. Rooms the service provisions already configured (Waddle channels)
//! are never locked.

use std::time::Duration;

use tokio::sync::Mutex;
use waddle_ws_test_support as ws_common;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::{self, Element};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const NS_CLIENT: &str = "jabber:client";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const NODE_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
const ROOMCONFIG: &str = "http://jabber.org/protocol/muc#roomconfig";

// Each test spawns a fresh waddle-server binary; serialize so the harness
// temp-port slot does not race when several tests run in parallel.
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn attr(name: &'static minidom::rxml::NcNameStr) -> minidom::rxml::NcName {
    name.to_owned()
}

async fn send(client: &mut WsXmppClient, element: Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send stanza");
}

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

fn join_presence(room: &str, nick: &str) -> Element {
    Element::builder("presence", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/{nick}"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build()
}

fn status_codes(presence: &Element) -> Vec<String> {
    presence
        .get_child("x", NS_MUC_USER)
        .map(|x| {
            x.children()
                .filter(|child| child.name() == "status")
                .filter_map(|status| status.attr("code").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Join `room` as `nick` and return the self-presence, draining the join
/// sequence through the room subject.
async fn join_expecting_self_presence(
    client: &mut WsXmppClient,
    room: &str,
    nick: &str,
) -> Element {
    send(client, join_presence(room, nick)).await;
    let occupant = format!("{room}/{nick}");
    let frame = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "presence" && has_from(&element, &occupant))
        })
        .await
        .expect("self-presence");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join sequence ends with the subject");
    frame.parse().expect("self-presence element")
}

fn has_from(element: &Element, from: &str) -> bool {
    element.attr("from") == Some(from)
}

/// Join `room` as `nick` and return the presence error the service answers.
async fn join_expecting_error(client: &mut WsXmppClient, room: &str, nick: &str) -> Element {
    send(client, join_presence(room, nick)).await;
    let frame = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("type") == Some("error")
            })
        })
        .await
        .expect("presence error");
    frame.parse().expect("presence error element")
}

/// Join until the service answers with something other than a presence error
/// (the registry may still be tearing the destroyed room down), and return
/// that answer, or the last error after a bounded number of attempts.
async fn join_until_created(client: &mut WsXmppClient, room: &str, nick: &str) -> Element {
    let mut last = None;
    for _ in 0..30 {
        send(client, join_presence(room, nick)).await;
        let occupant = format!("{room}/{nick}");
        let frame = client
            .recv_matching(|frame| {
                frame.parse::<Element>().is_ok_and(|element| {
                    element.name() == "presence" && has_from(&element, &occupant)
                })
            })
            .await
            .expect("answer to the join");
        let element: Element = frame.parse().expect("join answer");
        if element.attr("type") != Some("error") {
            return element;
        }
        last = Some(element);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    last.expect("at least one join answer")
}

fn assert_item_not_found(error_presence: &Element, room: &str, nick: &str) {
    let xml = String::from(error_presence);
    assert_eq!(
        error_presence.attr("from"),
        Some(format!("{room}/{nick}").as_str()),
        "{xml}"
    );
    let error = error_presence
        .children()
        .find(|child| child.name() == "error")
        .unwrap_or_else(|| panic!("presence error without <error>: {xml}"));
    assert_eq!(error.attr("type"), Some("cancel"), "{xml}");
    assert!(
        error
            .children()
            .any(|child| child.name() == "item-not-found" && child.ns() == NS_STANZAS),
        "XEP-0045 §7.2.10: a locked room refuses entry with <item-not-found/>: {xml}"
    );
}

fn owner_iq(id: &str, room: &str, kind: &str, query_child: Option<Element>) -> Element {
    let mut query = Element::builder("query", NS_MUC_OWNER);
    if let Some(child) = query_child {
        query = query.append(child);
    }
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), kind)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(query.build())
        .build()
}

fn data_form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    let mut form =
        Element::builder("x", NS_DATA).attr(attr(minidom::rxml::xml_ncname!("type")), form_type);
    if form_type == "submit" && !fields.is_empty() {
        form = form.append(field("FORM_TYPE", "hidden", ROOMCONFIG));
    }
    for (var, value) in fields {
        form = form.append(field(var, "text-single", value));
    }
    form.build()
}

fn field(var: &str, field_type: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), field_type)
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

async fn recv_iq(client: &mut WsXmppClient, id: &str) -> Element {
    let frame = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "iq" && element.attr("id") == Some(id))
        })
        .await
        .expect("IQ response");
    frame.parse().expect("IQ response element")
}

async fn assert_iq_result(client: &mut WsXmppClient, id: &str) {
    let response = recv_iq(client, id).await;
    assert_eq!(
        response.attr("type"),
        Some("result"),
        "{}",
        String::from(&response)
    );
}

/// Create `room` as `admin` and return once the creator holds its owner
/// self-presence with status 201.
async fn create_locked_room(admin: &mut WsXmppClient, room: &str) {
    let self_presence = join_expecting_self_presence(admin, room, ADMIN).await;
    let codes = status_codes(&self_presence);
    assert!(
        codes.iter().any(|code| code == "201"),
        "XEP-0045 §10.1.1: the creator's presence acknowledges creation with 201: {}",
        String::from(&self_presence)
    );
    let item = self_presence
        .get_child("x", NS_MUC_USER)
        .and_then(|x| x.get_child("item", NS_MUC_USER))
        .expect("owner item");
    assert_eq!(item.attr("affiliation"), Some("owner"));
}

fn unique_room() -> String {
    format!("lock-{}@muc.{DOMAIN}", uuid::Uuid::new_v4())
}

struct Fixture {
    _server: TestServer,
    admin: WsXmppClient,
    bob: WsXmppClient,
}

async fn fixture() -> (Fixture, tokio::sync::MutexGuard<'static, ()>) {
    let guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let admin = connect(&server, ADMIN, &admin_pass, "lock-admin").await;
    let bob = connect(&server, BOB, &bob_pass, "lock-bob").await;
    (
        Fixture {
            _server: server,
            admin,
            bob,
        },
        guard,
    )
}

/// §10.1.1: while the room awaits its owner's configuration, everyone but the
/// creator is refused entry with `<item-not-found/>`; §10.1.2: the instant
/// room request unlocks it.
#[tokio::test]
async fn locked_room_refuses_other_users_until_instant_room_request() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    create_locked_room(&mut f.admin, &room).await;

    let refusal = join_expecting_error(&mut f.bob, &room, BOB).await;
    assert_item_not_found(&refusal, &room, BOB);

    // The refused joiner is no occupant: the owner hears nothing of it.
    send(
        &mut f.admin,
        owner_iq("instant1", &room, "set", Some(data_form("submit", &[]))),
    )
    .await;
    assert_iq_result(&mut f.admin, "instant1").await;

    let bob_presence = join_expecting_self_presence(&mut f.bob, &room, BOB).await;
    assert!(
        !status_codes(&bob_presence).iter().any(|code| code == "201"),
        "only the creator is told the room was created: {}",
        String::from(&bob_presence)
    );
}

/// §10.1.3: the owner requests the configuration form and submits it; the
/// room is then unlocked and the submitted configuration applies.
#[tokio::test]
async fn reserved_room_configuration_form_submit_unlocks_the_room() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    create_locked_room(&mut f.admin, &room).await;

    send(&mut f.admin, owner_iq("form1", &room, "get", None)).await;
    let form_reply = recv_iq(&mut f.admin, "form1").await;
    assert_eq!(form_reply.attr("type"), Some("result"));
    assert!(
        form_reply
            .get_child("query", NS_MUC_OWNER)
            .and_then(|query| query.get_child("x", NS_DATA))
            .is_some_and(|form| form.attr("type") == Some("form")),
        "the owner is sent the configuration form: {}",
        String::from(&form_reply)
    );

    // Requesting the form does not unlock the room.
    let refusal = join_expecting_error(&mut f.bob, &room, BOB).await;
    assert_item_not_found(&refusal, &room, BOB);

    send(
        &mut f.admin,
        owner_iq(
            "config1",
            &room,
            "set",
            Some(data_form(
                "submit",
                &[("muc#roomconfig_roomname", "Reserved Hall")],
            )),
        ),
    )
    .await;
    assert_iq_result(&mut f.admin, "config1").await;

    join_expecting_self_presence(&mut f.bob, &room, BOB).await;
}

/// §10.1.3: cancelling the initial configuration destroys the room and the
/// owner receives unavailable presence carrying `<destroy/>`; the address is
/// free to be created afresh.
#[tokio::test]
async fn cancelling_initial_configuration_destroys_the_room() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    create_locked_room(&mut f.admin, &room).await;

    send(
        &mut f.admin,
        owner_iq("cancel1", &room, "set", Some(data_form("cancel", &[]))),
    )
    .await;
    let unavailable = f
        .admin
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence"
                    && element.attr("type") == Some("unavailable")
                    && has_from(&element, &format!("{room}/{ADMIN}"))
            })
        })
        .await
        .expect("owner receives unavailable presence");
    let unavailable: Element = unavailable.parse().expect("unavailable presence");
    assert!(
        unavailable
            .get_child("x", NS_MUC_USER)
            .is_some_and(|x| x.get_child("destroy", NS_MUC_USER).is_some()),
        "the unavailable presence carries <destroy/>: {}",
        String::from(&unavailable)
    );
    assert_iq_result(&mut f.admin, "cancel1").await;

    // The room is gone: the next entrant creates it and is told so.
    let bob_presence = join_until_created(&mut f.admin, &room, ADMIN).await;
    assert!(
        status_codes(&bob_presence).iter().any(|code| code == "201"),
        "a destroyed room is created anew by its next entrant: {}",
        String::from(&bob_presence)
    );
}

/// §10.1.3: an owner who becomes unavailable before configuring the room
/// destroys it.
#[tokio::test]
async fn owner_leaving_before_configuring_destroys_the_room() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    create_locked_room(&mut f.admin, &room).await;

    send(
        &mut f.admin,
        Element::builder("presence", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/{ADMIN}"),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "unavailable")
            .build(),
    )
    .await;
    f.admin
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("type") == Some("unavailable")
            })
        })
        .await
        .expect("owner sees its own departure");

    // While the room exists it is locked and bob is told it is not found;
    // once it is destroyed bob, who may not create rooms, is told so instead.
    let mut condition = String::new();
    for _ in 0..30 {
        let answer = join_expecting_error(&mut f.bob, &room, BOB).await;
        condition = answer
            .children()
            .find(|child| child.name() == "error")
            .and_then(|error| error.children().find(|c| c.name() != "text"))
            .map(|c| c.name().to_owned())
            .unwrap_or_default();
        if condition != "item-not-found" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        condition, "not-allowed",
        "the room was destroyed when its owner left: nothing is left to enter"
    );
}

/// The creator's other resources are the owner too and may enter while the
/// room is locked.
#[tokio::test]
async fn creator_may_enter_from_another_resource_while_locked() {
    let guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_pass = server.fixed_account_password().to_string();
    let mut first = connect(&server, ADMIN, &admin_pass, "lock-first").await;
    let mut second = connect(&server, ADMIN, &admin_pass, "lock-second").await;
    let room = unique_room();
    create_locked_room(&mut first, &room).await;

    join_expecting_self_presence(&mut second, &room, ADMIN).await;
    drop(guard);
}

/// §10.2: cancelling a later configuration leaves the room as it is.
#[tokio::test]
async fn cancelling_a_later_configuration_leaves_the_room_untouched() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    create_locked_room(&mut f.admin, &room).await;
    send(
        &mut f.admin,
        owner_iq("instant1", &room, "set", Some(data_form("submit", &[]))),
    )
    .await;
    assert_iq_result(&mut f.admin, "instant1").await;
    join_expecting_self_presence(&mut f.bob, &room, BOB).await;
    while f
        .admin
        .recv_timeout(Duration::from_millis(200))
        .await
        .is_ok()
    {}

    send(
        &mut f.admin,
        owner_iq("cancel2", &room, "set", Some(data_form("cancel", &[]))),
    )
    .await;
    assert_iq_result(&mut f.admin, "cancel2").await;

    // Nobody is removed and no configuration change (status 104) is sent.
    let observed = f.bob.recv_timeout(Duration::from_millis(700)).await;
    if let Ok(frame) = observed {
        assert!(
            !frame.contains("unavailable")
                && !frame.contains("code='104'")
                && !frame.contains("code=\"104\""),
            "a cancelled configuration changes nothing: {frame}"
        );
    }
    let message = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room.as_str())
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .append(
            Element::builder("body", NS_CLIENT)
                .append("still here")
                .build(),
        )
        .build();
    send(&mut f.admin, message).await;
    f.bob
        .recv_matching(|frame| frame.contains("still here"))
        .await
        .expect("the room keeps working after a cancelled configuration");
}

/// A room the service provisions (a Waddle channel) is already configured: its
/// first occupant is not told of a creation and others enter at once.
#[tokio::test]
async fn service_provisioned_channel_is_never_locked() {
    let (mut f, _guard) = fixture().await;
    let create = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "mk-channel")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .append(
            Element::builder("command", NS_COMMANDS)
                .attr(
                    minidom::rxml::xml_ncname!("node").to_owned(),
                    NODE_CHANNELS_CREATE,
                )
                .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
                .append(
                    Element::builder("x", NS_DATA)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                        .append(field("FORM_TYPE", "hidden", NODE_CHANNELS_CREATE))
                        .append(field("name", "text-single", "provisioned"))
                        .build(),
                )
                .build(),
        )
        .build();
    send(&mut f.admin, create).await;
    let created = recv_iq(&mut f.admin, "mk-channel").await;
    let channel = created
        .get_child("command", NS_COMMANDS)
        .and_then(|command| command.get_child("x", NS_DATA))
        .and_then(|form| {
            form.children()
                .find(|field| field.attr("var") == Some("channel_jid"))
        })
        .and_then(|field| field.get_child("value", NS_DATA))
        .map(Element::text)
        .unwrap_or_else(|| panic!("channel_jid in {}", String::from(&created)));

    let admin_presence = join_expecting_self_presence(&mut f.admin, &channel, ADMIN).await;
    assert!(
        !status_codes(&admin_presence)
            .iter()
            .any(|code| code == "201"),
        "a provisioned channel is not created by its first occupant: {}",
        String::from(&admin_presence)
    );
    let bob_presence = join_expecting_self_presence(&mut f.bob, &channel, BOB).await;
    assert!(
        !String::from(&bob_presence).contains("item-not-found"),
        "{}",
        String::from(&bob_presence)
    );
}

fn disco_iq(id: &str, to: &str, ns: &'static str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(Element::builder("query", ns).build())
        .build()
}

/// §10.1.1: a locked room does not (yet) exist for anyone but its owner, so
/// the service does not list it and describes it only to the owner; once
/// unlocked it is both listed and described to everyone.
#[tokio::test]
async fn locked_room_is_neither_listed_nor_described_to_other_users() {
    let (mut f, _guard) = fixture().await;
    let room = unique_room();
    let service = format!("muc.{DOMAIN}");
    create_locked_room(&mut f.admin, &room).await;

    send(&mut f.bob, disco_iq("items1", &service, NS_DISCO_ITEMS)).await;
    let items = recv_iq(&mut f.bob, "items1").await;
    assert!(
        !String::from(&items).contains(room.as_str()),
        "a locked room is not listed: {}",
        String::from(&items)
    );

    send(&mut f.bob, disco_iq("info1", &room, NS_DISCO_INFO)).await;
    let info = recv_iq(&mut f.bob, "info1").await;
    assert_eq!(info.attr("type"), Some("error"), "{}", String::from(&info));
    assert!(
        info.children()
            .find(|child| child.name() == "error")
            .is_some_and(|error| error
                .children()
                .any(|c| c.name() == "item-not-found" && c.ns() == NS_STANZAS)),
        "disco#info of a locked room is item-not-found for others: {}",
        String::from(&info)
    );

    send(&mut f.admin, disco_iq("info2", &room, NS_DISCO_INFO)).await;
    let owner_info = recv_iq(&mut f.admin, "info2").await;
    assert_eq!(owner_info.attr("type"), Some("result"));

    send(
        &mut f.admin,
        owner_iq("instant1", &room, "set", Some(data_form("submit", &[]))),
    )
    .await;
    assert_iq_result(&mut f.admin, "instant1").await;

    send(&mut f.bob, disco_iq("items2", &service, NS_DISCO_ITEMS)).await;
    let items = recv_iq(&mut f.bob, "items2").await;
    assert!(
        String::from(&items).contains(room.as_str()),
        "an unlocked room is listed: {}",
        String::from(&items)
    );
    send(&mut f.bob, disco_iq("info3", &room, NS_DISCO_INFO)).await;
    assert_eq!(
        recv_iq(&mut f.bob, "info3").await.attr("type"),
        Some("result")
    );
}
