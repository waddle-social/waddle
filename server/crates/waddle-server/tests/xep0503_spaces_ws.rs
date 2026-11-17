//! XEP-0503 Spaces wire-conformance integration tests over WebSocket,
//! including the identity profile for XEP-0402 room item ids.

use waddle_ws_test_support as ws_common;

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, ToElement};
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, resource)
        .await
        .expect("admin connect")
}

fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut buf = Vec::new();
    stanza
        .to_element()
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Get {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload,
    };
    client
        .send(&stanza_to_xml(&Stanza::Iq(Box::new(iq))))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame_has_iq_id(frame, id) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, payload: Element) -> String {
    let iq = Iq::Set {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload,
    };
    client
        .send(&stanza_to_xml(&Stanza::Iq(Box::new(iq))))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame_has_iq_id(frame, id) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

#[tokio::test]
async fn spaces_service_disco_info_advertises_xep0503_features() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-service-info").await;

    let info = iq_get_to(
        &mut admin,
        "spaces-info",
        "spaces.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;

    for feature in [
        "urn:xmpp:spaces:0",
        "http://jabber.org/protocol/pubsub#retrieve-items",
        "http://jabber.org/protocol/pubsub#multi-items",
        "http://jabber.org/protocol/pubsub#manage-subscriptions",
        "http://jabber.org/protocol/pubsub#modify-affiliations",
        "http://jabber.org/protocol/pubsub#retrieve-affiliations",
    ] {
        assert!(
            info.contains(feature),
            "Spaces service disco#info missing {feature}: {info}"
        );
    }
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_owner_subscriptions_round_trip() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-owner-subs").await;

    let set = iq_set_to(
        &mut admin,
        "spaces-subs-set",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub#owner")
            .append(
                Element::builder("subscriptions", "http://jabber.org/protocol/pubsub#owner")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .append(
                        Element::builder("subscription", "http://jabber.org/protocol/pubsub#owner")
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@localhost",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("subscription").to_owned(),
                                "subscribed",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        set.contains("type='result'") || set.contains("type=\"result\""),
        "owner subscriptions set should succeed: {set}"
    );

    let get = iq_get_to(
        &mut admin,
        "spaces-subs-get",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub#owner")
            .append(
                Element::builder("subscriptions", "http://jabber.org/protocol/pubsub#owner")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        get.contains("alice@localhost") && get.contains("subscription='subscribed'"),
        "owner subscriptions get should include added subscriber: {get}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn seeded_general_space_lists_bookmarked_rooms() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-seeded").await;

    let spaces = iq_get_to(
        &mut admin,
        "spaces-items",
        "spaces.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#items").build(),
    )
    .await;
    assert!(
        spaces.contains("node='general'"),
        "expected seeded general space node: {spaces}"
    );

    let items = iq_get_to(
        &mut admin,
        "general-items",
        "spaces.localhost",
        Element::builder("pubsub", "http://jabber.org/protocol/pubsub")
            .append(
                Element::builder("items", "http://jabber.org/protocol/pubsub")
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "general")
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        items.contains("chat@muc.localhost") && items.contains("announcements@muc.localhost"),
        "expected seeded room bookmarks: {items}"
    );
    assert!(
        items.contains("conference") && items.contains("urn:xmpp:bookmarks:1"),
        "expected XEP-0402 conference bookmark items: {items}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn room_disco_advertises_parent_space_metadata() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-room-disco").await;

    let info = iq_get_to(
        &mut admin,
        "chat-info",
        "chat@muc.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;

    assert!(
        info.contains("var='urn:xmpp:spaces:0'"),
        "expected XEP-0503 feature: {info}"
    );
    assert!(
        info.contains("<value>urn:xmpp:spaces:0</value>")
            && info.contains("var='parent'")
            && info.contains("xmpp:spaces.localhost?;node=general"),
        "expected XEP-0503 parent data form: {info}"
    );
    assert!(
        info.contains("http://jabber.org/protocol/muc#roominfo")
            && info.contains("muc#roomconfig_pubsub")
            && info.contains("xmpp:spaces.localhost?;node=general"),
        "expected MUC roominfo pubsub compatibility field: {info}"
    );
    let _ = admin.close().await;
}

fn is_bad_request_error(frame: &str) -> bool {
    let Ok(response) = frame.parse::<Element>() else {
        return false;
    };
    response.attr("type") == Some("error")
        && response
            .get_child("error", "jabber:client")
            .is_some_and(|error| {
                error
                    .get_child("bad-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
                    .is_some()
            })
}

fn space_publish(node: &str, item_id: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("publish", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                        .append(
                            Element::builder("conference", "urn:xmpp:bookmarks:1")
                                .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Chat")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

fn space_retract(node: &str, item_id: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("retract", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                        .build(),
                )
                .build(),
        )
        .build()
}

async fn space_items(admin: &mut WsXmppClient, id: &str, node: &str) -> String {
    iq_get_to(
        admin,
        id,
        "spaces.localhost",
        Element::builder("pubsub", NS_PUBSUB)
            .append(
                Element::builder("items", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                    .build(),
            )
            .build(),
    )
    .await
}

/// Create a second Space node beside the seeded `general`.
async fn create_space_node(admin: &mut WsXmppClient, node: &str) {
    let response = iq_set_to(
        admin,
        &format!("create-{node}"),
        "spaces.localhost",
        Element::builder("pubsub", NS_PUBSUB)
            .append(
                Element::builder("create", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                    .build(),
            )
            .build(),
    )
    .await;
    assert!(
        is_result(&response),
        "creating Space node {node} must succeed: {response}"
    );
}

fn is_result(frame: &str) -> bool {
    frame
        .parse::<Element>()
        .is_ok_and(|response| response.attr("type") == Some("result"))
}

fn item_ids(frame: &str) -> Vec<String> {
    let response: Element = frame.parse().expect("items response XML");
    response
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .map(|items| {
            items
                .children()
                .filter_map(|item| item.attr("id").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Spellings of the seeded `chat@muc.localhost` that the `jid` parser
/// would fold onto it. The item id is stored and matched as written, so
/// each would otherwise key a second item for the same room.
const NON_CANONICAL_CHAT_IDS: [&str; 3] = [
    "Chat@muc.localhost",
    "chat@MUC.localhost",
    "chat@muc.localhost.",
];

#[tokio::test]
async fn spaces_publish_refuses_a_non_canonical_room_item_id() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-publish-non-canonical").await;
    // Publishing the room into another Space would move it there, so a
    // folded id accepted on `second` would give `chat` a second parent.
    create_space_node(&mut admin, "second").await;

    for (index, item_id) in NON_CANONICAL_CHAT_IDS.iter().enumerate() {
        let id = format!("spaces-publish-non-canonical-{index}");
        let response = iq_set_to(
            &mut admin,
            &id,
            "spaces.localhost",
            space_publish("second", item_id),
        )
        .await;
        assert!(
            is_bad_request_error(&response),
            "publishing {item_id:?} must be refused with <bad-request/>: {response}"
        );
    }

    let second = space_items(&mut admin, "second-items-after-publish", "second").await;
    assert!(
        item_ids(&second).is_empty(),
        "no item may be stored on the second Space: {second}"
    );
    let general = space_items(&mut admin, "general-items-after-publish", "general").await;
    assert!(
        item_ids(&general).contains(&"chat@muc.localhost".to_owned()),
        "the canonical room item must remain on general: {general}"
    );

    // The channel keeps exactly one parent space.
    let info = iq_get_to(
        &mut admin,
        "chat-info-after-publish",
        "chat@muc.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;
    assert!(
        info.contains("xmpp:spaces.localhost?;node=general"),
        "the room must keep its general parent: {info}"
    );
    assert!(
        !info.contains("xmpp:spaces.localhost?;node=second"),
        "the room must not name the second Space as a parent: {info}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn spaces_retract_refuses_a_non_canonical_room_item_id() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-retract-non-canonical").await;

    for (index, item_id) in NON_CANONICAL_CHAT_IDS.iter().enumerate() {
        let id = format!("spaces-retract-non-canonical-{index}");
        let response = iq_set_to(
            &mut admin,
            &id,
            "spaces.localhost",
            space_retract("general", item_id),
        )
        .await;
        assert!(
            is_bad_request_error(&response),
            "retracting {item_id:?}, which names no stored item, must be refused with <bad-request/>: {response}"
        );
    }

    let items = space_items(&mut admin, "spaces-items-after-retract", "general").await;
    assert!(
        items.contains("chat@muc.localhost"),
        "the canonical room item must remain: {items}"
    );
    let _ = admin.close().await;
}

// ---------------------------------------------------------------------------
// Items stored under a non-canonical id. Earlier releases stored Space item
// ids as written, so the store can hold `Chat@muc.localhost` on a Space
// node, with the parent tuple its publish gave `chat` on that node. These
// tests seed that state in a persistent SQLite file shared by the global
// and PubSub stores.
// ---------------------------------------------------------------------------

/// A server whose global and PubSub stores are one SQLite file the test
/// can seed, and a pool on that file.
async fn persistent_server(directory: &tempfile::TempDir) -> (TestServer, SqlitePool) {
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("spaces.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_envs(
        &url,
        &[],
        &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &url)],
    );
    let options = SqliteConnectOptions::from_str(&url)
        .expect("SQLite URL")
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("seed pool");
    (server, pool)
}

/// Store `item_id` on Space `node` as written, bypassing publish.
async fn seed_space_item(pool: &SqlitePool, node: &str, item_id: &str) {
    sqlx::query(
        "INSERT INTO pubsub_items \
         (owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms) \
         VALUES ('spaces.localhost', ?, ?, ?, NULL, 0)",
    )
    .bind(node)
    .bind(item_id)
    .bind(String::from(
        &Element::builder("conference", "urn:xmpp:bookmarks:1")
            .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Chat")
            .build(),
    ))
    .execute(pool)
    .await
    .expect("seed Space item");
}

/// Give channel `chat` a parent tuple on Space `node`, as publishing a
/// bookmark for it there did, by copying its seeded `general` tuple.
async fn seed_chat_parent_tuple(pool: &SqlitePool, node: &str) {
    let inserted = sqlx::query(
        "INSERT INTO permission_tuples \
         (id, object_type, object_id, relation, subject_type, subject_id, subject_relation) \
         SELECT ?, object_type, object_id, relation, subject_type, ?, subject_relation \
         FROM permission_tuples \
         WHERE object_type = 'channel' AND object_id = 'chat' AND relation = 'parent' \
           AND subject_type = 'space' AND subject_id = 'general'",
    )
    .bind(format!("seeded-chat-parent-{node}"))
    .bind(node)
    .execute(pool)
    .await
    .expect("seed parent tuple")
    .rows_affected();
    assert_eq!(
        inserted, 1,
        "chat must have its seeded general parent tuple"
    );
}

async fn chat_parent_tuples(pool: &SqlitePool, node: &str) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) FROM permission_tuples \
         WHERE object_type = 'channel' AND object_id = 'chat' AND relation = 'parent' \
           AND subject_type = 'space' AND subject_id = ?",
    )
    .bind(node)
    .fetch_one(pool)
    .await
    .expect("count parent tuples")
    .get(0)
}

async fn stored_space_items(pool: &SqlitePool, node: &str, item_id: &str) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) FROM pubsub_items \
         WHERE owner_jid = 'spaces.localhost' AND node_name = ? AND item_id = ?",
    )
    .bind(node)
    .bind(item_id)
    .fetch_one(pool)
    .await
    .expect("count Space items")
    .get(0)
}

#[tokio::test]
async fn spaces_retract_removes_a_stored_non_canonical_item_and_its_parent_grant() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-retract-stored-non-canonical").await;
    create_space_node(&mut admin, "second").await;
    seed_space_item(&pool, "second", "Chat@muc.localhost").await;
    seed_chat_parent_tuple(&pool, "second").await;

    let response = iq_set_to(
        &mut admin,
        "retract-stored-non-canonical",
        "spaces.localhost",
        space_retract("second", "Chat@muc.localhost"),
    )
    .await;
    assert!(
        is_result(&response),
        "the stored non-canonical item must be retracted by its exact id: {response}"
    );
    assert_eq!(
        stored_space_items(&pool, "second", "Chat@muc.localhost").await,
        0
    );
    assert_eq!(
        chat_parent_tuples(&pool, "second").await,
        0,
        "the parent grant the item held on the second Space is revoked"
    );
    assert_eq!(
        chat_parent_tuples(&pool, "general").await,
        1,
        "the canonical parent grant on general remains"
    );
    let general = space_items(&mut admin, "general-items-after-stored-retract", "general").await;
    assert!(
        item_ids(&general).contains(&"chat@muc.localhost".to_owned()),
        "the canonical room item on general remains: {general}"
    );

    // Another spelling names no stored item and is still refused.
    let response = iq_set_to(
        &mut admin,
        "retract-unstored-non-canonical",
        "spaces.localhost",
        space_retract("second", "CHAT@muc.localhost"),
    )
    .await;
    assert!(
        is_bad_request_error(&response),
        "a non-canonical id that names no stored item is refused: {response}"
    );
    let _ = admin.close().await;
    pool.close().await;
}

#[tokio::test]
async fn spaces_retract_of_a_non_canonical_item_keeps_the_grant_a_canonical_item_holds() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-retract-beside-canonical").await;
    // `general` holds `chat@muc.localhost` and its parent tuple already.
    seed_space_item(&pool, "general", "Chat@muc.localhost").await;

    let response = iq_set_to(
        &mut admin,
        "retract-beside-canonical",
        "spaces.localhost",
        space_retract("general", "Chat@muc.localhost"),
    )
    .await;
    assert!(
        is_result(&response),
        "the stored non-canonical item must be retracted by its exact id: {response}"
    );
    assert_eq!(
        stored_space_items(&pool, "general", "Chat@muc.localhost").await,
        0
    );
    assert_eq!(
        stored_space_items(&pool, "general", "chat@muc.localhost").await,
        1
    );
    assert_eq!(
        chat_parent_tuples(&pool, "general").await,
        1,
        "the canonical item still names chat, so its parent grant remains"
    );
    let _ = admin.close().await;
    pool.close().await;
}

#[tokio::test]
async fn spaces_delete_does_not_destroy_a_room_only_a_non_canonical_item_names() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-delete-non-canonical").await;
    create_space_node(&mut admin, "second").await;
    // The state publishing `Chat@muc.localhost` into `second` left: the
    // item, a parent tuple for `chat` on `second`, and `chat`'s link moved
    // to `second`.
    seed_space_item(&pool, "second", "Chat@muc.localhost").await;
    seed_chat_parent_tuple(&pool, "second").await;
    sqlx::query(
        "INSERT OR REPLACE INTO channel_space_links \
         (channel_jid, space_jid, space_node, created_at) \
         VALUES ('chat@muc.localhost', 'second@spaces.localhost', 'second', 0)",
    )
    .execute(&pool)
    .await
    .expect("seed channel-space link");

    let delete = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:waddle:admin:spaces:delete:0",
        )
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(
            DataForm::new(FormType::Submit)
                .add_field(Field::form_type("urn:waddle:admin:spaces:delete:0"))
                .add_field(Field::text_single("space_jid", "second@spaces.localhost"))
                .add_field(Field::text_single("confirm", "yes"))
                .to_element(),
        )
        .build();
    let response = iq_set_to(&mut admin, "delete-second", DOMAIN, delete).await;
    assert!(
        is_result(&response),
        "spaces:delete must succeed: {response}"
    );

    assert_eq!(
        chat_parent_tuples(&pool, "second").await,
        0,
        "the parent grant on the deleted Space is gone"
    );
    assert_eq!(chat_parent_tuples(&pool, "general").await, 1);
    let channels: i64 = sqlx::query("SELECT COUNT(*) FROM channels WHERE id = 'chat'")
        .fetch_one(&pool)
        .await
        .expect("count channels")
        .get(0);
    assert_eq!(channels, 1, "the chat channel must not be destroyed");
    let info = iq_get_to(
        &mut admin,
        "chat-info-after-delete",
        "chat@muc.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;
    assert!(
        is_result(&info),
        "the chat room must still exist after the delete: {info}"
    );
    let general = space_items(&mut admin, "general-items-after-delete", "general").await;
    assert!(
        item_ids(&general).contains(&"chat@muc.localhost".to_owned()),
        "chat stays bookmarked in general: {general}"
    );
    let _ = admin.close().await;
    pool.close().await;
}

// ---------------------------------------------------------------------------
// A session's Space and channel on a server as startup builds it, and what
// `spaces:delete` destroys.
//
// `TestServer` runs the server binary, whose startup builds the permission
// actor deciding issuance of the privileged relations (ADR-0020 D8,
// `with_replica_privileged_issuance`): the channel and Space owner
// relations, among others, are written only for accounts of the served
// domain and of trusted homes.
// ---------------------------------------------------------------------------

const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_XDATA: &str = "jabber:x:data";

/// How many relationships `object_type:object_id#relation@subject_type:subject_id`
/// the deployment's store holds.
async fn relationships(
    pool: &SqlitePool,
    object: (&str, &str),
    relation: &str,
    subject: (&str, &str),
) -> i64 {
    sqlx::query(
        "SELECT COUNT(*) FROM permission_tuples \
         WHERE object_type = ? AND object_id = ? AND relation = ? \
           AND subject_type = ? AND subject_id = ?",
    )
    .bind(object.0)
    .bind(object.1)
    .bind(relation)
    .bind(subject.0)
    .bind(subject.1)
    .fetch_one(pool)
    .await
    .expect("count relationships")
    .get(0)
}

async fn catalog_channels(pool: &SqlitePool, id: &str) -> i64 {
    sqlx::query("SELECT COUNT(*) FROM channels WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count channels")
        .get(0)
}

/// The admin creates the room `id` by joining it, and configures it, which
/// makes it a channel of the catalog whose owner the admin is (XEP-0045
/// §10.1).
async fn create_room(admin: &mut WsXmppClient, id: &str) {
    let room = format!("{id}@muc.{DOMAIN}");
    let join = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/{USERNAME}"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build();
    admin.send(&String::from(&join)).await.expect("send join");
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("the join is answered");
    let form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append("http://jabber.org/protocol/muc#roomconfig")
                        .build(),
                )
                .build(),
        )
        .build();
    let configured = iq_set_to(
        admin,
        &format!("configure-{id}"),
        &room,
        Element::builder("query", NS_MUC_OWNER).append(form).build(),
    )
    .await;
    assert!(
        is_result(&configured),
        "configuring the new room {room} must succeed: {configured}"
    );
}

