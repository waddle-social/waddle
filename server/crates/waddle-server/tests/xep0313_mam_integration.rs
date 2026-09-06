//! XEP-0313 MAM integration tests over WebSocket.
//!
//! Each test starts its own isolated waddle-server on dynamic ports.

use waddle_ws_test_support as ws_common;

use jid::Jid;
use sqlx::PgPool;
use tokio::sync::Mutex;
use waddle_server::{
    config::LineageConfig,
    db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner},
    ingress_uow::{IngressUnitOfWork, MamArchiveRepository},
};
use waddle_xmpp::mam::{
    ArchivedMessage, MamArchiveKind, MamQuery, MamStorage, MamStorageError, MamTxStoreOutcome,
    SqlxMamStorage,
};
use ws_common::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::message::MessageType;
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

/// Start an isolated server and connect an authenticated client.
async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("test-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("Failed to connect and authenticate");
    (server, client)
}

fn mam_query_xml(id: &str, archive_jid: &str, max: Option<u32>) -> String {
    let rsm = max
        .map(|m| format!(r#"<set xmlns="http://jabber.org/protocol/rsm"><max>{m}</max></set>"#))
        .unwrap_or_default();
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2">{rsm}</query></iq>"#
    )
}

fn mam_query_after_xml(id: &str, archive_jid: &str, max: u32, after: &str) -> String {
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"><set xmlns="http://jabber.org/protocol/rsm"><max>{max}</max><after>{after}</after></set></query></iq>"#
    )
}

