//! XEP-0461 reply integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamStorage, MamStorageError, SqlxMamStorage};
use waddle_xmpp_core::mam::{ArchivedReply, RichMessageId};
use ws_common::{extract_attr_after, TestServer, WsXmppClient};
use xmpp_parsers::message::MessageType;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0461-{}", uuid::Uuid::new_v4());
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

fn stanza_id(frame: &str) -> String {
    extract_attr_after(frame, "stanza-id", "id").expect("stanza-id id")
}

#[tokio::test]
async fn reply_routes_and_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>question</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("question"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-1">
                <body>answer</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="{target}"/>
            </message>"#
        ))
        .await
        .expect("send reply");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:reply:0"))
        .await
        .expect("reply echo");
    assert!(echo.contains(&target), "missing reply target: {echo}");

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reply" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reply") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains("urn:xmpp:reply:0") && frame.contains(&target)),
        "MAM did not replay reply: {frames:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn reply_to_unknown_target_routes_without_error() {
    // XEP-0461 imposes no server-side target-existence requirement
    // ("It is up to receiving entities…"). The previous implementation
    // returned `<item-not-found/>` when the server hadn't archived the
    // referenced message, which would reject legitimate cross-server
    // replies, replies to messages before retention, or replies to
    // client-cached history we never saw. Verify that a well-formed
    // reply to an unknown id is routed normally.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-unknown-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-orphan">
                <body>orphan reply</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="never-archived-id"/>
            </message>"#
        ))
        .await
        .expect("send reply to unknown target");
    let echo = client
        .recv_matching(|frame| frame.contains("orphan reply"))
        .await
        .expect("reply echo");
    assert!(
        echo.contains("urn:xmpp:reply:0"),
        "reply payload missing: {echo}"
    );
    assert!(
        !echo.contains("<item-not-found"),
        "spec-non-conformant rejection: {echo}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn reply_with_empty_to_jid_returns_bad_request() {
    // XEP-0461 §Use Cases: if the optional `to` attribute is present it
    // names the author of the referenced message, so it must be a valid JID.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-bad-to-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-bad-to">
                <body>bad reply</body>
                <reply xmlns="urn:xmpp:reply:0" to=" " id="parent-1"/>
            </message>"#
        ))
        .await
        .expect("send malformed reply");
    let error = client
        .recv_matching(|frame| frame.contains("<bad-request"))
        .await
        .expect("bad-request error");
    assert!(
        error.contains("type='error'"),
        "not an error stanza: {error}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn reply_with_fallback_replays_from_mam() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("reply-fb-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="orig-1"><body>original message</body></message>"#
        ))
        .await
        .expect("send original");
    let target = stanza_id(
        &client
            .recv_matching(|frame| frame.contains("original message"))
            .await
            .expect("original echo"),
    );

    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="reply-fb-1">
                <body>&gt; original message\nmy reply</body>
                <reply xmlns="urn:xmpp:reply:0" to="{room}/{USERNAME}" id="{target}"/>
                <fallback xmlns="urn:xmpp:fallback:0" for="urn:xmpp:reply:0">
                    <body start="0" end="20"/>
                </fallback>
            </message>"#
        ))
        .await
        .expect("send reply with fallback");
    let echo = client
        .recv_matching(|frame| frame.contains("urn:xmpp:fallback:0"))
        .await
        .expect("reply fallback echo");
    assert!(
        echo.contains("urn:xmpp:reply:0"),
        "reply echo missing reply element: {echo}"
    );
    assert!(
        echo.contains("urn:xmpp:fallback:0"),
        "reply echo missing fallback element: {echo}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="mam-reply-fb" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send MAM");
    let frames = client
        .recv_until(|frame| frame.contains("mam-reply-fb") && frame.contains("<fin"))
        .await
        .expect("MAM frames");
    let reply_mam_frame = frames
        .iter()
        .find(|frame| frame.contains("urn:xmpp:reply:0"))
        .expect("MAM should replay the reply message");
    assert!(
        reply_mam_frame.contains("urn:xmpp:fallback:0"),
        "MAM replay should preserve fallback element: {reply_mam_frame}"
    );
    assert!(
        reply_mam_frame.contains("for='urn:xmpp:reply:0'"),
        "MAM replay should preserve fallback 'for' attribute: {reply_mam_frame}"
    );

    let _ = client.close().await;
}

const STORAGE_ARCHIVE: &str = "room@conference.example.com";

fn storage_archive_bare() -> jid::BareJid {
    STORAGE_ARCHIVE
        .parse::<jid::BareJid>()
        .expect("valid bare jid literal")
}

fn reply_archived_row(
    archive_id: &str,
    body: &str,
    reply: Option<ArchivedReply>,
) -> ArchivedMessage {
    ArchivedMessage {
        id: archive_id.to_string(),
        timestamp: chrono::Utc::now(),
        from: format!("{STORAGE_ARCHIVE}/alice")
            .parse::<jid::Jid>()
            .expect("valid full jid"),
        to: STORAGE_ARCHIVE.parse::<jid::Jid>().expect("valid bare jid"),
        body: Some(body.to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            format!("wire-{archive_id}"),
            STORAGE_ARCHIVE.parse::<jid::Jid>().expect("valid jid"),
        )),
        thread: None,
        reply,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    }
}

