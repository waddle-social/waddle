//! XEP-0421 occupant-id on XEP-0045 §7.8.2 mediated invitation relays.
//!
//! XEP-0421 §Business Rules: "The <occupant-id> element MUST be attached to
//! every message and every presence sent by a MUC." The room relays a
//! mediated invitation to the invitee, and a decline to the inviter, from its
//! own bare JID, so both carry the room's occupant-id: the same value the room
//! stamps on its other room-authored messages.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0421::{generate_room_occupant_id, OccupantIdSecret};
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const NS_OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

/// Must match `WADDLE_OCCUPANT_ID_SECRET` set by the test harness.
const HARNESS_SECRET: &str = "integration-test-occupant-id-secret-32-bytes-long";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn room_occupant_id(room: &str) -> String {
    let secret = OccupantIdSecret::try_new(HARNESS_SECRET.as_bytes().to_vec())
        .expect("harness secret meets length floor");
    let room: jid::BareJid = room.parse().expect("room bare jid");
    generate_room_occupant_id(&room, &secret)
        .as_str()
        .to_owned()
}

fn occupant_ids(frame: &str) -> Vec<String> {
    let element = frame
        .parse::<xmpp_parsers::minidom::Element>()
        .unwrap_or_else(|error| panic!("frame is XML ({error}): {frame}"));
    let mut ids = Vec::new();
    collect(&element, &mut ids);
    ids
}

fn collect(element: &xmpp_parsers::minidom::Element, ids: &mut Vec<String>) {
    for child in element.children() {
        if child.is("occupant-id", NS_OCCUPANT_ID) {
            ids.push(child.attr("id").unwrap_or_default().to_owned());
        }
        collect(child, ids);
    }
}

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

#[tokio::test]
async fn mediated_invite_and_decline_relays_carry_the_room_occupant_id() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "oid-invite-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "oid-invite-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "oid-invite-bob").await;

    let room = format!("oid-invite-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let expected = room_occupant_id(&room);
    admin
        .send(&format!(
            r#"<presence to="{room}/{ADMIN}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    for (invitee, client, id) in [(ALICE, &mut alice, "inv-alice"), (BOB, &mut bob, "inv-bob")] {
        admin
            .send(&format!(
                r#"<message to="{room}" id="{id}"><x xmlns="{NS_MUC_USER}"><invite to="{invitee}@{DOMAIN}"><reason>join us</reason></invite></x></message>"#
            ))
            .await
            .expect("send mediated invite");
        let invite = client
            .recv_matching(|frame| frame.contains("<invite") && frame.contains("join us"))
            .await
            .expect("invitee receives the relayed invitation");
        let element = invite
            .parse::<xmpp_parsers::minidom::Element>()
            .expect("invite XML");
        assert_eq!(
            element.attr("from"),
            Some(room.as_str()),
            "the room relays the invitation from its bare JID: {invite}"
        );
        assert_eq!(
            occupant_ids(&invite),
            vec![expected.clone()],
            "exactly one occupant-id, the room's, on the relayed invitation: {invite}"
        );
    }

    alice
        .send(&format!(
            r#"<message to="{room}" id="dec-alice"><x xmlns="{NS_MUC_USER}"><decline to="{ADMIN}@{DOMAIN}"><reason>busy</reason></decline></x></message>"#
        ))
        .await
        .expect("send decline");
    let decline = admin
        .recv_matching(|frame| frame.contains("<decline") && frame.contains("busy"))
        .await
        .expect("inviter receives the relayed decline");
    let element = decline
        .parse::<xmpp_parsers::minidom::Element>()
        .expect("decline XML");
    assert_eq!(
        element.attr("from"),
        Some(room.as_str()),
        "the room relays the decline from its bare JID: {decline}"
    );
    assert_eq!(
        occupant_ids(&decline),
        vec![expected],
        "exactly one occupant-id, the room's, on the relayed decline: {decline}"
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn a_client_supplied_occupant_id_on_an_invite_is_replaced() {
    // XEP-0421 §Business Rules: "If the message ... received by the MUC
    // service already contains <occupant-id> element, the MUC service MUST
    // replace such element before reflecting".
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "oid-forge-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "oid-forge-alice").await;
    let room = format!("oid-forge-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let expected = room_occupant_id(&room);
    admin
        .send(&format!(
            r#"<presence to="{room}/{ADMIN}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
    admin
        .send(&format!(
            r#"<message to="{room}" id="inv-forged"><occupant-id xmlns="{NS_OCCUPANT_ID}" id="forged"/><x xmlns="{NS_MUC_USER}"><invite to="{ALICE}@{DOMAIN}"/></x></message>"#
        ))
        .await
        .expect("send forged invite");
    let invite = alice
        .recv_matching(|frame| frame.contains("<invite"))
        .await
        .expect("invitee receives the relayed invitation");
    assert_eq!(occupant_ids(&invite), vec![expected], "{invite}");

    let _ = admin.close().await;
    let _ = alice.close().await;
}