fn mam_query_ids_xml(id: &str, archive_jid: &str, ids: &[&str]) -> String {
    let id_values = ids
        .iter()
        .map(|archive_id| format!(r#"<value>{archive_id}</value>"#))
        .collect::<String>();
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>urn:xmpp:mam:2</value></field><field var="ids">{id_values}</field></x></query></iq>"#
    )
}

async fn query_mam(client: &mut WsXmppClient, query_xml: &str) -> Result<Vec<String>, String> {
    client.send(query_xml).await?;
    client
        .recv_until(|frame| frame.contains("urn:xmpp:mam:2") && frame.contains("<fin"))
        .await
}

fn extract_mam_body(frame: &str) -> Option<String> {
    let start = frame.find("<body>")?;
    let end = frame.find("</body>")?;
    Some(frame[start + 6..end].to_string())
}

fn extract_result_id(frame: &str) -> Option<String> {
    let result = &frame[frame.find("<result")?..];
    if let Some(start) = result.find(" id=\"") {
        let value = &result[start + 5..];
        return Some(value[..value.find('"')?].to_string());
    }
    if let Some(start) = result.find(" id='") {
        let value = &result[start + 5..];
        return Some(value[..value.find('\'')?].to_string());
    }
    None
}

fn extract_fin_last(frame: &str) -> Option<String> {
    let start = frame.find("<last>")?;
    let end = frame.find("</last>")?;
    Some(frame[start + 6..end].to_string())
}

// =========================================================================
// MUC MAM
// =========================================================================

#[tokio::test]
async fn muc_message_archived_and_queryable() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-test-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Join
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    // Send 3 messages, wait for the specific echo after each
    let bodies = ["Hello MAM one", "Hello MAM two", "Hello MAM three"];
    for body in &bodies {
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send message");
        let expected = body.to_string();
        client
            .recv_matching(|f| f.contains(&expected))
            .await
            .expect("echo");
    }

    // Query MAM
    let q_id = format!("q-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut client, &mam_query_xml(&q_id, &room, Some(50)))
        .await
        .expect("MAM query failed");

    let fin = frames.last().expect("No frames from MAM");
    assert!(fin.contains("<fin"), "Last frame should be <fin>: {fin}");
    assert!(
        fin.contains("complete") && fin.contains("true"),
        "Expected complete='true': {fin}"
    );

    let result_frames: Vec<&str> = frames
        .iter()
        .map(|f| f.as_str())
        .filter(|f| f.contains("<forwarded"))
        .collect();
    assert_eq!(result_frames.len(), 3, "Expected 3 MAM results");

    let returned_bodies: Vec<String> = result_frames
        .iter()
        .filter_map(|f| extract_mam_body(f))
        .collect();
    for body in &bodies {
        assert!(
            returned_bodies.iter().any(|b| b == body),
            "Missing '{body}' in {returned_bodies:?}"
        );
    }

    let _ = client.close().await;
}

#[tokio::test]
async fn muc_mam_pagination() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-page-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Join
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    // Send 5 messages, wait for each specific echo
    for i in 1..=5 {
        let body = format!("page msg {i}");
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send");
        client
            .recv_matching(|f| f.contains(&body))
            .await
            .expect("echo");
    }

    // Page 1: max=2
    let q1 = format!("p1-{}", uuid::Uuid::new_v4());
    let page1 = query_mam(&mut client, &mam_query_xml(&q1, &room, Some(2)))
        .await
        .expect("page 1");
    let fin1 = page1.last().expect("no fin page 1");
    assert!(
        !(fin1.contains("complete") && fin1.contains("true")),
        "Page 1 should not be complete"
    );
    assert_eq!(
        page1.iter().filter(|f| f.contains("<forwarded")).count(),
        2,
        "Page 1 should have 2 results"
    );
    let last1 = extract_fin_last(fin1).expect("<last> in page 1 fin");

    // Page 2: max=2, after last1
    let q2 = format!("p2-{}", uuid::Uuid::new_v4());
    let page2 = query_mam(&mut client, &mam_query_after_xml(&q2, &room, 2, &last1))
        .await
        .expect("page 2");
    let page2_bodies: Vec<String> = page2.iter().filter_map(|f| extract_mam_body(f)).collect();
    assert_eq!(
        page2_bodies,
        vec!["page msg 3".to_string(), "page msg 4".to_string()],
        "Page 2 should contain the next two messages"
    );
    assert_eq!(
        page2.iter().filter(|f| f.contains("<forwarded")).count(),
        2,
        "Page 2 should have 2 results"
    );
    let last2 = extract_fin_last(page2.last().expect("fin page 2")).expect("<last> page 2");

    // Page 3: remaining 1
    let q3 = format!("p3-{}", uuid::Uuid::new_v4());
    let page3 = query_mam(&mut client, &mam_query_after_xml(&q3, &room, 2, &last2))
        .await
        .expect("page 3");
    let page3_bodies: Vec<String> = page3.iter().filter_map(|f| extract_mam_body(f)).collect();
    assert_eq!(
        page3_bodies,
        vec!["page msg 5".to_string()],
        "Page 3 should contain only the final message"
    );
    assert_eq!(
        page3.iter().filter(|f| f.contains("<forwarded")).count(),
        1,
        "Page 3 should have 1 result"
    );
    let fin3 = page3.last().expect("fin page 3");
    assert!(
        fin3.contains("complete") && fin3.contains("true"),
        "Page 3 should be complete"
    );

    // All 5 bodies across pages
    let all_bodies: Vec<String> = page1
        .iter()
        .chain(page2.iter())
        .chain(page3.iter())
        .filter_map(|f| extract_mam_body(f))
        .collect();
    assert_eq!(
        all_bodies,
        vec![
            "page msg 1".to_string(),
            "page msg 2".to_string(),
            "page msg 3".to_string(),
            "page msg 4".to_string(),
            "page msg 5".to_string()
        ]
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn personal_archive_disco_advertises_extended_mam_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let archive_jid = format!("{USERNAME}@{DOMAIN}");

    let response = disco_info_query(&mut client, &archive_jid, "mam-disco-personal")
        .await
        .expect("personal archive disco#info response");

    assert!(
        response.contains("urn:xmpp:mam:2#extended"),
        "personal archive disco missing urn:xmpp:mam:2#extended: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn personal_archive_mam_form_includes_extended_fields() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let archive_jid = format!("{USERNAME}@{DOMAIN}");

    client
        .send(&format!(
            r#"<iq type="get" id="mam-form-personal" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send mam form request");
    let response = client
        .recv_matching(|frame| frame.contains("mam-form-personal"))
        .await
        .expect("mam form response");

    for field in ["before-id", "after-id", "ids"] {
        assert!(
            response.contains(&format!("var='{field}'"))
                || response.contains(&format!("var='{field}'")),
            "mam form missing {field}: {response}"
        );
    }
    assert!(
        response.contains("http://jabber.org/protocol/xdata-validate"),
        "mam form missing XEP-0122 validation namespace: {response}"
    );
    assert!(
        response.contains("datatype='xs:string'") || response.contains("datatype='xs:string'"),
        "mam form missing ids datatype: {response}"
    );
    assert!(response.contains("<open") || response.contains("<open/>"));

    let _ = client.close().await;
}

#[tokio::test]
async fn room_disco_advertises_extended_mam_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-disco-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

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

    let response = disco_info_query(&mut client, &room, "mam-disco-room")
        .await
        .expect("room disco#info response");

    assert!(
        response.contains("urn:xmpp:mam:2#extended"),
        "room disco missing urn:xmpp:mam:2#extended: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn room_mam_ids_query_returns_only_requested_messages() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-ids-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

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

    let bodies = ["ids one", "ids two", "ids three"];
    for body in &bodies {
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send message");
        client
            .recv_matching(|frame| frame.contains(body))
            .await
            .expect("echo");
    }

    let initial = query_mam(
        &mut client,
        &mam_query_xml(
            &format!("ids-seed-{}", uuid::Uuid::new_v4()),
            &room,
            Some(10),
        ),
    )
    .await
    .expect("seed mam query");
    let result_frames: Vec<&str> = initial
        .iter()
        .map(|frame| frame.as_str())
        .filter(|frame| frame.contains("<forwarded"))
        .collect();
    let selected_ids = [
        extract_result_id(result_frames[2]).expect("third archive id"),
        extract_result_id(result_frames[0]).expect("first archive id"),
    ];

    let filtered = query_mam(
        &mut client,
        &mam_query_ids_xml(
            &format!("ids-filter-{}", uuid::Uuid::new_v4()),
            &room,
            &[selected_ids[0].as_str(), selected_ids[1].as_str()],
        ),
    )
    .await
    .expect("ids mam query");
    let filtered_bodies: Vec<String> = filtered
        .iter()
        .filter_map(|frame| extract_mam_body(frame))
        .collect();

    assert_eq!(
        filtered_bodies,
        vec!["ids one".to_string(), "ids three".to_string()]
    );

    let _ = client.close().await;
}

/// Returns the contents of the inner `<message>`'s `<body>` element
/// inside a single MAM `<forwarded>` frame, distinguishing three
/// wire shapes:
///
/// - `Some(Some(text))` — `<body>text</body>` (or self-closing
///   `<body/>` parses as the empty string).
/// - `Some(None)` — no `<body>` element on the inner message at all.
/// - `None` — the frame has no inner `<message>` at all (caller bug).
fn extract_inner_body_presence(frame: &str) -> Option<Option<String>> {
    let forwarded_start = frame.find("<forwarded")?;
    let inner_msg_start = frame[forwarded_start..]
        .find("<message")
        .map(|i| i + forwarded_start)?;
    let inner_msg_end = frame[inner_msg_start..]
        .find("</message>")
        .map(|i| i + inner_msg_start + "</message>".len())?;
    let inner = &frame[inner_msg_start..inner_msg_end];

    let mut depth: i32 = 0;
    let mut idx = 0usize;
    let bytes = inner.as_bytes();
    while idx < bytes.len() {
        if bytes[idx] == b'<' {
            if idx + 1 < bytes.len() && bytes[idx + 1] == b'/' {
                depth -= 1;
                idx += 1;
                continue;
            }
            if depth == 1 && inner[idx..].starts_with("<body") {
                let after_tag = idx + "<body".len();
                let close_rel = inner[after_tag..].find('>')?;
                let close = after_tag + close_rel;
                let is_self_close = bytes[close - 1] == b'/';
                if is_self_close {
                    return Some(Some(String::new()));
                }
                let text_start = close + 1;
                let text_end_rel = inner[text_start..].find("</body>")?;
                return Some(Some(
                    inner[text_start..text_start + text_end_rel].to_owned(),
                ));
            }
            depth += 1;
        }
        idx += 1;
    }
    Some(None)
}

#[tokio::test]
async fn xep_0313_archives_preserve_body_presence_distinction() {
    // RFC 6121 §5.2.3 / XEP-0313 §3 wire fidelity: the MAM archive must
    // distinguish three body wire shapes when a row is replayed:
    //
    //   1. `<body>text</body>` -> Some("text")
    //   2. `<body></body>` (or `<body/>`) -> Some("")
    //   3. no `<body>` element at all -> None
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("body-fidelity-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    let id_text = format!("body-text-{}", uuid::Uuid::new_v4());
    let id_empty = format!("body-empty-{}", uuid::Uuid::new_v4());
    let id_absent = format!("body-absent-{}", uuid::Uuid::new_v4());
    let body_text = format!("hello-{}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{id_text}"><body>{body_text}</body></message>"#
        ))
        .await
        .expect("send text-body message");
    client
        .recv_matching(|f| f.contains(&body_text))
        .await
        .expect("echo text-body");

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{id_empty}"><body></body></message>"#
        ))
        .await
        .expect("send empty-body message");
    client
        .recv_matching(|f| f.contains(&id_empty))
        .await
        .expect("echo empty-body");

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{id_absent}"><reactions xmlns="urn:xmpp:reactions:0" id="{id_text}"><reaction>👍</reaction></reactions></message>"#
        ))
        .await
        .expect("send reaction-only message");
    client
        .recv_matching(|f| f.contains(&id_absent))
        .await
        .expect("echo reaction-only");

    let q_id = format!("body-fid-q-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut client, &mam_query_xml(&q_id, &room, Some(50)))
        .await
        .expect("MAM query");

    let result_frames: Vec<&str> = frames
        .iter()
        .map(String::as_str)
        .filter(|f| f.contains("<forwarded"))
        .collect();
    assert!(
        result_frames.len() >= 3,
        "Expected at least 3 MAM result frames, got {}: {result_frames:?}",
        result_frames.len()
    );

    let frame_text = result_frames
        .iter()
        .find(|f| f.contains(&id_text))
        .unwrap_or_else(|| panic!("text-body frame not found in {result_frames:?}"));
    let frame_empty = result_frames
        .iter()
        .find(|f| f.contains(&id_empty))
        .unwrap_or_else(|| panic!("empty-body frame not found in {result_frames:?}"));
    let frame_absent = result_frames
        .iter()
        .find(|f| f.contains(&id_absent))
        .unwrap_or_else(|| panic!("reaction-only frame not found in {result_frames:?}"));

    assert_eq!(
        extract_inner_body_presence(frame_text),
        Some(Some(body_text.clone())),
        "Case 1 (text body) must replay as <body>{body_text}</body>: {frame_text}"
    );
    assert_eq!(
        extract_inner_body_presence(frame_empty),
        Some(Some(String::new())),
        "Case 2 (empty body element) must replay as <body></body> (or <body/>), not be dropped: {frame_empty}"
    );
    assert_eq!(
        extract_inner_body_presence(frame_absent),
        Some(None),
        "Case 3 (no <body> element) must replay with NO <body> element on the inner message. Frame: {frame_absent}"
    );

    let _ = client.close().await;
}

// =========================================================================
// DM MAM
// =========================================================================

#[tokio::test]
async fn dm_archived_in_sender_personal_archive() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let bare_jid = format!("{USERNAME}@{DOMAIN}");

    // Self-DM so it archives in personal archive
    let unique_body = format!("dm-test-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<message type="chat" to="{bare_jid}"><body>{unique_body}</body></message>"#
        ))
        .await
        .expect("send DM");

    // Consume any echo/carbon
    let _ = client
        .recv_timeout(std::time::Duration::from_millis(500))
        .await;

    // Query personal archive
    let q_id = format!("dm-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut client, &mam_query_xml(&q_id, &bare_jid, Some(50)))
        .await
        .expect("DM MAM query failed");

    assert!(
        frames.last().expect("no fin").contains("<fin"),
        "Last frame should be <fin>"
    );
    // Verify the body appears in a MAM result frame (not just any echo)
    assert!(
        frames.iter().any(|f| {
            f.contains(r#"<result xmlns="urn:xmpp:mam:2""#)
                || f.contains(r#"<result xmlns='urn:xmpp:mam:2'"#)
        } && f.contains("<forwarded")
            && f.contains(&unique_body)),
        "DM body not found in MAM result frames"
    );

    let _ = client.close().await;
}

// =========================================================================
// Typed-JID round-trip (#228 commit 6)
// =========================================================================
//
// These tests bypass the WebSocket I/O boundary and exercise
// `SqlxMamStorage` directly. The point is to lock the typed-JID
// invariants on the MAM storage surface — `ArchivedMessage.from`/`.to`
// as `jid::Jid` and the storage trait's `archive_jid` as `&BareJid` —
// so a future regression that reverts to stringly-typed payloads can't
// land silently. The decode-error case mirrors the
// `xep0201_thread_parent.rs` orphan-column escape hatch pattern from
// commit 4 / commit 5: deliberately insert a malformed row via raw
// SQL, then assert that the typed decode boundary surfaces a
// `MamStorageError::Serialization` rather than papering over the
// corruption with a sentinel JID (the prior `parse_message_jid`
// "unknown@invalid" data-loss bug).

const ARCHIVE: &str = "room@conference.example.com";

#[tokio::test]
async fn xep_0313_uow_mam_write_is_queryable_through_the_archive_read_path() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (XEP-0313 UoW MAM)");
        return;
    };
    let schema = format!("waddle_test_xep0313_uow_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPool::connect(&database_url)
        .await
        .expect("connect PostgreSQL admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated PostgreSQL schema");
    let schema_url = postgres_url_with_search_path(&database_url, &schema);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
    config.pool_size = 2;
    let db = Database::from_config("xep0313-uow-mam", &config)
        .await
        .expect("open isolated PostgreSQL database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("apply server migrations");
    let lineage_config = LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90031"
                .parse()
                .expect("valid fixture deployment UUID"),
        ),
        action: None,
    };
    lineage::enroll(&db, &lineage_config)
        .await
        .expect("enroll fixture lineage");
    let storage = SqlxMamStorage::open(&schema_url)
        .await
        .expect("initialize MAM storage schema");
    let archive: jid::BareJid = format!("uow-{}@conference.example.com", uuid::Uuid::new_v4())
        .parse()
        .expect("valid archive JID");
    let archive_id = format!("uow-mam-{}", uuid::Uuid::new_v4());
    let message = ArchivedMessage {
        id: archive_id.clone(),
        body: Some("unit-of-work MAM archive row".to_string()),
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new(format!(
            "origin-{archive_id}"
        ))),
        message_type: MessageType::Groupchat,
        ..ArchivedMessage::for_test(
            format!("{archive}/romeo")
                .parse()
                .expect("valid fixture sender JID"),
            jid::Jid::from(archive.clone()),
        )
    };
    let uow = IngressUnitOfWork::open(db.clone(), lineage_config)
        .expect("open PostgreSQL ingress unit of work");
    let mut transaction = uow.begin().await.expect("begin ingress unit of work");
    match MamArchiveRepository::store(&mut transaction, &archive, &message)
        .await
        .expect("store MAM archive in ingress unit of work")
    {
        MamTxStoreOutcome::Inserted(stanza_id) => {
            assert_eq!(stanza_id.id, archive_id);
            assert_eq!(stanza_id.by, jid::Jid::from(archive.clone()));
        }
        outcome => panic!("expected inserted MAM archive row, got {outcome:?}"),
    }
    transaction
        .commit()
        .await
        .expect("commit ingress unit of work");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("read UoW-written MAM archive row");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].id, archive_id);
    assert_eq!(
        result.messages[0].body.as_deref(),
        Some("unit-of-work MAM archive row")
    );

    drop(uow);
    drop(storage);
    drop(db);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated PostgreSQL schema");
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