#[tokio::test]
async fn xep_0461_collapsed_reply_field_round_trips_bare_to_jid_through_storage() {
    // #228 commit 5: `ArchivedMessage.reply: Option<ArchivedReply>`
    // collapses the previous flat (`reply_to_id`, `reply_to_jid`)
    // pair. SQL schema is unchanged (still two columns plus the
    // `idx_mam_room_reply_to` index); encode splits, decode combines.
    // Locks the typed-struct round-trip end to end so the field-level
    // collapse never silently regresses to the flat shape. Bare JID
    // case: `to = "juliet@capulet.lit"`.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let original = ArchivedReply {
        id: RichMessageId::new("reply-target-bare").expect("non-empty reply id"),
        to: Some("juliet@capulet.lit".parse::<jid::Jid>().expect("valid jid")),
    };
    let row = reply_archived_row("archive-bare-to", "bare to", Some(original.clone()));
    storage
        .store_message(&storage_archive_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&storage_archive_bare(), &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(
        result.messages[0].reply.as_ref(),
        Some(&original),
        "the typed ArchivedReply struct must round-trip exactly for bare `to` JIDs"
    );
}

#[tokio::test]
async fn xep_0461_collapsed_reply_field_round_trips_full_to_jid_through_storage() {
    // Full JID (with resource) must round-trip identically to the bare
    // case — the storage layer must not bare-ify `to` on encode and the
    // decode side must accept the full form.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let original = ArchivedReply {
        id: RichMessageId::new("reply-target-full").expect("non-empty reply id"),
        to: Some(
            "romeo@montague.lit/orchard"
                .parse::<jid::Jid>()
                .expect("valid full jid"),
        ),
    };
    let row = reply_archived_row("archive-full-to", "full to", Some(original.clone()));
    storage
        .store_message(&storage_archive_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&storage_archive_bare(), &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let reply = result.messages[0]
        .reply
        .as_ref()
        .expect("reply must round-trip as Some");
    assert_eq!(reply, &original);
    assert_eq!(
        reply.to.as_ref().map(|jid| jid.to_string()),
        Some("romeo@montague.lit/orchard".to_string()),
        "full JID resource must survive the round-trip"
    );
}

#[tokio::test]
async fn xep_0461_collapsed_reply_field_round_trips_no_to_jid_through_storage() {
    // XEP-0461 §3 makes `to` SHOULD, not MUST: a reply with only `id`
    // is well-formed. Must round-trip as `Some(ArchivedReply { id,
    // to: None })`, never as `None`.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let original = ArchivedReply {
        id: RichMessageId::new("reply-target-no-to").expect("non-empty reply id"),
        to: None,
    };
    let row = reply_archived_row("archive-no-to", "no to", Some(original.clone()));
    storage
        .store_message(&storage_archive_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&storage_archive_bare(), &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let reply = result.messages[0]
        .reply
        .as_ref()
        .expect("id-only reply must round-trip as Some");
    assert_eq!(reply, &original);
    assert!(
        reply.to.is_none(),
        "reply without `to` must decode with `to = None`, not as `reply: None`"
    );
}

#[tokio::test]
async fn xep_0461_decode_rejects_orphan_reply_to_jid_row() {
    // Q7 hard-error policy (mirroring the XEP-0201 thread orphan
    // contract): a malformed row with `reply_to_id IS NULL` but
    // `reply_to_jid` set is incoherent (XEP-0461 §3 makes `id` MUST
    // and `to` SHOULD — a `to` without an `id` cannot identify the
    // replied-to message, so the row leaks a sender JID with no way
    // to associate it back to a message). The collapsed
    // `Option<ArchivedReply>` field would otherwise paper over the
    // corruption by silently dropping the orphan `to`. Decode MUST
    // surface this as a serialization error so DB corruption is
    // visible at the boundary.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    storage
        .insert_raw_reply_columns_for_test(
            &storage_archive_bare(),
            "archive-orphan-reply",
            None,
            Some("juliet@capulet.lit"),
        )
        .await
        .expect("raw insert");

    let result = storage
        .query_messages(&storage_archive_bare(), &MamQuery::default())
        .await;
    match result {
        Err(MamStorageError::Serialization(message)) => {
            assert!(
                message.contains("orphan"),
                "decode error must mention the orphan condition; got: {message}"
            );
            assert!(
                message.contains("reply_to_jid"),
                "decode error must reference the leak-prone column; got: {message}"
            );
        }
        Err(other) => panic!("expected Serialization error, got: {other:?}"),
        Ok(result) => panic!(
            "decode of orphan reply_to_jid row must hard-error; got rows: {:?}",
            result.messages
        ),
    }
}
