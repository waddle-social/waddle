//! XEP-0359 stanza-id integration tests over WebSocket.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

use jid::{BareJid, Jid};
use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage, MamQuery, MamStorage, StoreOutcome};
use waddle_xmpp_core::xep0359::{add_stanza_id, OriginId, StanzaId};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::ns::{JABBER_CLIENT as NS_CLIENT, MAM as NS_MAM, MUC as NS_MUC, SID as NS_SID};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const ALICE: &str = "alice";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Debug, PartialEq, Eq)]
struct EnvelopeId(String);

impl EnvelopeId {
    fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    String::from_utf8(bytes).expect("XML serialization is UTF-8")
}

fn find_message_by_body<'a>(element: &'a Element, body: &str) -> Option<&'a Element> {
    if element.name() == "message"
        && element
            .children()
            .find(|child| child.name() == "body")
            .is_some_and(|candidate| candidate.text() == body)
    {
        return Some(element);
    }
    element
        .children()
        .find_map(|child| find_message_by_body(child, body))
}

fn frame_has_message_body(frame: &str, body: &str) -> bool {
    frame
        .parse::<Element>()
        .ok()
        .and_then(|root| find_message_by_body(&root, body).map(|_| ()))
        .is_some()
}

fn message_ids(
    frame: &str,
    body: &str,
    room: &BareJid,
) -> (EnvelopeId, Vec<OriginId>, Vec<StanzaId>) {
    let root = frame.parse::<Element>().expect("valid XML frame");
    let message = find_message_by_body(&root, body)
        .unwrap_or_else(|| panic!("message with body {body:?} missing from frame: {frame}"));
    let envelope_id = EnvelopeId::new(message.attr("id").expect("message id preserved"));
    let origin_ids = message
        .children()
        .filter(|child| child.is("origin-id", NS_SID))
        .filter_map(|child| child.attr("id").map(OriginId::new))
        .collect();
    let room_stanza_ids = message
        .children()
        .filter(|child| child.is("stanza-id", NS_SID) && child.attr("by") == Some(room.as_str()))
        .filter_map(|child| {
            child
                .attr("id")
                .map(|id| StanzaId::new(id, Jid::from(room.clone())))
        })
        .collect();
    (envelope_id, origin_ids, room_stanza_ids)
}

fn groupchat_message(room: &BareJid, id: &EnvelopeId, origin_id: &OriginId, body: &str) -> String {
    element_to_xml(
        Element::builder("message", NS_CLIENT)
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
                room.as_str(),
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
                "groupchat",
            )
            .attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
                id.as_str(),
            )
            .append(Element::builder("body", NS_CLIENT).append(body).build())
            .append(
                Element::builder("origin-id", NS_SID)
                    .attr(
                        xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
                        origin_id.as_str(),
                    )
                    .build(),
            )
            .build(),
    )
}

async fn join_room_as(client: &mut WsXmppClient, room: &BareJid, nick: &str) {
    let occupant = room
        .clone()
        .with_resource_str(nick)
        .expect("valid occupant resource");
    let presence = Element::builder("presence", NS_CLIENT)
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
            occupant.as_str(),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build();
    client
        .send(&element_to_xml(presence))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0359-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

async fn join_room(client: &mut WsXmppClient, room: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

#[tokio::test]
async fn room_replaces_spoofed_room_stanza_id_and_preserves_origin_id() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("sid-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="client-msg-1">
                <body>sid body</body>
                <stanza-id xmlns="urn:xmpp:sid:0" id="spoofed" by="{room}"/>
                <origin-id xmlns="urn:xmpp:sid:0" id="origin-1"/>
            </message>"#
        ))
        .await
        .expect("send message");

    let echo = client
        .recv_matching(|frame| frame.contains("sid body"))
        .await
        .expect("echo");
    assert!(echo.contains("urn:xmpp:sid:0"), "echo missing sid: {echo}");
    assert!(echo.contains("origin-1"), "origin-id not preserved: {echo}");
    assert!(
        !echo.contains("spoofed"),
        "spoofed room stanza-id leaked: {echo}"
    );
    assert!(echo.contains(&format!("by='{room}'")) || echo.contains(&format!("by='{room}'")));

    let _ = client.close().await;
}

