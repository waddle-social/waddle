//! XEP-0503 Spaces wire-conformance integration tests over WebSocket,
//! including the identity profile for XEP-0402 room item ids and the
//! avatar and banner items of version 0.3.0 ("Spaces images").

use waddle_ws_test_support as ws_common;

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, ToElement};
use waddle_xmpp::xep::xep0503::{ITEM_SPACE_AVATAR_METADATA, ITEM_SPACE_BANNER_METADATA};
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

const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

/// A `disco#items` query, of `node` if given.
fn disco_items(node: Option<&str>) -> Element {
    let mut query = Element::builder("query", NS_DISCO_ITEMS);
    if let Some(node) = node {
        query = query.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    query.build()
}

/// The `(jid, name)` of each item of a `disco#items` answer.
fn disco_item_pairs(frame: &str) -> Vec<(String, Option<String>)> {
    let response: Element = frame.parse().expect("disco#items response XML");
    response
        .get_child("query", NS_DISCO_ITEMS)
        .map(|query| {
            query
                .children()
                .filter(|item| item.name() == "item")
                .map(|item| {
                    (
                        item.attr("jid").unwrap_or_default().to_owned(),
                        item.attr("name").map(str::to_owned),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// XEP-0503, "Advertising space membership", and its MUC fallback: the
/// `disco#items` of a Space's node lists the rooms the Space holds, as
/// XEP-0060 §5.5 lists published items (the service's address and the item
/// ID, the room's JID, as its name); a node the session may not read is not
/// found. A session that never asks the Spaces service finds the same room
/// in the MUC service's `disco#items` (XEP-0045 §6.3) and enters it by its
/// plain JID (§7.2).
#[tokio::test]
async fn a_space_node_lists_its_rooms_and_a_client_without_spaces_enters_one() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-space-children").await;
    space_holding_a_new_room(&mut admin, "made", "lab").await;
    let room = format!("lab@muc.{DOMAIN}");

    let children = iq_get_to(
        &mut admin,
        "made-children",
        "spaces.localhost",
        disco_items(Some("made")),
    )
    .await;
    assert!(is_result(&children), "{children}");
    assert_eq!(
        disco_item_pairs(&children),
        vec![("spaces.localhost".to_owned(), Some(room.clone()))]
    );
    let missing = iq_get_to(
        &mut admin,
        "missing-children",
        "spaces.localhost",
        disco_items(Some("nowhere")),
    )
    .await;
    assert!(
        missing.contains("item-not-found"),
        "a node that does not exist must not be listed: {missing}"
    );

    let mut plain = admin_client(&server, "xep0503-no-spaces").await;
    let rooms = iq_get_to(
        &mut plain,
        "muc-rooms",
        &format!("muc.{DOMAIN}"),
        disco_items(None),
    )
    .await;
    assert!(
        disco_item_pairs(&rooms).iter().any(|(jid, _)| jid == &room),
        "the Space's room must be listed by the MUC service: {rooms}"
    );
    // Waddle locks a nickname to its account, so the second session enters
    // under the account's nickname (XEP-0045 §7.2, a second session of one
    // occupant).
    let occupant = format!("{room}/{USERNAME}");
    let join = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            occupant.clone(),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build();
    plain.send(&String::from(&join)).await.expect("send join");
    let entered = plain
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence"
                    && element.attr("from") == Some(occupant.as_str())
                    && (element.attr("type") == Some("error")
                        || element.children().flat_map(Element::children).any(|child| {
                            child.name() == "status" && child.attr("code") == Some("110")
                        }))
            })
        })
        .await
        .expect("the room answers the join");
    let entered: Element = entered.parse().expect("presence XML");
    assert_eq!(
        entered.attr("type"),
        None,
        "the join was refused: {entered:?}"
    );
    let _ = plain.close().await;
    let _ = admin.close().await;
}

const NS_RSM: &str = "http://jabber.org/protocol/rsm";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

/// A `disco#items` query of `node` asking for at most `max` items, after
/// `after` when given (XEP-0059 §2.2).
fn paged_disco_items(node: &str, max: usize, after: Option<&str>) -> Element {
    let mut set = Element::builder("set", NS_RSM).append(
        Element::builder("max", NS_RSM)
            .append(max.to_string())
            .build(),
    );
    if let Some(after) = after {
        set = set.append(Element::builder("after", NS_RSM).append(after).build());
    }
    Element::builder("query", NS_DISCO_ITEMS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .append(set.build())
        .build()
}

/// What a `<set/>` tells: its count, its first item's index and its first
/// and last items.
type ListingSet = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The `<set/>` of a `disco#items` answer.
fn disco_items_set(frame: &str) -> Option<ListingSet> {
    let response: Element = frame.parse().expect("disco#items response XML");
    let set = response
        .get_child("query", NS_DISCO_ITEMS)?
        .get_child("set", NS_RSM)?;
    let text = |name: &str| set.get_child(name, NS_RSM).map(Element::text);
    let index = set
        .get_child("first", NS_RSM)
        .and_then(|first| first.attr("index").map(str::to_owned));
    Some((text("count"), index, text("first"), text("last")))
}

/// The `<error/>` of an answer, and the address it came from.
fn error_and_sender(frame: &str) -> (Option<String>, Option<Element>) {
    let response: Element = frame.parse().expect("response XML");
    (
        response.attr("from").map(str::to_owned),
        response
            .children()
            .find(|child| child.name() == "error")
            .cloned(),
    )
}

/// A Space owner's configuration of `node` as a private Space: only its
/// members may read it (`whitelist`).
fn private_space(node: &str) -> Element {
    let field = Element::builder("field", NS_XDATA)
        .attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            "pubsub#access_model",
        )
        .append(
            Element::builder("value", NS_XDATA)
                .append("whitelist")
                .build(),
        )
        .build();
    Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(
            Element::builder("configure", NS_PUBSUB_OWNER)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .append(
                    Element::builder("x", NS_XDATA)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                        .append(field)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// The `disco#items` of a Space node a session may not read is answered
/// exactly as that of a node that does not exist: the same error, from the
/// same address, so the answer tells nothing of the Space. Its owner is
/// still told its rooms.
#[tokio::test]
async fn a_space_node_a_session_may_not_read_is_listed_as_one_that_does_not_exist() {
    let _serial = TEST_SERIAL.lock().await;
    let carol_password = format!("carol-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("carol", &carol_password)]);
    let mut admin = admin_client(&server, "xep0503-private-children").await;
    space_holding_a_new_room(&mut admin, "private", "vault").await;
    let configured = iq_set_to(
        &mut admin,
        "private-whitelist",
        "spaces.localhost",
        private_space("private"),
    )
    .await;
    assert!(is_result(&configured), "{configured}");
    let owned = iq_get_to(
        &mut admin,
        "private-owner-children",
        "spaces.localhost",
        disco_items(Some("private")),
    )
    .await;
    assert_eq!(
        disco_item_pairs(&owned),
        vec![(
            "spaces.localhost".to_owned(),
            Some(format!("vault@muc.{DOMAIN}"))
        )]
    );

    let mut carol = ws_common::WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "carol",
        &carol_password,
        "xep0503-private-carol",
    )
    .await
    .expect("carol connect");
    let unreadable = iq_get_to(
        &mut carol,
        "private-children",
        "spaces.localhost",
        disco_items(Some("private")),
    )
    .await;
    let missing = iq_get_to(
        &mut carol,
        "missing-children",
        "spaces.localhost",
        disco_items(Some("nowhere")),
    )
    .await;
    assert!(
        unreadable.contains("item-not-found"),
        "a Space the session may not read must not be listed: {unreadable}"
    );
    assert_eq!(error_and_sender(&unreadable), error_and_sender(&missing));
    let _ = carol.close().await;
    let _ = admin.close().await;
}

/// A session pages a Space node's rooms with XEP-0059, by item ID: the
/// count is of the rooms listed to it, and a page continues from its last
/// item.
#[tokio::test]
async fn a_session_pages_a_space_nodes_rooms() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-paged-children").await;
    space_holding_a_new_room(&mut admin, "paged", "one").await;
    create_room(&mut admin, "two").await;
    let two = format!("two@muc.{DOMAIN}");
    let placed = iq_set_to(
        &mut admin,
        "place-two",
        "spaces.localhost",
        space_publish("paged", &two),
    )
    .await;
    assert!(is_result(&placed), "{placed}");
    let one = format!("one@muc.{DOMAIN}");

    let first = iq_get_to(
        &mut admin,
        "paged-first",
        "spaces.localhost",
        paged_disco_items("paged", 1, None),
    )
    .await;
    assert!(is_result(&first), "{first}");
    assert_eq!(
        disco_item_pairs(&first),
        vec![("spaces.localhost".to_owned(), Some(one.clone()))]
    );
    assert_eq!(
        disco_items_set(&first),
        Some((
            Some("2".to_owned()),
            Some("0".to_owned()),
            Some(one.clone()),
            Some(one.clone())
        ))
    );

    let second = iq_get_to(
        &mut admin,
        "paged-second",
        "spaces.localhost",
        paged_disco_items("paged", 1, Some(&one)),
    )
    .await;
    assert_eq!(
        disco_item_pairs(&second),
        vec![("spaces.localhost".to_owned(), Some(two.clone()))]
    );
    assert_eq!(
        disco_items_set(&second),
        Some((
            Some("2".to_owned()),
            Some("1".to_owned()),
            Some(two.clone()),
            Some(two)
        ))
    );

    // Without a request for paging, a listing that fits in one answer
    // carries no `<set/>`.
    let whole = iq_get_to(
        &mut admin,
        "paged-whole",
        "spaces.localhost",
        disco_items(Some("paged")),
    )
    .await;
    assert_eq!(disco_item_pairs(&whole).len(), 2, "{whole}");
    assert_eq!(disco_items_set(&whole), None, "{whole}");
    let _ = admin.close().await;
}

// ---------------------------------------------------------------------------
// XEP-0503 0.3.0, "Spaces images": a Space's avatar and banner are items of
// its node holding XEP-0084 metadata. They are stored and read as items,
// name no room, and nothing fetches the URLs they carry (ADR-0020 D4).
// ---------------------------------------------------------------------------

const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

/// A XEP-0084 `<metadata/>` naming one image at `url`.
fn image_metadata(url: &str) -> Element {
    Element::builder("metadata", NS_AVATAR_METADATA)
        .append(
            Element::builder("info", NS_AVATAR_METADATA)
                .attr(minidom::rxml::xml_ncname!("bytes").to_owned(), "420")
                .attr(minidom::rxml::xml_ncname!("height").to_owned(), "64")
                .attr(minidom::rxml::xml_ncname!("width").to_owned(), "64")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "image/png")
                .attr(minidom::rxml::xml_ncname!("url").to_owned(), url)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "image-hash")
                .build(),
        )
        .build()
}

/// A `<publish/>` of the item `item_id` holding `payloads` to `node`.
fn item_publish(node: &str, item_id: &str, payloads: Vec<Element>) -> Element {
    let mut item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id);
    for payload in payloads {
        item = item.append(payload);
    }
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("publish", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .append(item.build())
                .build(),
        )
        .build()
}

/// The `<item/>` `item_id` of an items answer.
fn answered_item(frame: &str, item_id: &str) -> Option<Element> {
    let response: Element = frame.parse().expect("items response XML");
    response
        .get_child("pubsub", NS_PUBSUB)?
        .get_child("items", NS_PUBSUB)?
        .children()
        .find(|item| item.attr("id") == Some(item_id))
        .cloned()
}

/// The pubsub-specific condition of an error answer.
fn pubsub_condition(frame: &str) -> Option<String> {
    let response: Element = frame.parse().ok()?;
    response
        .get_child("error", "jabber:client")?
        .children()
        .find(|child| child.ns() == NS_PUBSUB_ERRORS)
        .map(|condition| condition.name().to_owned())
}

fn is_item_not_found(frame: &str) -> bool {
    frame.parse::<Element>().is_ok_and(|response| {
        response
            .get_child("error", "jabber:client")
            .is_some_and(|error| {
                error
                    .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
                    .is_some()
            })
    })
}

/// A listener on a loopback port that no one is expected to connect to,
/// and the `http:` URL of an image on it.
async fn image_host(path: &str) -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let port = listener.local_addr().expect("listener address").port();
    (listener, format!("http://127.0.0.1:{port}{path}"))
}

