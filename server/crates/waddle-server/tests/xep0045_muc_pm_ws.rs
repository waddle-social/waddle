//! XEP-0045 §7.5 MUC private messages — dedicated reliability suite
//! (#1257, epic #1269).
//!
//! Pre-#1257 the PM path was fire-and-forget (`try_send_to`, result
//! discarded): a PM to a detached/backpressured session vanished, PMs
//! were never archived, and the sender's other devices never saw a
//! carbon. This suite pins the reliable behavior end-to-end over the
//! WebSocket transport:
//!
//! 1. Delivery to EVERY session of a multi-session target nick, with
//!    the XEP-0421 occupant-id + the §7.5 empty `muc#user` `<x/>`
//!    marker and a XEP-0359 stanza-id stamped by the recipient's
//!    archive.
//! 2. XEP-0280 MUC rule: the OUTBOUND local PM is carbon-copied
//!    (`<sent/>`) to the sender's other carbon-enabled resource.
//! 3. XEP-0198: a PM to a detached-but-resumable occupant session is
//!    queued and replayed on resume, not lost.
//! 4. XEP-0313: the PM lands in both users' archives, keyed by the
//!    room bare JID.
//! 5. `muc#roomconfig_allowpm` is honored (`none` → `<forbidden/>`).
//! 6. A PM to an unknown nick still bounces `<item-not-found/>`.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const ALICE: &str = "alice";
const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_XDATA: &str = "jabber:x:data";
const NS_OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";
const NS_SID: &str = "urn:xmpp:sid:0";
const NS_CARBONS: &str = "urn:xmpp:carbons:2";
const NS_XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const MUC_ROOMCONFIG_FORM: &str = "http://jabber.org/protocol/muc#roomconfig";

// Each test spawns a fresh waddle-server binary; serialize so the
// harness temp-port slot does not race across parallel tests.
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn attr_name(name: &'static str) -> &'static xmpp_parsers::minidom::rxml::NcNameStr {
    name.try_into().expect("valid ncname")
}

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    String::from_utf8(bytes).expect("XML serialization is UTF-8")
}

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect and auth")
}

async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    let join = Element::builder("presence", NS_CLIENT)
        .attr(attr_name("to").to_owned(), format!("{room}/{nick}"))
        .append(Element::builder("x", NS_MUC).build())
        .build();
    client.send(&element_to_xml(join)).await.expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

fn pm_xml(room: &str, target_nick: &str, id: &str, body: &str) -> String {
    element_to_xml(
        Element::builder("message", NS_CLIENT)
            .attr(attr_name("type").to_owned(), "chat")
            .attr(attr_name("to").to_owned(), format!("{room}/{target_nick}"))
            .attr(attr_name("id").to_owned(), id.to_owned())
            .append(
                Element::builder("body", NS_CLIENT)
                    .append(body.to_owned())
                    .build(),
            )
            .build(),
    )
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

/// Assert the canonical §7.5 relayed PM shape: `type='chat'` from the
/// sender's occupant JID with the empty `muc#user` marker, the server
/// occupant-id (#1268), and the recipient-archive stanza-id (#1257).
fn assert_relayed_pm_shape(frame: &str, expected_from: &str, recipient_bare: &str) {
    let element = frame
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("frame must parse as XML: {err}; frame={frame}"));
    assert_eq!(element.name(), "message", "expected <message>: {frame}");
    assert_eq!(
        element.attr("from"),
        Some(expected_from),
        "PM must be relayed from the sender's occupant JID: {frame}"
    );
    let marker = find_descendant(&element, "x", NS_MUC_USER)
        .unwrap_or_else(|| panic!("PM missing the §7.5 muc#user <x/> marker: {frame}"));
    assert_eq!(
        marker.children().count(),
        0,
        "the PM muc#user marker must be empty: {frame}"
    );
    assert!(
        find_descendant(&element, "occupant-id", NS_OCCUPANT_ID).is_some(),
        "XEP-0421: PM must carry the server-stamped occupant-id: {frame}"
    );
    let sid = find_descendant(&element, "stanza-id", NS_SID)
        .unwrap_or_else(|| panic!("PM missing the XEP-0359 stanza-id: {frame}"));
    assert_eq!(
        sid.attr("by"),
        Some(recipient_bare),
        "the live PM copy carries the recipient archive's stanza-id: {frame}"
    );
}

async fn enable_carbons(client: &mut WsXmppClient, id: &str) {
    let iq = Element::builder("iq", NS_CLIENT)
        .attr(attr_name("type").to_owned(), "set")
        .attr(attr_name("id").to_owned(), id.to_owned())
        .append(Element::builder("enable", NS_CARBONS).build())
        .build();
    client
        .send(&element_to_xml(iq))
        .await
        .expect("send carbons enable");
    let _ = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("carbons enable ack");
}