#[tokio::test]
async fn room_assigns_distinct_stanza_ids_when_occupants_reuse_sender_ids() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[(ALICE, &alice_password)]);
    let admin_password = server.fixed_account_password().to_owned();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_password,
        "sid-collision-admin",
    )
    .await
    .expect("connect admin");
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        ALICE,
        &alice_password,
        "sid-collision-alice",
    )
    .await
    .expect("connect alice");
    let room: BareJid = format!("sid-collision-{}@muc.{DOMAIN}", uuid::Uuid::new_v4())
        .parse()
        .expect("valid room JID");
    join_room_as(&mut admin, &room, USERNAME).await;
    join_room_as(&mut alice, &room, ALICE).await;

    let shared_envelope_id = EnvelopeId::new("reused-client-id");
    let shared_origin_id = OriginId::new("reused-origin-id");
    let admin_body = "admin collision message";
    let alice_body = "alice collision message";
    admin
        .send(&groupchat_message(
            &room,
            &shared_envelope_id,
            &shared_origin_id,
            admin_body,
        ))
        .await
        .expect("send admin message");
    let admin_reflection = admin
        .recv_matching(|frame| frame_has_message_body(frame, admin_body))
        .await
        .expect("admin reflection");
    alice
        .send(&groupchat_message(
            &room,
            &shared_envelope_id,
            &shared_origin_id,
            alice_body,
        ))
        .await
        .expect("send alice message");
    let alice_reflection = admin
        .recv_matching(|frame| frame_has_message_body(frame, alice_body))
        .await
        .expect("alice reflection");

    let (admin_envelope, admin_origins, admin_room_ids) =
        message_ids(&admin_reflection, admin_body, &room);
    let (alice_envelope, alice_origins, alice_room_ids) =
        message_ids(&alice_reflection, alice_body, &room);
    assert_eq!(admin_envelope, shared_envelope_id);
    assert_eq!(alice_envelope, shared_envelope_id);
    assert_eq!(admin_origins, vec![shared_origin_id.clone()]);
    assert_eq!(alice_origins, vec![shared_origin_id.clone()]);
    assert_eq!(admin_room_ids.len(), 1);
    assert_eq!(alice_room_ids.len(), 1);
    assert!(!admin_room_ids[0].id.is_empty());
    assert!(!alice_room_ids[0].id.is_empty());
    assert_ne!(
        admin_room_ids[0], alice_room_ids[0],
        "the room authority must assign distinct identities despite reused sender aliases"
    );

    let query_id = "mam-sid-collision";
    let mam_query = Element::builder("iq", NS_CLIENT)
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "set",
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            query_id,
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
            room.as_str(),
        )
        .append(Element::builder("query", NS_MAM).build())
        .build();
    admin
        .send(&element_to_xml(mam_query))
        .await
        .expect("query room MAM");
    let mam_frames = admin
        .recv_until(|frame| frame.contains(query_id) && frame.contains("<fin"))
        .await
        .expect("MAM results");
    let admin_mam = mam_frames
        .iter()
        .find(|frame| frame_has_message_body(frame, admin_body))
        .expect("admin MAM row");
    let alice_mam = mam_frames
        .iter()
        .find(|frame| frame_has_message_body(frame, alice_body))
        .expect("alice MAM row");
    let (admin_mam_envelope, admin_mam_origins, admin_mam_room_ids) =
        message_ids(admin_mam, admin_body, &room);
    let (alice_mam_envelope, alice_mam_origins, alice_mam_room_ids) =
        message_ids(alice_mam, alice_body, &room);
    assert_eq!(admin_mam_envelope, shared_envelope_id);
    assert_eq!(alice_mam_envelope, shared_envelope_id);
    assert_eq!(admin_mam_origins, vec![shared_origin_id.clone()]);
    assert_eq!(alice_mam_origins, vec![shared_origin_id]);
    assert_eq!(admin_mam_room_ids, admin_room_ids);
    assert_eq!(alice_mam_room_ids, alice_room_ids);

    let _ = admin.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn xep_0359_typed_stanza_id_round_trips_through_storage_with_typed_by_jid() {
    // #228 commit 7: ArchivedMessage.stanza_id is now Option<StanzaId>
    // ({ id: String, by: jid::Jid }). The `by` is reconstructed from
    // the row's archive jid at decode time per the locked Q4 design.
    // Locks the round-trip at the column level so a future regression
    // that drops the `by` reconstruction (or substitutes a sentinel)
    // is immediately caught.
    let storage = InMemoryMamStorage::new();
    let archive: BareJid = "room@conference.example.com"
        .parse()
        .expect("valid bare jid");
    let archive_jid: Jid = "room@conference.example.com".parse().expect("valid jid");

    let row = ArchivedMessage {
        body: Some("typed sid round-trip".to_string()),
        stanza_id: Some(StanzaId::new("wire-id-typed-1", archive_jid.clone())),
        ..ArchivedMessage::for_test(
            "alice@example.com/web".parse().expect("from jid"),
            archive_jid.clone(),
        )
    };
    let stored_id = match storage.store_message(&archive, &row).await.expect("store") {
        StoreOutcome::Stored(id) => id,
        other => panic!("expected stored row, got {other:?}"),
    };

    let retrieved = storage
        .get_message(&stored_id)
        .await
        .expect("query")
        .expect("retrieved row");

    let sid = retrieved.stanza_id.expect("stanza_id present after decode");
    assert_eq!(sid.id, "wire-id-typed-1");
    assert_eq!(
        sid.by, archive_jid,
        "decoded StanzaId.by must be the typed Jid reconstructed from the row's archive jid"
    );
}

