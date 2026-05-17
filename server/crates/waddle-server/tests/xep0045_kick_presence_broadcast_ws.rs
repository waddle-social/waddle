//! XEP-0045 §9.1 kick presence broadcast integration test over WebSocket.
//!
//! XEP-0045 §9.1.1 ("Kicking an Occupant"), normative:
//!
//! > The service MUST then remove the kicked occupant by sending a presence
//! > stanza of type "unavailable" to each kicked occupant, including status
//! > code 307 in the extended presence information, optionally along with
//! > the reason (if provided) and the JID of the actor who initiated the
//! > kick.
//! >
//! > The service MUST then inform all of the remaining occupants that the
//! > kicked occupant is no longer in the room by sending presence stanzas
//! > of type "unavailable" from the individual's room-nick (i.e.,
//! > `<room@service/nick>`) to all the remaining occupants.
//!
//! Three occupants (admin/owner, alice/participant, bob/participant) join
//! a MUC. The owner kicks bob via an admin IQ setting `role='none'` and
//! the test asserts:
//!
//! - alice receives `<presence type='unavailable' from='room/bob'>` with
//!   `<x xmlns='http://jabber.org/protocol/muc#user'>` carrying
//!   `<item role='none'>` (with the `<reason/>` and `<actor jid='.../admin'/>`
//!   children) and `<status code='307'/>`.
//! - bob receives the same shape, additionally carrying
//!   `<status code='110'/>` (self-presence per XEP-0045 §6.6).
//! - The admin IQ returns `type='result'`.
//!
//! The wire format is parsed via `minidom::Element` and asserted
//! structurally so reordering of children or attribute-quoting changes
//! do not flake the test.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";

// Each test spawns a fresh waddle-server binary; serialize so the harness
// temp-port slot does not race when several tests run in parallel.
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

/// Send a XEP-0045 join presence to `room/nick` and drain frames until
/// the historical-subject ack arrives (last frame in the join sequence).
async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

/// Find a descendant element with the given local-name and namespace.
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

/// Find the `<x xmlns='muc#user'>` child of a `<presence>` element.
fn muc_user_payload(presence: &Element) -> Option<&Element> {
    find_descendant(presence, "x", NS_MUC_USER)
}

/// Find a `<status code='N'/>` child of a `<x xmlns='muc#user'>` payload.
fn has_status_code(muc_user: &Element, code: &str) -> bool {
    muc_user
        .children()
        .filter(|child| child.name() == "status" && child.ns() == NS_MUC_USER)
        .any(|status| status.attr("code") == Some(code))
}

/// Assert the broadcast shape required by XEP-0045 §9.1.1 / §6.6 for the
/// kicked occupant or a remaining occupant.
fn assert_kick_presence(
    frame: &str,
    expected_from: &str,
    expected_reason: &str,
    expected_actor_contains: &str,
    is_self: bool,
) {
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.name(), "presence", "expected <presence>: {frame}");
    assert_eq!(
        element.attr("type"),
        Some("unavailable"),
        "kick presence must have type='unavailable': {frame}"
    );
    assert_eq!(
        element.attr("from"),
        Some(expected_from),
        "kick presence must come from the kicked occupant's room-nick: {frame}"
    );

    let muc_user = muc_user_payload(&element).unwrap_or_else(|| {
        panic!("kick presence missing <x xmlns='muc#user'>: {frame}");
    });
    assert!(
        has_status_code(muc_user, "307"),
        "XEP-0045 §9.1.1 requires <status code='307'/>: {frame}"
    );
    if is_self {
        assert!(
            has_status_code(muc_user, "110"),
            "XEP-0045 §6.6: self-presence to the kicked occupant must include <status code='110'/>: {frame}"
        );
    }

    let item = muc_user
        .children()
        .find(|child| child.name() == "item" && child.ns() == NS_MUC_USER)
        .unwrap_or_else(|| panic!("kick presence missing <item> in muc#user: {frame}"));
    assert_eq!(
        item.attr("role"),
        Some("none"),
        "kicked occupant must have role='none': {frame}"
    );

    let reason = item
        .get_child("reason", NS_MUC_USER)
        .unwrap_or_else(|| panic!("kick presence missing <reason>: {frame}"));
    assert_eq!(
        reason.text().trim(),
        expected_reason,
        "kick reason should carry through: {frame}"
    );

    let actor = item
        .get_child("actor", NS_MUC_USER)
        .unwrap_or_else(|| panic!("kick presence missing <actor>: {frame}"));
    let actor_jid = actor
        .attr("jid")
        .unwrap_or_else(|| panic!("<actor> must carry the kicker's jid: {frame}"));
    assert!(
        actor_jid.contains(expected_actor_contains),
        "<actor jid='...'> must mention the kicker '{expected_actor_contains}' (got '{actor_jid}'): {frame}"
    );
}