fn archive_bare() -> jid::BareJid {
    ARCHIVE
        .parse::<jid::BareJid>()
        .expect("valid bare jid literal")
}

fn jid_lit(value: &str) -> Jid {
    value.parse::<Jid>().expect("valid jid literal")
}

#[tokio::test]
async fn xep_0313_full_jid_from_round_trips_through_mam_without_resource_truncation() {
    // Locks the contract that `ArchivedMessage.from`'s typed `Jid`
    // preserves the resource part end-to-end. Pre-commit-6 the field
    // was `String` and the typed projection was lossy via
    // `parse_message_jid`; with `from: Jid` the encoder serializes
    // with `to_string()` once at the SQL bind site and the decoder
    // re-parses via `parse_archived_addressing` once at the row
    // boundary. A regression that truncates to bare JID (e.g. via
    // `to_bare()` on the write side) would fail this test.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let archive = archive_bare();
    let from_full = jid_lit("alice@example.com/laptop");
    let to_room = jid_lit(ARCHIVE);
    let row = ArchivedMessage {
        id: "archive-full-from".to_string(),
        timestamp: chrono::Utc::now(),
        from: from_full.clone(),
        to: to_room.clone(),
        body: Some("typed full from".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-full-from",
            ARCHIVE.parse::<jid::Jid>().expect("valid jid"),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    storage.store_message(&archive, &row).await.expect("store");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let retrieved = &result.messages[0];
    assert_eq!(
        retrieved.from, from_full,
        "full JID `from` (with resource) must round-trip exactly — no bare-ification, no reparse loss"
    );
    assert_eq!(
        retrieved.from.resource().map(|r| r.to_string()).as_deref(),
        Some("laptop"),
        "resource part survives the round-trip"
    );
}

#[tokio::test]
async fn xep_0313_bare_jid_to_round_trips_through_mam() {
    // Mirror of the full-JID test on the `to` side. MUC archive rows
    // typically carry the room's bare JID as `to`; locking the typed
    // round-trip ensures the encoder doesn't accidentally append a
    // resource (e.g. by formatting the writer's full JID instead) and
    // the decoder doesn't split-and-reattach incorrectly.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let archive = archive_bare();
    let from = jid_lit(&format!("{ARCHIVE}/alice"));
    let to_bare = jid_lit(ARCHIVE);
    let row = ArchivedMessage {
        id: "archive-bare-to".to_string(),
        timestamp: chrono::Utc::now(),
        from,
        to: to_bare.clone(),
        body: Some("typed bare to".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-bare-to",
            ARCHIVE.parse::<jid::Jid>().expect("valid jid"),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    storage.store_message(&archive, &row).await.expect("store");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(
        result.messages[0].to, to_bare,
        "bare JID `to` must round-trip exactly"
    );
    assert!(
        result.messages[0].to.resource().is_none(),
        "bare JID must decode without a resource — no spurious resource attachment"
    );
}

#[tokio::test]
async fn xep_0313_decode_rejects_unparseable_from_jid_row() {
    // Q7 / typed-decode hard-error policy: a malformed row whose
    // `from_jid` SQL column does not parse as a [`jid::Jid`] MUST
    // surface as `MamStorageError::Serialization` at the decode
    // boundary. The pre-commit-6 code path collapsed any parse
    // failure to a sentinel `unknown@invalid` JID via
    // `parse_message_jid` — a hot-path data-loss bug that pushed
    // silent garbage into MAM result XML output. Deleting that
    // helper and tightening the storage decode to surface the parse
    // error means DB corruption is visible at the boundary instead
    // of being papered over.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let archive = archive_bare();
    // A value with whitespace + a leading slash is invalid per RFC
    // 7622 (the JID grammar disallows leading slash in resourcepart and
    // bans whitespace anywhere), so this row's `from_jid` will fail
    // typed parse. The value is also obviously corrupt to a human
    // reader of the panic — a real DB-corruption signature, not an
    // edge case of the parser.
    let bad_from = "/not a valid jid/";
    storage
        .insert_raw_from_jid_for_test(&archive, "archive-bad-from", bad_from)
        .await
        .expect("raw insert");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await;
    match result {
        Err(MamStorageError::Serialization(message)) => {
            assert!(
                message.contains("from_jid"),
                "decode error must reference the `from_jid` column; got: {message}"
            );
            assert!(
                message.contains(bad_from),
                "decode error must echo the bad value; got: {message}"
            );
        }
        Err(other) => panic!("expected Serialization error, got: {other:?}"),
        Ok(result) => panic!(
            "decode of unparseable from_jid row must hard-error; got rows: {:?}",
            result.messages
        ),
    }
}

#[tokio::test]
async fn xep_0313_decode_rejects_unknown_message_type_row() {
    // Q7 / typed-decode hard-error policy applied to `message_type`
    // (#228 commit 8). RFC 6121 §5.2.2 makes the `type` attribute a
    // closed set: `chat`, `error`, `groupchat`, `headline`, `normal`.
    // A row whose `message_type` SQL column is outside that set MUST
    // surface as `MamStorageError::Serialization` at the decode
    // boundary rather than silently substituting a default. This test
    // mirrors the from_jid / thread / reply decode-error patterns
    // from commits 4, 5, 6: insert a deliberately malformed row via
    // the raw-insert escape hatch, assert the typed decoder rejects
    // it.
    //
    // Pre-commit-8 the field was `String` and any value round-tripped
    // verbatim into the `ArchivedMessage` struct, including obvious
    // garbage like `"invalid"` — downstream code that compared the
    // string to the wire literal `"groupchat"` etc. would silently
    // misclassify the message. Typing the field as `MessageType` and
    // hard-erroring at the decode boundary makes corruption visible.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let archive = archive_bare();
    let bad_message_type = "invalid";
    storage
        .insert_raw_message_type_for_test(&archive, "archive-bad-mtype", bad_message_type)
        .await
        .expect("raw insert");

    let result = storage
        .query_messages(&archive, MamArchiveKind::Room, &MamQuery::default())
        .await;
    match result {
        Err(MamStorageError::Serialization(message)) => {
            assert!(
                message.contains("message_type"),
                "decode error must reference the `message_type` column; got: {message}"
            );
            assert!(
                message.contains(bad_message_type),
                "decode error must echo the bad value; got: {message}"
            );
        }
        Err(other) => panic!("expected Serialization error, got: {other:?}"),
        Ok(result) => panic!(
            "decode of unknown message_type row must hard-error; got rows: {:?}",
            result.messages
        ),
    }
}

#[tokio::test]
async fn xep_0313_with_filter_does_not_match_domain_prefix_collision() {
    // PR #331 review (Fix 7): the `with` filter MUST match by parsed
    // JID structure, not by textual prefix. The earlier shape used
    // `LIKE '{with}%'` (and `starts_with` in the in-memory backend),
    // which incorrectly matched archived rows whose JID merely shared
    // a textual prefix with the query — e.g. `with=alice@example.com`
    // would falsely match a row with `from=alice@example.com.evil/x`.
    // Per XEP-0313 §4.3.1 a bare `with` matches only the bare form of
    // the archived JID (so the row may be the same bare JID, or that
    // bare JID with any resource); a full `with` matches only exact
    // equality.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let archive = archive_bare();
    let to_room = jid_lit(ARCHIVE);

    // Row 1: legitimate match — bare form equals `alice@example.com`,
    // resource is `web`.
    let legit_from = jid_lit("alice@example.com/web");
    let legit_row = ArchivedMessage {
        id: "archive-with-legit".to_string(),
        timestamp: chrono::Utc::now(),
        from: legit_from.clone(),
        to: to_room.clone(),
        body: Some("legit".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-with-legit",
            ARCHIVE.parse::<jid::Jid>().expect("valid jid"),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    storage
        .store_message(&archive, &legit_row)
        .await
        .expect("store legit");

    // Row 2: malicious prefix collision — `alice@example.com.evil` is
    // a different domain that shares the textual prefix
    // `alice@example.com`.
    let evil_from = jid_lit("alice@example.com.evil/whatever");
    let evil_row = ArchivedMessage {
        id: "archive-with-evil".to_string(),
        timestamp: chrono::Utc::now(),
        from: evil_from.clone(),
        to: to_room.clone(),
        body: Some("evil".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-with-evil",
            ARCHIVE.parse::<jid::Jid>().expect("valid jid"),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    storage
        .store_message(&archive, &evil_row)
        .await
        .expect("store evil");

    // Bare `with` must match the legit row's bare form but NOT the
    // evil row's prefix-colliding bare form.
    let bare_with_query = MamQuery {
        with: Some(jid_lit("alice@example.com")),
        ..MamQuery::default()
    };
    let bare_result = storage
        .query_messages(&archive, MamArchiveKind::Room, &bare_with_query)
        .await
        .expect("query bare with");
    assert_eq!(
        bare_result.messages.len(),
        1,
        "bare `with` must match only the legit alice@example.com row, not the alice@example.com.evil prefix collision; got: {:?}",
        bare_result.messages.iter().map(|m| m.from.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(
        bare_result.messages[0].from, legit_from,
        "bare `with` must match the legit row, not the prefix-colliding evil row"
    );

    // Full `with` must match only exact equality — the legit row's
    // resource is `web`, so a `with=alice@example.com/laptop` query
    // must return zero rows even though the bare form matches.
    let full_with_query = MamQuery {
        with: Some(jid_lit("alice@example.com/laptop")),
        ..MamQuery::default()
    };
    let full_result = storage
        .query_messages(&archive, MamArchiveKind::Room, &full_with_query)
        .await
        .expect("query full with");
    assert!(
        full_result.messages.is_empty(),
        "full `with` (with a resource the archive does not have) must return zero rows; got: {:?}",
        full_result
            .messages
            .iter()
            .map(|m| m.from.to_string())
            .collect::<Vec<_>>()
    );

    // Full `with` exact match: querying for the actual resource on
    // the legit row must return exactly that row.
    let exact_full_with_query = MamQuery {
        with: Some(legit_from.clone()),
        ..MamQuery::default()
    };
    let exact_result = storage
        .query_messages(&archive, MamArchiveKind::Room, &exact_full_with_query)
        .await
        .expect("query exact full with");
    assert_eq!(
        exact_result.messages.len(),
        1,
        "full `with` matching the exact archived JID must return that row"
    );
    assert_eq!(exact_result.messages[0].from, legit_from);
}

// =========================================================================
// XEP-0313 §5.1 MUC archive access gate (#1093)
// =========================================================================

const NS_CLIENT: &str = "jabber:client";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_XDATA: &str = "jabber:x:data";
const MUC_ROOMCONFIG_FORM: &str = "http://jabber.org/protocol/muc#roomconfig";
const ARCHIVE_PROBE_BODY: &str = "archive gate probe";

/// Serialize a `minidom::Element` to a wire frame. The XML-generation
/// hard rule bans `format!`-built stanzas even in tests, so stanzas are
/// composed with typed `minidom` builders and serialized here.
fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    String::from_utf8(bytes).expect("XML serialization is UTF-8")
}

fn attr_name(name: &'static str) -> &'static minidom::rxml::NcNameStr {
    name.try_into().expect("valid ncname")
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

/// Join (creating an instant room), set the given members-only state via
/// the XEP-0045 §10.2 owner-config form, and archive one message.
async fn create_room_with_message(client: &mut WsXmppClient, room: &str, members_only: bool) {
    let join = Element::builder("presence", NS_CLIENT)
        .attr(attr_name("to").to_owned(), format!("{room}/{USERNAME}"))
        .append(Element::builder("x", NS_MUC).build())
        .build();
    client.send(&element_to_xml(join)).await.expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    let cfg_id = format!("cfg-{}", uuid::Uuid::new_v4());
    let members_only_value = if members_only { "1" } else { "0" };
    let form = Element::builder("x", NS_XDATA)
        .attr(attr_name("type").to_owned(), "submit")
        .append(data_form_field(
            "FORM_TYPE",
            Some("hidden"),
            MUC_ROOMCONFIG_FORM,
        ))
        .append(data_form_field(
            "muc#roomconfig_membersonly",
            None,
            members_only_value,
        ))
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
    let cfg_response = client
        .recv_matching(|f| f.contains("<iq") && f.contains(&cfg_id))
        .await
        .expect("owner config response");
    assert!(
        cfg_response.contains("result"),
        "owner config must be accepted: {cfg_response}"
    );

    let message = Element::builder("message", NS_CLIENT)
        .attr(attr_name("type").to_owned(), "groupchat")
        .attr(attr_name("to").to_owned(), room.to_owned())
        .append(
            Element::builder("body", NS_CLIENT)
                .append(ARCHIVE_PROBE_BODY)
                .build(),
        )
        .build();
    client
        .send(&element_to_xml(message))
        .await
        .expect("send message");
    client
        .recv_matching(|f| f.contains(ARCHIVE_PROBE_BODY))
        .await
        .expect("echo");
}

/// XEP-0313 §5.1: "In a members-only chat room, only owners, admins or
/// members can query a room archive." A non-member MUST get <forbidden/>
/// before any archive read; the owner keeps full access.
#[tokio::test]
async fn xep_0313_members_only_room_archive_requires_membership() {
    let _guard = TEST_SERIAL.lock().await;
    let mallory_pass = format!("mallory-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("mallory", &mallory_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_pass,
        &format!("test-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connect");

    let room = format!("members-only-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    create_room_with_message(&mut admin, &room, true).await;

    // Owner (member+) retains archive access.
    let owner_q = format!("q-owner-{}", uuid::Uuid::new_v4());
    let owner_frames = query_mam(&mut admin, &mam_query_xml(&owner_q, &room, Some(10)))
        .await
        .expect("owner MAM query");
    assert!(
        owner_frames
            .iter()
            .any(|f| f.contains("archive gate probe")),
        "room owner must still read the members-only archive: {owner_frames:?}"
    );

    // Non-member is forbidden.
    let mut mallory = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "mallory",
        &mallory_pass,
        &format!("test-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("mallory connect");
    let mallory_q = format!("q-mallory-{}", uuid::Uuid::new_v4());
    mallory
        .send(&mam_query_xml(&mallory_q, &room, Some(10)))
        .await
        .expect("send non-member MAM query");
    let denial = mallory
        .recv_matching(|f| f.contains(&mallory_q))
        .await
        .expect("non-member MAM reply");
    assert!(
        denial.contains("<forbidden"),
        "non-member MAM query on a members-only room must be forbidden: {denial}"
    );
    assert!(
        !denial.contains("archive gate probe"),
        "forbidden reply must not leak archived content: {denial}"
    );

    let _ = mallory.close().await;
    let _ = admin.close().await;
}

/// XEP-0313 §5.1: "In the case of open MUC rooms, the MUC archives can
/// generally be accessed by any users (including those who have never
/// entered the room) who do not have an affiliation of 'outcast'."
#[tokio::test]
async fn xep_0313_open_room_archive_accessible_to_non_member() {
    let _guard = TEST_SERIAL.lock().await;
    let mallory_pass = format!("mallory-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("mallory", &mallory_pass)]);
    let admin_pass = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &admin_pass,
        &format!("test-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connect");

    let room = format!("open-room-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    create_room_with_message(&mut admin, &room, false).await;

    let mut mallory = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "mallory",
        &mallory_pass,
        &format!("test-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("mallory connect");
    let mallory_q = format!("q-open-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut mallory, &mam_query_xml(&mallory_q, &room, Some(10)))
        .await
        .expect("open-room MAM query by non-member");
    assert!(
        frames.iter().any(|f| f.contains("archive gate probe")),
        "open-room archive must stay readable for non-outcast non-members: {frames:?}"
    );

    let _ = mallory.close().await;
    let _ = admin.close().await;
}