/// Whether anything connects to `listener` within a quiet period.
async fn connected_within_quiet_period(listener: &tokio::net::TcpListener) -> bool {
    tokio::time::timeout(Duration::from_millis(750), listener.accept())
        .await
        .is_ok()
}

/// The Space's owner publishes its avatar and banner beside its rooms: a
/// read of the Space's items returns them as published with the rooms, the
/// rooms keep their Space, the `disco#items` of the Space lists rooms only,
/// another Space keeps its own image under the same item id, and nothing
/// requests the image URLs.
#[tokio::test]
async fn an_owner_publishes_a_spaces_images_beside_its_rooms_and_nothing_fetches_them() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-space-images").await;
    let (avatar_host, avatar_url) = image_host("/avatar.png").await;
    let (banner_host, banner_url) = image_host("/banner.png").await;

    for (item_id, url) in [
        (ITEM_SPACE_AVATAR_METADATA, &avatar_url),
        (ITEM_SPACE_BANNER_METADATA, &banner_url),
    ] {
        let published = iq_set_to(
            &mut admin,
            &format!("publish-{item_id}"),
            "spaces.localhost",
            item_publish("general", item_id, vec![image_metadata(url)]),
        )
        .await;
        assert!(is_result(&published), "publishing {item_id}: {published}");
        assert!(
            published.contains(item_id),
            "the result names the item: {published}"
        );
    }

    let items = space_items(&mut admin, "general-items-with-images", "general").await;
    let ids = item_ids(&items);
    for expected in [
        "chat@muc.localhost",
        "announcements@muc.localhost",
        ITEM_SPACE_AVATAR_METADATA,
        ITEM_SPACE_BANNER_METADATA,
    ] {
        assert!(
            ids.iter().any(|id| id == expected),
            "{expected} is listed: {items}"
        );
    }
    let avatar = answered_item(&items, ITEM_SPACE_AVATAR_METADATA).expect("the avatar item");
    let info = avatar
        .get_child("metadata", NS_AVATAR_METADATA)
        .and_then(|metadata| metadata.get_child("info", NS_AVATAR_METADATA))
        .expect("the avatar's metadata");
    assert_eq!(info.attr("url"), Some(avatar_url.as_str()));
    assert_eq!(info.attr("bytes"), Some("420"));
    assert!(answered_item(&items, "chat@muc.localhost")
        .and_then(|chat| chat
            .get_child("conference", "urn:xmpp:bookmarks:1")
            .cloned())
        .is_some());

    // The rooms keep their Space, and the Space's listing of its rooms
    // names no image.
    let info = iq_get_to(
        &mut admin,
        "chat-info-with-images",
        "chat@muc.localhost",
        Element::builder("query", "http://jabber.org/protocol/disco#info").build(),
    )
    .await;
    assert!(
        info.contains("xmpp:spaces.localhost?;node=general"),
        "chat keeps its general parent: {info}"
    );
    let children = iq_get_to(
        &mut admin,
        "general-children-with-images",
        "spaces.localhost",
        disco_items(Some("general")),
    )
    .await;
    let names: Vec<_> = disco_item_pairs(&children)
        .into_iter()
        .filter_map(|(_, name)| name)
        .collect();
    assert!(
        names.contains(&"chat@muc.localhost".to_owned()),
        "{children}"
    );
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("urn:xmpp:spaces:")),
        "an image is no room of the Space: {children}"
    );

    // Every Space holds its avatar under the same item id; publishing one
    // moves nothing out of another Space.
    create_space_node(&mut admin, "second").await;
    let second = iq_set_to(
        &mut admin,
        "publish-second-avatar",
        "spaces.localhost",
        item_publish(
            "second",
            ITEM_SPACE_AVATAR_METADATA,
            vec![image_metadata(&avatar_url)],
        ),
    )
    .await;
    assert!(is_result(&second), "{second}");
    let general = space_items(&mut admin, "general-items-after-second", "general").await;
    assert!(
        item_ids(&general).contains(&ITEM_SPACE_AVATAR_METADATA.to_owned()),
        "general keeps its avatar: {general}"
    );
    let second = space_items(&mut admin, "second-items", "second").await;
    assert_eq!(
        item_ids(&second),
        vec![ITEM_SPACE_AVATAR_METADATA.to_owned()]
    );

    assert!(
        !connected_within_quiet_period(&avatar_host).await,
        "nothing requests the avatar"
    );
    assert!(
        !connected_within_quiet_period(&banner_host).await,
        "nothing requests the banner"
    );
    let _ = admin.close().await;
}