#[tokio::test]
async fn xep_0045_kick_broadcasts_status_307_to_all_occupants() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_pass = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass), (BOB, &bob_pass)]);

    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "kick-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "kick-alice").await;
    let mut bob = connect(&server, BOB, &bob_pass, "kick-bob").await;

    let room = format!("kick-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Admin joins first — opening the room makes admin the owner per
    // XEP-0045 §10.1.3 (instant room). Then alice and bob join as
    // participants. Drain alice/bob's presence-from-others frames so
    // the only frames in flight after this point are kick-related.
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    // Owner sends the kick IQ. XEP-0045 §9.1 says role='none' on the
    // <item> is the kick verb.
    let kick_id = format!("kick-iq-{}", uuid::Uuid::new_v4());
    let reason = "spam";
    admin
        .send(&format!(
            r#"<iq type="set" id="{kick_id}" to="{room}"><query xmlns="{NS_MUC_ADMIN}"><item nick="{BOB}" role="none"><reason>{reason}</reason></item></query></iq>"#
        ))
        .await
        .expect("send kick iq");

    // Admin should receive the IQ result. Drain admin's frames until we
    // see the matching id — the admin also receives its own broadcast
    // copy because the §9.1.1 send-to-all loop includes the admin.
    let admin_result = admin
        .recv_matching(|frame| frame.contains(&kick_id) && frame.contains("<iq"))
        .await
        .expect("admin iq result");
    assert!(
        admin_result.contains(r#"type="result""#) || admin_result.contains(r#"type='result'"#),
        "kick IQ must succeed: {admin_result}"
    );

    let expected_from = format!("{room}/{BOB}");

    // Bob (kicked) MUST receive an unavailable presence with code 307
    // and code 110 (self), from his own room-nick.
    let bob_kick = bob
        .recv_matching(|frame| {
            frame.contains("<presence") && frame.contains("type=\"unavailable\"")
                || frame.contains("type='unavailable'")
        })
        .await
        .expect("bob receives kick presence");
    assert_kick_presence(&bob_kick, &expected_from, reason, ADMIN, true);

    // Alice (remaining occupant) MUST receive the same unavailable
    // presence from bob's room-nick, with code 307 but NOT 110.
    let alice_kick = alice
        .recv_matching(|frame| {
            frame.contains("<presence")
                && (frame.contains("type=\"unavailable\"") || frame.contains("type='unavailable'"))
                && frame.contains(&format!("/{BOB}"))
        })
        .await
        .expect("alice receives kick broadcast");
    assert_kick_presence(&alice_kick, &expected_from, reason, ADMIN, false);

    // The same broadcast also goes to the admin's session per §9.1.1
    // ("inform all of the remaining occupants"). Drain it so the close
    // exchange below does not see leftover frames.
    let _admin_kick = admin
        .recv_matching(|frame| {
            frame.contains("<presence")
                && (frame.contains("type=\"unavailable\"") || frame.contains("type='unavailable'"))
                && frame.contains(&format!("/{BOB}"))
        })
        .await
        .expect("admin also receives kick broadcast");

    let _ = admin.close().await;
    let _ = alice.close().await;
    let _ = bob.close().await;
}