#[tokio::test]
async fn xep_0359_typed_origin_id_round_trips_through_storage() {
    // #228 commit 7: ArchivedMessage.origin_id is now Option<OriginId>.
    // OriginId carries only the id value (XEP-0359 origin-ids have no
    // `by` attribute). Lock the round-trip so the typed wrapper isn't
    // accidentally collapsed back to Option<String> by a future change.
    let storage = InMemoryMamStorage::new();
    let archive: BareJid = "room@conference.example.com"
        .parse()
        .expect("valid bare jid");
    let archive_jid: Jid = "room@conference.example.com".parse().expect("valid jid");

    let row = ArchivedMessage {
        body: Some("typed origin id round-trip".to_string()),
        origin_id: Some(OriginId::new("client-origin-typed-1")),
        ..ArchivedMessage::for_test(
            "alice@example.com/web".parse().expect("from jid"),
            archive_jid,
        )
    };
    let stored_id = match storage.store_message(&archive, &row).await.expect("store") {
        StoreOutcome::Stored(id) => id,
        other => panic!("expected stored row, got {other:?}"),
    };

    let retrieved = storage
        .get_message(&stored_id)
        .await
        .expect("query")
        .expect("retrieved row");

    let oid = retrieved.origin_id.expect("origin_id present after decode");
    assert_eq!(oid.id, "client-origin-typed-1");

    // Round-trip still resolves the row by the origin-id lookup path
    // even though the field shape changed.
    let by_origin = storage
        .get_message_by_stanza_id(
            &"room@conference.example.com"
                .parse::<BareJid>()
                .expect("valid bare jid"),
            "client-origin-typed-1",
        )
        .await
        .expect("by-origin lookup");
    assert!(
        by_origin.is_some(),
        "get_message_by_stanza_id must still match the typed origin_id field"
    );
}

#[tokio::test]
async fn xep_0359_query_filter_finds_row_by_typed_stanza_id() {
    // Regression guard: the `matches_thread_filter` callsite
    // (`MamQuery.thread_id`) inspects `message.stanza_id` and was
    // previously a `.as_deref()` check. After typing the field, the
    // equivalent typed projection must keep matching against the
    // inner `id` value.
    let storage = InMemoryMamStorage::new();
    let archive: BareJid = "room@conference.example.com"
        .parse()
        .expect("valid bare jid");
    let archive_jid: Jid = "room@conference.example.com".parse().expect("valid jid");

    let row = ArchivedMessage {
        id: "row-thread-via-stanza-id".to_string(),
        body: Some("root".to_string()),
        stanza_id: Some(StanzaId::new("thread-by-stanza-id", archive_jid.clone())),
        message_type: MessageType::Groupchat,
        ..ArchivedMessage::for_test(
            "room@conference.example.com/alice"
                .parse()
                .expect("from jid"),
            archive_jid,
        )
    };
    storage.store_message(&archive, &row).await.expect("store");

    let result = storage
        .query_messages(
            &archive,
            waddle_xmpp::mam::MamArchiveKind::Room,
            &MamQuery {
                thread_id: waddle_xmpp_core::mam::ThreadId::new("thread-by-stanza-id"),
                ..Default::default()
            },
        )
        .await
        .expect("query");

    assert_eq!(
        result.messages.len(),
        1,
        "thread filter must still match against typed stanza_id.id"
    );
}

#[test]
fn xep_0359_add_stanza_id_emits_required_by_attribute_per_section_3() {
    // XEP-0359 §3: "The `by` attribute MUST be present." After commit 7
    // the typed `add_stanza_id(msg, &StanzaId)` makes that requirement
    // structural — the typed value carries both fields together so
    // callers cannot accidentally emit `<stanza-id id='X'/>` without
    // a `by` attribute.
    let mut msg = Message::new(None::<jid::Jid>);
    let by: jid::Jid = "room@conference.example.com".parse().expect("valid jid");
    add_stanza_id(&mut msg, &StanzaId::new("archive-1", by.clone()));

    let elem = msg
        .payloads
        .iter()
        .find(|p| p.name() == "stanza-id" && p.ns() == "urn:xmpp:sid:0")
        .expect("stanza-id payload emitted");
    assert_eq!(elem.attr("id"), Some("archive-1"));
    assert_eq!(
        elem.attr("by"),
        Some(by.to_string().as_str()),
        "XEP-0359 §3: `by` attribute MUST be present"
    );
}

#[tokio::test]
async fn server_disco_advertises_rich_message_features() {
    // The shared `server_features()` catalogue from `waddle-xmpp-core`
    // must be reflected by the live server-disco IQ path so clients
    // can discover XEP support. Without this, advertised behaviour
    // is invisible to clients (the inverse of the project's
    // "no advertise without behaviour" rule — equally bad).
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let response = disco_info_query(&mut client, DOMAIN, "rich-disco-1")
        .await
        .expect("disco#info response");

    for ns in [
        "urn:xmpp:sid:0",
        "urn:xmpp:reply:0",
        "urn:xmpp:message-correct:0",
        "urn:xmpp:message-retract:1",
        "urn:xmpp:reactions:0",
        "urn:xmpp:reference:0",
        "urn:xmpp:fallback:0",
    ] {
        assert!(
            response.contains(ns),
            "server disco#info missing feature {ns}: {response}"
        );
    }

    let _ = client.close().await;
}