/// An image item whose payload is not a Space image's XEP-0084 metadata is
/// refused with `<bad-request/>` and stores nothing; an item under any
/// other id that is no room's bookmark is refused as before.
#[tokio::test]
async fn a_malformed_space_image_is_refused_and_any_other_id_stays_a_room() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "xep0503-space-images-refused").await;
    let url = "https://images.example.com/avatar.png";
    let info = |attributes: &[(&'static str, &'static str)]| {
        let mut info = Element::builder("info", NS_AVATAR_METADATA);
        for (name, value) in attributes {
            info = info.attr(
                minidom::rxml::NcName::try_from(*name).expect("an attribute name"),
                *value,
            );
        }
        Element::builder("metadata", NS_AVATAR_METADATA)
            .append(info.build())
            .build()
    };
    let refused: Vec<(&str, Vec<Element>, &str)> = vec![
        ("no-payload", Vec::new(), "payload-required"),
        (
            "empty-metadata",
            vec![Element::builder("metadata", NS_AVATAR_METADATA).build()],
            "invalid-payload",
        ),
        (
            "two-metadata",
            vec![image_metadata(url), image_metadata(url)],
            "invalid-payload",
        ),
        (
            "conference",
            vec![Element::builder("conference", "urn:xmpp:bookmarks:1").build()],
            "invalid-payload",
        ),
        (
            "no-bytes",
            vec![info(&[("id", "a"), ("type", "image/png")])],
            "invalid-payload",
        ),
        (
            "not-an-image",
            vec![info(&[("bytes", "1"), ("id", "a"), ("type", "text/html")])],
            "invalid-payload",
        ),
        (
            "ftp-url",
            vec![info(&[
                ("bytes", "1"),
                ("id", "a"),
                ("type", "image/png"),
                ("url", "ftp://images.example.com/a.png"),
            ])],
            "invalid-payload",
        ),
        (
            "pointer",
            vec![Element::builder("metadata", NS_AVATAR_METADATA)
                .append(
                    image_metadata(url)
                        .get_child("info", NS_AVATAR_METADATA)
                        .cloned()
                        .expect("an info"),
                )
                .append(
                    Element::builder("pointer", NS_AVATAR_METADATA)
                        .append(Element::builder("x", "urn:example:service").build())
                        .build(),
                )
                .build()],
            "invalid-payload",
        ),
    ];
    for (case, payloads, condition) in refused {
        let response = iq_set_to(
            &mut admin,
            &format!("refused-image-{case}"),
            "spaces.localhost",
            item_publish("general", ITEM_SPACE_BANNER_METADATA, payloads),
        )
        .await;
        assert!(
            is_bad_request_error(&response),
            "{case} is refused with <bad-request/>: {response}"
        );
        assert_eq!(
            pubsub_condition(&response).as_deref(),
            Some(condition),
            "{case}: {response}"
        );
    }
    let items = space_items(&mut admin, "general-after-refused-images", "general").await;
    assert!(
        !item_ids(&items).contains(&ITEM_SPACE_BANNER_METADATA.to_owned()),
        "no refused image is stored: {items}"
    );

    // An id that names neither an image nor a room is refused as a room
    // item that is no bookmark is, whatever it holds.
    let other = iq_set_to(
        &mut admin,
        "refused-other-id",
        "spaces.localhost",
        item_publish(
            "general",
            "urn:example:spaces:logo",
            vec![image_metadata(url)],
        ),
    )
    .await;
    assert!(is_bad_request_error(&other), "{other}");
    assert_eq!(pubsub_condition(&other), None, "{other}");
    let items = space_items(&mut admin, "general-after-other-id", "general").await;
    assert!(
        !item_ids(&items).contains(&"urn:example:spaces:logo".to_owned()),
        "{items}"
    );
    let _ = admin.close().await;
}