fn data_form_field(var: &str, field_type: Option<&str>, value: &str) -> Element {
    let mut builder =
        Element::builder("field", NS_XDATA).attr(attr_name("var").to_owned(), var.to_owned());
    if let Some(field_type) = field_type {
        builder = builder.attr(attr_name("type").to_owned(), field_type.to_owned());
    }
    builder
        .append(
            Element::builder("value", NS_XDATA)
                .append(value.to_owned())
                .build(),
        )
        .build()
}

/// Submit a §10.2 owner-config form setting `muc#roomconfig_allowpm`.
async fn set_allow_pm(client: &mut WsXmppClient, room: &str, allow_pm: &str) {
    let cfg_id = format!("cfg-allowpm-{}", uuid::Uuid::new_v4());
    let form = Element::builder("x", NS_XDATA)
        .attr(attr_name("type").to_owned(), "submit")
        .append(data_form_field(
            "FORM_TYPE",
            Some("hidden"),
            MUC_ROOMCONFIG_FORM,
        ))
        .append(data_form_field("muc#roomconfig_allowpm", None, allow_pm))
        .build();
    let owner_config = Element::builder("iq", NS_CLIENT)
        .attr(attr_name("type").to_owned(), "set")
        .attr(attr_name("id").to_owned(), cfg_id.clone())
        .attr(attr_name("to").to_owned(), room.to_owned())
        .append(Element::builder("query", NS_MUC_OWNER).append(form).build())
        .build();
    client
        .send(&element_to_xml(owner_config))
        .await
        .expect("send owner config");
    let response = client
        .recv_matching(|frame| frame.contains("<iq") && frame.contains(&cfg_id))
        .await
        .expect("owner config response");
    assert!(
        response.contains("result"),
        "allowpm owner config must be accepted: {response}"
    );
}

/// #1257 + #1268: the PM reaches EVERY session of a multi-session
/// target nick with the canonical relayed shape.
#[tokio::test]
async fn muc_pm_delivered_to_all_sessions_of_target_nick_with_canonical_shape() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-admin").await;
    let mut alice_web = connect(&server, ALICE, &alice_pass, "pm-web").await;
    let mut alice_mobile = connect(&server, ALICE, &alice_pass, "pm-mobile").await;

    let room = format!("pm-fanout-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice_web, &room, ALICE).await;
    join_room(&mut alice_mobile, &room, ALICE).await;

    let body = format!("psst-{}", uuid::Uuid::new_v4());
    admin
        .send(&pm_xml(&room, ALICE, "pm-fanout-1", &body))
        .await
        .expect("send PM");

    let expected_from = format!("{room}/{ADMIN}");
    let alice_bare = format!("{ALICE}@{DOMAIN}");
    let web_copy = alice_web
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("web session receives PM");
    assert_relayed_pm_shape(&web_copy, &expected_from, &alice_bare);
    let mobile_copy = alice_mobile
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("mobile session receives PM (#1257 multi-session fan-out)");
    assert_relayed_pm_shape(&mobile_copy, &expected_from, &alice_bare);

    let _ = admin.close().await;
    let _ = alice_web.close().await;
    let _ = alice_mobile.close().await;
}

/// XEP-0280 MUC rule (#1257): the sender's other carbon-enabled
/// resource receives a `<sent/>` carbon of the outbound PM.
#[tokio::test]
async fn muc_pm_sent_carbon_reaches_senders_other_resource() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin_room = connect(&server, ADMIN, &admin_pass, "pm-carbon-room").await;
    let mut admin_other = connect(&server, ADMIN, &admin_pass, "pm-carbon-other").await;
    enable_carbons(&mut admin_other, "pm-carbons-enable").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-carbon-target").await;

    let room = format!("pm-carbon-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin_room, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;

    let body = format!("carbon-proof-{}", uuid::Uuid::new_v4());
    admin_room
        .send(&pm_xml(&room, ALICE, "pm-carbon-1", &body))
        .await
        .expect("send PM");

    let carbon = admin_other
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("sender's other resource receives the sent carbon");
    let element = carbon
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("carbon must parse as XML: {err}; frame={carbon}"));
    assert!(
        find_descendant(&element, "sent", NS_CARBONS).is_some(),
        "outbound MUC PM must be wrapped in <sent xmlns='{NS_CARBONS}'>: {carbon}"
    );

    let _ = admin_room.close().await;
    let _ = admin_other.close().await;
    let _ = alice.close().await;
}

