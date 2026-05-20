//! XEP-0359 stanza-id integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

use jid::{BareJid, Jid};
use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage, MamQuery, MamStorage};
use waddle_xmpp_core::xep0359::{add_stanza_id, OriginId, StanzaId};
use xmpp_parsers::message::{Message, MessageType};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

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
    let stored_id = storage.store_message(&archive, &row).await.expect("store");

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
    let stored_id = storage.store_message(&archive, &row).await.expect("store");

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