/// Retracting a Space's image removes its item and nothing else: the rooms
/// keep their items, parent relationships and links; retracting it again
/// finds no item.
#[tokio::test]
async fn retracting_a_spaces_image_leaves_its_rooms_in_place() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-space-image-retract").await;
    let links = || async {
        sqlx::query("SELECT COUNT(*) FROM channel_space_links")
            .fetch_one(&pool)
            .await
            .expect("count links")
            .get::<i64, _>(0)
    };
    let (host, url) = image_host("/avatar.png").await;

    let published = iq_set_to(
        &mut admin,
        "retract-image-publish",
        "spaces.localhost",
        item_publish(
            "general",
            ITEM_SPACE_AVATAR_METADATA,
            vec![image_metadata(&url)],
        ),
    )
    .await;
    assert!(is_result(&published), "{published}");
    let links_before = links().await;
    assert_eq!(chat_parent_tuples(&pool, "general").await, 1);

    let retracted = iq_set_to(
        &mut admin,
        "retract-image",
        "spaces.localhost",
        space_retract("general", ITEM_SPACE_AVATAR_METADATA),
    )
    .await;
    assert!(is_result(&retracted), "{retracted}");
    assert_eq!(
        stored_space_items(&pool, "general", ITEM_SPACE_AVATAR_METADATA).await,
        0
    );
    assert_eq!(
        stored_space_items(&pool, "general", "chat@muc.localhost").await,
        1
    );
    assert_eq!(chat_parent_tuples(&pool, "general").await, 1);
    assert_eq!(links().await, links_before);
    let items = space_items(&mut admin, "general-after-image-retract", "general").await;
    let ids = item_ids(&items);
    assert!(ids.contains(&"chat@muc.localhost".to_owned()), "{items}");
    assert!(
        !ids.contains(&ITEM_SPACE_AVATAR_METADATA.to_owned()),
        "{items}"
    );

    let again = iq_set_to(
        &mut admin,
        "retract-image-again",
        "spaces.localhost",
        space_retract("general", ITEM_SPACE_AVATAR_METADATA),
    )
    .await;
    assert!(is_item_not_found(&again), "{again}");
    assert!(!connected_within_quiet_period(&host).await);
    let _ = admin.close().await;
    pool.close().await;
}