/// The Space `node` created by the admin's session, holding the room `id`
/// the session created and placed in it.
async fn space_holding_a_new_room(admin: &mut WsXmppClient, node: &str, id: &str) {
    create_space_node(admin, node).await;
    create_room(admin, id).await;
    let room = format!("{id}@muc.{DOMAIN}");
    let placed = iq_set_to(
        admin,
        &format!("place-{id}"),
        "spaces.localhost",
        space_publish(node, &room),
    )
    .await;
    assert!(
        is_result(&placed),
        "placing {room} in Space {node} must succeed: {placed}"
    );
}

/// A session creates a Space and a channel, and places the channel in the
/// Space, on a server whose permission actor startup built: the actor
/// writes the Space's owner relation and the channel's for the creator, an
/// account of the served domain, and the channel's parent grant on the
/// Space.
#[tokio::test]
async fn a_session_creates_a_space_and_a_channel_under_the_startup_issuance_policy() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-startup-issuance").await;
    let creator = format!("{USERNAME}@{DOMAIN}");

    space_holding_a_new_room(&mut admin, "made", "lab").await;

    assert_eq!(
        relationships(&pool, ("space", "made"), "owner", ("user", &creator)).await,
        1,
        "the Space's creator owns it"
    );
    assert_eq!(
        relationships(&pool, ("channel", "lab"), "owner", ("user", &creator)).await,
        1,
        "the channel's creator owns it"
    );
    assert_eq!(
        relationships(&pool, ("channel", "lab"), "parent", ("space", "made")).await,
        1,
        "the channel's parent is the Space"
    );
    assert_eq!(catalog_channels(&pool, "lab").await, 1);
    let items = space_items(&mut admin, "made-items", "made").await;
    assert_eq!(item_ids(&items), vec![format!("lab@muc.{DOMAIN}")]);
    let _ = admin.close().await;
    pool.close().await;
}