/// XEP-0198 (#1257 core): a PM to an occupant whose only session is
/// detached-but-resumable is queued in the replay buffer and delivered
/// on resume — pre-fix it silently vanished.
#[tokio::test]
async fn muc_pm_to_detached_resumable_occupant_replays_on_resume() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-detached-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-detached-alice").await;

    let room = format!("pm-detached-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;

    // Alice enables XEP-0198 resumption AFTER joining, then drops
    // uncleanly — her occupancy detaches instead of leaving.
    let enable = Element::builder("enable", "urn:xmpp:sm:3")
        .attr(attr_name("resume").to_owned(), "true")
        .build();
    alice
        .send(&element_to_xml(enable))
        .await
        .expect("send sm enable");
    let enabled = alice
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("sm enabled");
    let stream_id = ["id=\"", "id='"]
        .iter()
        .find_map(|prefix| {
            let start = enabled.find(prefix)? + prefix.len();
            let quote = &prefix[prefix.len() - 1..];
            let end = enabled[start..].find(quote)?;
            Some(enabled[start..start + end].to_string())
        })
        .expect("stream id in <enabled>");
    drop(alice);

    let mut replay = None;
    let mut resumed = None;
    for attempt in 0..20 {
        let body = format!("detached-pm-proof-{attempt}");
        admin
            .send(&pm_xml(&room, ALICE, &format!("pm-det-{attempt}"), &body))
            .await
            .expect("send PM to detached occupant");

        let mut candidate = WsXmppClient::connect(&server.ws_url())
            .await
            .expect("resume connection");
        candidate
            .authenticate(DOMAIN, ALICE, &alice_pass)
            .await
            .expect("authenticate resume connection");
        let resume = Element::builder("resume", "urn:xmpp:sm:3")
            .attr(attr_name("previd").to_owned(), stream_id.clone())
            .attr(attr_name("h").to_owned(), "0")
            .build();
        candidate
            .send(&element_to_xml(resume))
            .await
            .expect("send resume");
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            candidate.recv_matching(|frame| frame.contains(&body)),
        )
        .await
        {
            Ok(Ok(frame)) => {
                replay = Some(frame);
                resumed = Some(candidate);
                break;
            }
            _ => {
                drop(candidate);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    let replay = replay.expect("PM replayed to the resumed session (#1257)");
    assert!(
        replay.contains(&format!("{room}/{ADMIN}")),
        "replayed PM keeps the occupant-JID from: {replay}"
    );

    let _ = admin.close().await;
    if let Some(resumed) = resumed {
        let _ = resumed.close().await;
    }
}

/// XEP-0313 (#1257): the PM is archived in BOTH users' archives, keyed
/// by the room bare JID.
#[tokio::test]
async fn muc_pm_archived_in_both_user_archives() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-mam-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-mam-alice").await;

    let room = format!("pm-mam-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;

    let body = format!("archived-pm-{}", uuid::Uuid::new_v4());
    admin
        .send(&pm_xml(&room, ALICE, "pm-mam-1", &body))
        .await
        .expect("send PM");
    alice
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("alice receives PM");

    for (client, archive_jid, who) in [
        (&mut alice, format!("{ALICE}@{DOMAIN}"), "recipient"),
        (&mut admin, format!("{ADMIN}@{DOMAIN}"), "sender"),
    ] {
        let query_id = format!("pm-mam-query-{who}");
        let query = Element::builder("iq", NS_CLIENT)
            .attr(attr_name("type").to_owned(), "set")
            .attr(attr_name("id").to_owned(), query_id.clone())
            .attr(attr_name("to").to_owned(), archive_jid)
            .append(Element::builder("query", "urn:xmpp:mam:2").build())
            .build();
        client
            .send(&element_to_xml(query))
            .await
            .expect("send MAM query");
        let frames = client
            .recv_until(|frame| frame.contains("<fin"))
            .await
            .expect("MAM query completes");
        assert!(
            frames.iter().any(|frame| frame.contains(&body)),
            "the PM must be archived in the {who}'s user archive (#1257): {frames:?}"
        );
    }

    let _ = admin.close().await;
    let _ = alice.close().await;
}

/// `muc#roomconfig_allowpm` (#1257): `none` disables PMs with
/// `<forbidden/>`; restoring `anyone` re-enables them.
#[tokio::test]
async fn muc_pm_honors_allowpm_room_config() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-allowpm-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-allowpm-alice").await;

    let room = format!("pm-allowpm-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    set_allow_pm(&mut admin, &room, "none").await;
    join_room(&mut alice, &room, ALICE).await;

    let body = format!("blocked-pm-{}", uuid::Uuid::new_v4());
    alice
        .send(&pm_xml(&room, ADMIN, "pm-blocked-1", &body))
        .await
        .expect("send blocked PM");
    let bounce = alice
        .recv_matching(|frame| frame.contains("pm-blocked-1"))
        .await
        .expect("allowpm=none bounce");
    assert!(
        bounce.contains("<error") && bounce.contains("forbidden"),
        "allowpm=none must bounce PMs with <forbidden/>: {bounce}"
    );

    set_allow_pm(&mut admin, &room, "anyone").await;
    let allowed_body = format!("allowed-pm-{}", uuid::Uuid::new_v4());
    alice
        .send(&pm_xml(&room, ADMIN, "pm-allowed-1", &allowed_body))
        .await
        .expect("send allowed PM");
    let delivered = admin
        .recv_matching(|frame| frame.contains(&allowed_body))
        .await
        .expect("allowpm=anyone delivers the PM");
    assert!(
        delivered.contains(&format!("{room}/{ALICE}")),
        "PM relayed from alice's occupant JID: {delivered}"
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
}

/// A PM to a nick nobody holds still bounces `<item-not-found/>`.
#[tokio::test]
async fn muc_pm_to_unknown_nick_bounces_item_not_found() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-unknown-admin").await;

    let room = format!("pm-unknown-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;

    admin
        .send(&pm_xml(&room, "nobody-here", "pm-unknown-1", "hello?"))
        .await
        .expect("send PM to unknown nick");
    let bounce = admin
        .recv_matching(|frame| frame.contains("pm-unknown-1"))
        .await
        .expect("unknown-nick bounce");
    let element = bounce
        .parse::<Element>()
        .unwrap_or_else(|err| panic!("bounce must parse as XML: {err}; frame={bounce}"));
    assert!(
        find_descendant(&element, "item-not-found", NS_XMPP_STANZAS).is_some(),
        "PM to an unknown nick bounces <item-not-found/>: {bounce}"
    );

    let _ = admin.close().await;
}

/// XEP-0045 §7.5: a NON-occupant sending a PM into a room gets
/// `<not-acceptable/>`.
#[tokio::test]
async fn muc_pm_from_non_occupant_bounces_not_acceptable() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin = connect(&server, ADMIN, &admin_pass, "pm-nonocc-admin").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-nonocc-alice").await;

    let room = format!("pm-nonocc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin, &room, ADMIN).await;
    // Alice never joins.
    alice
        .send(&pm_xml(&room, ADMIN, "pm-nonocc-1", "sneaky"))
        .await
        .expect("send PM as non-occupant");
    let bounce = alice
        .recv_matching(|frame| frame.contains("pm-nonocc-1"))
        .await
        .expect("non-occupant bounce");
    assert!(
        bounce.contains("<error") && bounce.contains("not-acceptable"),
        "XEP-0045 §7.5: only occupants may send PMs: {bounce}"
    );

    let _ = admin.close().await;
    let _ = alice.close().await;
}

/// XEP-0280 §6.1 (review P2 on PR #1277): a PM carrying
/// `<private xmlns='urn:xmpp:carbons:2'/>` is delivered but MUST NOT be
/// carbon-copied to the sender's other carbon-enabled resource.
#[tokio::test]
async fn muc_pm_private_hint_suppresses_sent_carbon() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_pass = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_pass)]);
    let admin_pass = server.fixed_account_password().to_string();

    let mut admin_room = connect(&server, ADMIN, &admin_pass, "pm-priv-room").await;
    let mut admin_other = connect(&server, ADMIN, &admin_pass, "pm-priv-other").await;
    enable_carbons(&mut admin_other, "pm-priv-carbons").await;
    let mut alice = connect(&server, ALICE, &alice_pass, "pm-priv-target").await;

    let room = format!("pm-priv-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut admin_room, &room, ADMIN).await;
    join_room(&mut alice, &room, ALICE).await;

    let body = format!("private-hint-{}", uuid::Uuid::new_v4());
    let pm = Element::builder("message", NS_CLIENT)
        .attr(attr_name("type").to_owned(), "chat")
        .attr(attr_name("to").to_owned(), format!("{room}/{ALICE}"))
        .attr(attr_name("id").to_owned(), "pm-priv-1".to_owned())
        .append(
            Element::builder("body", NS_CLIENT)
                .append(body.clone())
                .build(),
        )
        .append(Element::builder("private", NS_CARBONS).build())
        .build();
    admin_room
        .send(&element_to_xml(pm))
        .await
        .expect("send private-hinted PM");

    alice
        .recv_matching(|frame| frame.contains(&body))
        .await
        .expect("PM still delivered to the target occupant");

    // The other resource must observe NO carbon of it.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(900);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, admin_other.recv()).await {
            Ok(Ok(frame)) => {
                assert!(
                    !frame.contains(&body),
                    "XEP-0280 §6.1: <private/> PM must not be carbon-copied: {frame}"
                );
            }
            _ => break,
        }
    }

    let _ = admin_room.close().await;
    let _ = admin_other.close().await;
    let _ = alice.close().await;
}