/// `spaces:delete` of a Space holding an image takes the image with the
/// Space and destroys only the rooms the Space was the parent of.
#[tokio::test]
async fn spaces_delete_takes_a_spaces_image_with_it() {
    let _serial = TEST_SERIAL.lock().await;
    let directory = tempfile::tempdir().expect("database directory");
    let (server, pool) = persistent_server(&directory).await;
    let mut admin = admin_client(&server, "xep0503-space-image-delete").await;
    space_holding_a_new_room(&mut admin, "pictured", "gallery").await;
    let published = iq_set_to(
        &mut admin,
        "pictured-banner",
        "spaces.localhost",
        item_publish(
            "pictured",
            ITEM_SPACE_BANNER_METADATA,
            vec![image_metadata("https://images.example.com/banner.png")],
        ),
    )
    .await;
    assert!(is_result(&published), "{published}");

    let delete = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:waddle:admin:spaces:delete:0",
        )
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(
            DataForm::new(FormType::Submit)
                .add_field(Field::form_type("urn:waddle:admin:spaces:delete:0"))
                .add_field(Field::text_single("space_jid", "pictured@spaces.localhost"))
                .add_field(Field::text_single("confirm", "yes"))
                .to_element(),
        )
        .build();
    let response = iq_set_to(&mut admin, "delete-pictured", DOMAIN, delete).await;
    assert!(is_result(&response), "spaces:delete succeeds: {response}");
    assert_eq!(catalog_channels(&pool, "gallery").await, 0);
    assert_eq!(
        stored_space_items(&pool, "pictured", ITEM_SPACE_BANNER_METADATA).await,
        0
    );
    assert_eq!(catalog_channels(&pool, "chat").await, 1);
    let _ = admin.close().await;
    pool.close().await;
}