/// `spaces:delete` destroys the rooms the Space was the parent of, and only
/// those: a room a canonical item of the Space names, whose parent grant is
/// on another Space and which no link places here, loses the item and
/// stays.
#[tokio::test]
async fn spaces_delete_destroys_only_the_rooms_whose_parent_the_space_is() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-delete-parented").await;
    space_holding_a_new_room(&mut admin, "made", "lab").await;
    // `chat` is the seeded `general` Space's room. An item under its
    // canonical id is stored on `made` without a publish, so `made` is not
    // its parent, and no link places it anywhere.
    seed_space_item(&pool, "made", "chat@muc.localhost").await;
    sqlx::query("DELETE FROM channel_space_links WHERE channel_jid = 'chat@muc.localhost'")
        .execute(&pool)
        .await
        .expect("remove chat's link");

    let delete = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:waddle:admin:spaces:delete:0",
        )
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(
            DataForm::new(FormType::Submit)
                .add_field(Field::form_type("urn:waddle:admin:spaces:delete:0"))
                .add_field(Field::text_single("space_jid", "made@spaces.localhost"))
                .add_field(Field::text_single("confirm", "yes"))
                .to_element(),
        )
        .build();
    let response = iq_set_to(&mut admin, "delete-made", DOMAIN, delete).await;
    assert!(
        is_result(&response),
        "spaces:delete must succeed: {response}"
    );

    assert_eq!(
        catalog_channels(&pool, "lab").await,
        0,
        "the room the Space was the parent of is destroyed"
    );
    assert_eq!(
        catalog_channels(&pool, "chat").await,
        1,
        "a room only an item named is not destroyed"
    );
    assert_eq!(chat_parent_tuples(&pool, "general").await, 1);
    assert_eq!(
        stored_space_items(&pool, "made", "chat@muc.localhost").await,
        0
    );
    let general = space_items(&mut admin, "general-after-made", "general").await;
    assert!(
        item_ids(&general).contains(&"chat@muc.localhost".to_owned()),
        "chat stays bookmarked in general: {general}"
    );
    let _ = admin.close().await;
    pool.close().await;
}
