//! XEP-0313 MAM integration tests over WebSocket.
//!
//! Each test starts its own isolated waddle-server on dynamic ports.

mod ws_common;

use jid::Jid;
use tokio::sync::Mutex;
use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamStorage, MamStorageError, SqlxMamStorage};
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::message::MessageType;

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

    client.close().await;
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

    client.close().await;
}

/// Returns the contents of the inner `<message>`'s `<body>` element
/// inside a single MAM `<forwarded>` frame, distinguishing three
/// wire shapes:
///
/// - `Some(Some(text))` — `<body>text</body>` (or self-closing
///   `<body/>` parses as the empty string).
/// - `Some(None)` — no `<body>` element on the inner message at all.
/// - `None` — the frame has no inner `<message>` at all (caller bug).
///
/// Operates only on the **inner** `<message>` element of a MAM result
/// frame so it isn't fooled by the outer `<message>` wrapper. Hand-rolled
/// rather than pulling in a full XML parser to keep the integration test
/// dependency-light, mirroring the existing `extract_mam_body` helper.
fn extract_inner_body_presence(frame: &str) -> Option<Option<String>> {
    let forwarded_start = frame.find("<forwarded")?;
    let inner_msg_start = frame[forwarded_start..]
        .find("<message")
        .map(|i| i + forwarded_start)?;
    let inner_msg_end = frame[inner_msg_start..]
        .find("</message>")
        .map(|i| i + inner_msg_start + "</message>".len())?;
    let inner = &frame[inner_msg_start..inner_msg_end];

    // Look for `<body` with either `>` (open) or `/>` (self-closing) or
    // attributes. Distinguish from a nested `<body/>` inside another
    // namespace (e.g. XEP-0428 `<fallback>` carries its own
    // `<body start='..' end='..'/>`); restrict to direct children of the
    // inner `<message>` by requiring the tag to live at depth-1.
    //
    // Cheap depth tracking: scan forward and count nesting depth, only
    // matching `<body` when depth == 0.
    let mut depth: i32 = 0;
    let mut idx = 0usize;
    let bytes = inner.as_bytes();
    while idx < bytes.len() {
        if bytes[idx] == b'<' {
            // Closing tag drops depth before we read a new one.
            if idx + 1 < bytes.len() && bytes[idx + 1] == b'/' {
                depth -= 1;
                idx += 1;
                continue;
            }
            // The outermost `<message ...>` itself opens at depth 0
            // — only inspect children (depth == 1 after we step in).
            if depth == 1 && inner[idx..].starts_with("<body") {
                let after_tag = idx + "<body".len();
                // Find end of the body open tag.
                let close_rel = inner[after_tag..].find('>')?;
                let close = after_tag + close_rel;
                let is_self_close = bytes[close - 1] == b'/';
                if is_self_close {
                    // `<body/>` — empty body element.
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
    //
    // Earlier denormalization collapsed (2) and (3) into the empty
    // string via `.unwrap_or_default()`, so consumers reading the
    // typed `body` field saw a misleading "empty body" for stanzas
    // that had no `<body>` element at all (subject-only,
    // reaction-only, etc.). This test locks the wire-level
    // distinction end-to-end through the MUC archive.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("body-fidelity-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Join MUC.
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

    // Distinct id-prefix per case so we can pick out the right frame.
    // Reactions need a target message id — the textual root msg gives
    // the reaction-only frames a stable target.
    let id_text = format!("body-text-{}", uuid::Uuid::new_v4());
    let id_empty = format!("body-empty-{}", uuid::Uuid::new_v4());
    let id_absent = format!("body-absent-{}", uuid::Uuid::new_v4());
    let body_text = format!("hello-{}", uuid::Uuid::new_v4());

    // Case 1: text body.
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

    // Case 2: empty body element on the wire. `is_archivable` for
    // groupchat treats any non-empty `bodies` collection as
    // archivable (XEP-0313 §5.1.3 allowance for groupchat), so an
    // empty `<body></body>` round-trips as Some("").
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

    // Case 3: no `<body>` element at all. A reaction is the simplest
    // bodyless archivable groupchat message (XEP-0444; the room
    // archive treats reactions as archivable per `is_archivable`).
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

    // Query the room archive.
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
        "Case 3 (no <body> element) must replay with NO <body> element on the inner message; bug: it was being materialized as an empty body. Frame: {frame_absent}"
    );

    client.close().await;
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

    client.close().await;
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
        .query_messages(&archive, &MamQuery::default())
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
        .query_messages(&archive, &MamQuery::default())
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

    let result = storage.query_messages(&archive, &MamQuery::default()).await;
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

    let result = storage.query_messages(&archive, &MamQuery::default()).await;
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
