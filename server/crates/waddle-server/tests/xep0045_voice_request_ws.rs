//! XEP-0045 §7.13 voice requests and §8.6 approval, over the WebSocket
//! transport.
//!
//! - a visitor's `muc#request` form to the room reaches every moderator as a
//!   voice approval form from the room;
//! - a moderator's submitted approval grants voice with the ordinary role
//!   change presence;
//! - a non-moderator's approval, a malformed request, a request from a
//!   non-occupant and a second request in quick succession are refused.

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
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_XDATA: &str = "jabber:x:data";
const FORM_TYPE_REQUEST: &str = "http://jabber.org/protocol/muc#request";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

struct Fixture {
    _server: TestServer,
    admin: WsXmppClient,
    alice: WsXmppClient,
    bob: WsXmppClient,
    room: String,
}

fn attr(name: &str) -> minidom::rxml::NcName {
    minidom::rxml::NcName::try_from(name).expect("static attribute name")
}

async fn send(client: &mut WsXmppClient, element: Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send stanza");
}

fn field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(attr("var"), var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn voice_form(fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_XDATA)
        .attr(attr("type"), "submit")
        .append(field("FORM_TYPE", FORM_TYPE_REQUEST));
    for (var, value) in fields {
        form = form.append(field(var, value));
    }
    form.build()
}

fn message_to_room(room: &str, form: Element) -> Element {
    Element::builder("message", NS_CLIENT)
        .attr(attr("to"), room)
        .attr(attr("id"), uuid::Uuid::new_v4().to_string())
        .append(form)
        .build()
}

async fn drain(client: &mut WsXmppClient) -> Vec<Element> {
    let mut frames = Vec::new();
    while let Ok(frame) = client.recv_timeout(Duration::from_millis(300)).await {
        frames.push(frame.parse::<Element>().expect("frame is XML"));
    }
    frames
}

fn form_values(form: &Element, var: &str) -> Vec<String> {
    form.children()
        .filter(|field| field.name() == "field" && field.attr("var") == Some(var))
        .flat_map(|field| field.children())
        .filter(|child| child.name() == "value")
        .map(|value| value.text())
        .collect()
}

fn is_error_with(frame: &Element, condition: &str) -> bool {
    frame.name() == "message"
        && frame.attr("type") == Some("error")
        && frame
            .children()
            .find(|child| child.name() == "error")
            .is_some_and(|error| error.children().any(|c| c.name() == condition))
}

fn presence_role(frame: &Element, from: &str) -> Option<String> {
    if frame.name() != "presence" || frame.attr("from") != Some(from) {
        return None;
    }
    let user = frame
        .children()
        .find(|child| child.name() == "x" && child.ns() == NS_MUC_USER)?;
    user.children()
        .find(|child| child.name() == "item")
        .and_then(|item| item.attr("role"))
        .map(str::to_owned)
}

async fn join(client: &mut WsXmppClient, room: &str, nick: &str) {
    send(
        client,
        Element::builder("presence", NS_CLIENT)
            .attr(attr("to"), format!("{room}/{nick}"))
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

async fn fixture() -> Fixture {
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let connect = |user: &'static str, pass: String, resource: &'static str| {
        let url = server.ws_url();
        async move {
            WsXmppClient::connect_and_auth(&url, DOMAIN, user, &pass, resource)
                .await
                .expect("connect and auth")
        }
    };
    let mut admin = connect(ADMIN, admin_pass, "voice-admin").await;
    let mut alice = connect(ALICE, alice_pass, "voice-alice").await;
    let mut bob = connect(BOB, bob_pass, "voice-bob").await;
    let room = format!("voice-request-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    join(&mut admin, &room, ADMIN).await;
    ws_common::iq::unlock_created_room(&mut admin, &room).await;
    let config_id = format!("moderated-{}", uuid::Uuid::new_v4());
    send(
        &mut admin,
        Element::builder("iq", NS_CLIENT)
            .attr(attr("type"), "set")
            .attr(attr("id"), config_id.as_str())
            .attr(attr("to"), room.as_str())
            .append(
                Element::builder("query", NS_MUC_OWNER)
                    .append(
                        Element::builder("x", NS_XDATA)
                            .attr(attr("type"), "submit")
                            .append(field(
                                "FORM_TYPE",
                                "http://jabber.org/protocol/muc#roomconfig",
                            ))
                            .append(field("muc#roomconfig_moderatedroom", "1"))
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    admin
        .recv_matching(|frame| frame.contains(&config_id))
        .await
        .expect("room configured");
    join(&mut alice, &room, ALICE).await;
    join(&mut bob, &room, BOB).await;
    drain(&mut admin).await;
    drain(&mut alice).await;
    drain(&mut bob).await;
    Fixture {
        _server: server,
        admin,
        alice,
        bob,
        room,
    }
}

#[tokio::test]
async fn xep_0045_visitor_voice_request_reaches_moderators_as_an_approval_form() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.alice,
        message_to_room(&f.room, voice_form(&[("muc#role", "participant")])),
    )
    .await;

    let approval = f
        .admin
        .recv_matching(|frame| frame.contains(FORM_TYPE_REQUEST))
        .await
        .expect("moderator receives the approval form");
    let message: Element = approval.parse().expect("message");
    assert_eq!(message.attr("from"), Some(f.room.as_str()));
    assert_eq!(
        message.attr("to"),
        Some(format!("{ADMIN}@{DOMAIN}/voice-admin").as_str())
    );
    assert_ne!(message.attr("type"), Some("error"));
    let form = message
        .children()
        .find(|child| child.name() == "x" && child.ns() == NS_XDATA)
        .expect("data form");
    assert_eq!(form.attr("type"), Some("form"));
    assert_eq!(form_values(form, "FORM_TYPE"), [FORM_TYPE_REQUEST]);
    assert_eq!(form_values(form, "muc#role"), ["participant"]);
    assert_eq!(
        form_values(form, "muc#jid"),
        [format!("{ALICE}@{DOMAIN}/voice-alice")]
    );
    assert_eq!(form_values(form, "muc#roomnick"), [ALICE]);
    assert!(matches!(
        form_values(form, "muc#request_allow")
            .first()
            .map(String::as_str),
        Some("0" | "false")
    ));

    assert!(
        !drain(&mut f.alice)
            .await
            .iter()
            .any(|frame| is_error_with(frame, "bad-request")),
        "a valid request is not an error"
    );
}

#[tokio::test]
async fn xep_0045_moderator_approval_grants_voice() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.admin,
        message_to_room(
            &f.room,
            voice_form(&[
                ("muc#role", "participant"),
                ("muc#jid", &format!("{ALICE}@{DOMAIN}/voice-alice")),
                ("muc#roomnick", ALICE),
                ("muc#request_allow", "true"),
            ]),
        ),
    )
    .await;

    let from = format!("{}/{ALICE}", f.room);
    let frames = f
        .alice
        .recv_until(|frame| {
            frame.contains("role='participant'") || frame.contains("role=\"participant\"")
        })
        .await
        .expect("alice sees her role change");
    let last: Element = frames.last().expect("frame").parse().expect("presence");
    assert_eq!(presence_role(&last, &from).as_deref(), Some("participant"));

    let admin_frames = drain(&mut f.admin).await;
    assert!(
        !admin_frames.iter().any(|frame| frame.name() == "iq"),
        "the approval is a message, so it gets no IQ result: {admin_frames:?}"
    );
    assert!(
        !admin_frames
            .iter()
            .any(|frame| is_error_with(frame, "bad-request")),
        "{admin_frames:?}"
    );
}

#[tokio::test]
async fn xep_0045_declined_approval_leaves_the_visitor_without_voice() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.admin,
        message_to_room(
            &f.room,
            voice_form(&[
                ("muc#role", "participant"),
                ("muc#jid", &format!("{ALICE}@{DOMAIN}/voice-alice")),
                ("muc#roomnick", ALICE),
                ("muc#request_allow", "false"),
            ]),
        ),
    )
    .await;
    let from = format!("{}/{ALICE}", f.room);
    assert!(drain(&mut f.alice)
        .await
        .iter()
        .all(|frame| presence_role(frame, &from).is_none()));
}

#[tokio::test]
async fn xep_0045_visitor_cannot_approve_voice() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.bob,
        message_to_room(
            &f.room,
            voice_form(&[
                ("muc#role", "participant"),
                ("muc#jid", &format!("{ALICE}@{DOMAIN}/voice-alice")),
                ("muc#roomnick", ALICE),
                ("muc#request_allow", "true"),
            ]),
        ),
    )
    .await;
    let bob_frames = drain(&mut f.bob).await;
    assert!(
        bob_frames
            .iter()
            .any(|frame| is_error_with(frame, "forbidden")),
        "{bob_frames:?}"
    );
    let from = format!("{}/{ALICE}", f.room);
    assert!(drain(&mut f.alice)
        .await
        .iter()
        .all(|frame| presence_role(frame, &from).is_none()));
}

#[tokio::test]
async fn xep_0045_voice_request_for_another_role_is_bad_request() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.alice,
        message_to_room(&f.room, voice_form(&[("muc#role", "moderator")])),
    )
    .await;
    let frames = drain(&mut f.alice).await;
    assert!(
        frames
            .iter()
            .any(|frame| is_error_with(frame, "bad-request")),
        "{frames:?}"
    );
    assert!(
        drain(&mut f.admin).await.is_empty(),
        "nothing reaches moderators"
    );
}

#[tokio::test]
async fn xep_0045_voice_request_from_a_non_occupant_is_refused() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    send(
        &mut f.bob,
        Element::builder("presence", NS_CLIENT)
            .attr(attr("to"), format!("{}/{BOB}", f.room))
            .attr(attr("type"), "unavailable")
            .build(),
    )
    .await;
    drain(&mut f.bob).await;
    drain(&mut f.admin).await;
    send(
        &mut f.bob,
        message_to_room(&f.room, voice_form(&[("muc#role", "participant")])),
    )
    .await;
    let frames = drain(&mut f.bob).await;
    assert!(
        frames
            .iter()
            .any(|frame| is_error_with(frame, "not-acceptable")),
        "{frames:?}"
    );
    assert!(!drain(&mut f.admin)
        .await
        .iter()
        .any(|frame| String::from(frame).contains(FORM_TYPE_REQUEST)));
}

#[tokio::test]
async fn xep_0045_repeated_voice_requests_are_rate_limited() {
    let _guard = TEST_SERIAL.lock().await;
    let mut f = fixture().await;
    for _ in 0..2 {
        send(
            &mut f.alice,
            message_to_room(&f.room, voice_form(&[("muc#role", "participant")])),
        )
        .await;
    }
    let alice_frames = drain(&mut f.alice).await;
    assert!(
        alice_frames
            .iter()
            .any(|frame| is_error_with(frame, "resource-constraint")),
        "{alice_frames:?}"
    );
    let forms = drain(&mut f.admin)
        .await
        .iter()
        .filter(|frame| String::from(*frame).contains(FORM_TYPE_REQUEST))
        .count();
    assert_eq!(forms, 1, "only the first request reaches the moderators");
}
