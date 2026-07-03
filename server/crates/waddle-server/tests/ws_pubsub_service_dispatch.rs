//! Characterization tests for the service-domain PubSub dispatch paths
//! (Spaces bookmarks on `spaces.<domain>`, community nodes on
//! `community.<domain>`, and extension routes on `extensions.<domain>`).
//!
//! These pin the wire-visible routing, authorization, and error
//! semantics of the WebSocket pubsub handlers so the handler modules
//! can be restructured without behavior drift:
//!
//! - Spaces bookmark publish/retract round trip (XEP-0503 over
//!   XEP-0060), including retract-after-retract `item-not-found`.
//! - Owner-only Spaces mutation gate (`forbidden` for plain members).
//! - Bookmark validation: unknown managed channel is `item-not-found`,
//!   a bookmark JID outside the MUC domain is `bad-request`.
//! - Unknown node names on every service domain surface
//!   `item-not-found` rather than auto-creating nodes.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const MEMBER_USERNAME: &str = "member";
const MEMBER_PASSWORD: &str = "member-pass-ws-pubsub";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const SPACES_JID: &str = "spaces.localhost";
const COMMUNITY_JID: &str = "community.localhost";
const EXTENSIONS_JID: &str = "extensions.localhost";
const MUC_DOMAIN: &str = "muc.localhost";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn member_client(server: &TestServer, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        resource,
    )
    .await
    .expect("member connect")
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#)
}

fn is_item_not_found(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) && frame.contains("<item-not-found")
}

fn is_forbidden(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) && frame.contains("<forbidden")
}

fn is_bad_request(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) && frame.contains("<bad-request")
}

fn bookmark_publish_body(node: &str, room_jid: &str, name: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="{room_jid}"><conference xmlns="urn:xmpp:bookmarks:1" name="{name}"/></item></publish></pubsub>"#
    )
}

fn retract_body(node: &str, item_id: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><retract node="{node}"><item id="{item_id}"/></retract></pubsub>"#
    )
}

fn items_body(node: &str) -> String {
    format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{node}"/></pubsub>"#)
}

/// Publish + retract of a Spaces bookmark round-trips through
/// `spaces.<domain>`, and a second retract of the same item id reports
/// `item-not-found` (XEP-0060 §7.2.3.2 shape).
#[tokio::test]
async fn spaces_bookmark_retract_round_trip() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-retract-1").await;
    let chat_room = format!("chat@{MUC_DOMAIN}");

    let publish = iq_set_to(
        &mut admin,
        "spaces-rt-pub",
        SPACES_JID,
        &bookmark_publish_body("general", &chat_room, "Chat"),
    )
    .await;
    assert!(is_result(&publish), "Spaces bookmark publish: {publish}");

    let items = iq_get_to(
        &mut admin,
        "spaces-rt-items-before",
        SPACES_JID,
        &items_body("general"),
    )
    .await;
    assert!(is_result(&items), "Spaces items read: {items}");
    assert!(
        items.contains(&chat_room),
        "published bookmark must be listed in Spaces items: {items}"
    );

    let retract = iq_set_to(
        &mut admin,
        "spaces-rt-retract",
        SPACES_JID,
        &retract_body("general", &chat_room),
    )
    .await;
    assert!(is_result(&retract), "Spaces bookmark retract: {retract}");

    let items_after = iq_get_to(
        &mut admin,
        "spaces-rt-items-after",
        SPACES_JID,
        &items_body("general"),
    )
    .await;
    assert!(
        is_result(&items_after),
        "Spaces items re-read: {items_after}"
    );
    assert!(
        !items_after.contains(&format!(r#"id='{chat_room}'"#)),
        "retracted bookmark must no longer be listed: {items_after}"
    );

    let retract_again = iq_set_to(
        &mut admin,
        "spaces-rt-retract-again",
        SPACES_JID,
        &retract_body("general", &chat_room),
    )
    .await;
    assert!(
        is_item_not_found(&retract_again),
        "retracting an already-retracted bookmark must be item-not-found: {retract_again}"
    );

    let _ = admin.close().await;
}

/// Spaces bookmark mutation is owner-gated: a plain member can neither
/// publish nor retract bookmarks on a Space node.
#[tokio::test]
async fn non_owner_cannot_publish_or_retract_spaces_bookmark() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(MEMBER_USERNAME, MEMBER_PASSWORD)]);
    let mut member = member_client(&server, "spaces-member-1").await;
    let chat_room = format!("chat@{MUC_DOMAIN}");

    let publish = iq_set_to(
        &mut member,
        "spaces-member-pub",
        SPACES_JID,
        &bookmark_publish_body("general", &chat_room, "Chat"),
    )
    .await;
    assert!(
        is_forbidden(&publish),
        "member Spaces bookmark publish must be forbidden: {publish}"
    );

    let retract = iq_set_to(
        &mut member,
        "spaces-member-retract",
        SPACES_JID,
        &retract_body("general", &chat_room),
    )
    .await;
    assert!(
        is_forbidden(&retract),
        "member Spaces bookmark retract must be forbidden: {retract}"
    );

    let _ = member.close().await;
}

/// A Spaces bookmark must reference an existing managed channel; a
/// bookmark for an unknown room id is `item-not-found`.
#[tokio::test]
async fn spaces_publish_rejects_bookmark_for_unknown_channel() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-unknown-chan-1").await;
    let ghost_room = format!("ghost-{}@{MUC_DOMAIN}", uuid::Uuid::new_v4());

    let publish = iq_set_to(
        &mut admin,
        "spaces-ghost-pub",
        SPACES_JID,
        &bookmark_publish_body("general", &ghost_room, "Ghost"),
    )
    .await;
    assert!(
        is_item_not_found(&publish),
        "bookmark for a channel missing from the catalog must be item-not-found: {publish}"
    );

    let _ = admin.close().await;
}

/// A Spaces bookmark JID outside the deployment MUC domain is rejected
/// as `bad-request`.
#[tokio::test]
async fn spaces_publish_rejects_foreign_domain_bookmark() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-foreign-1").await;

    let publish = iq_set_to(
        &mut admin,
        "spaces-foreign-pub",
        SPACES_JID,
        &bookmark_publish_body("general", "room@elsewhere.example", "Elsewhere"),
    )
    .await;
    assert!(
        is_bad_request(&publish),
        "bookmark JID outside the MUC domain must be bad-request: {publish}"
    );

    let _ = admin.close().await;
}

/// Publishing a valid bookmark to a Space node that does not exist is
/// `item-not-found` — the Spaces service never auto-creates nodes on
/// publish.
#[tokio::test]
async fn spaces_publish_to_unknown_space_node_is_item_not_found() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-unknown-node-1").await;
    let chat_room = format!("chat@{MUC_DOMAIN}");

    let publish = iq_set_to(
        &mut admin,
        "spaces-unknown-node-pub",
        SPACES_JID,
        &bookmark_publish_body("no-such-space", &chat_room, "Chat"),
    )
    .await;
    assert!(
        is_item_not_found(&publish),
        "publish to an unknown Space node must be item-not-found: {publish}"
    );

    let items = iq_get_to(
        &mut admin,
        "spaces-unknown-node-items",
        SPACES_JID,
        &items_body("no-such-space"),
    )
    .await;
    assert!(
        is_item_not_found(&items),
        "items read of an unknown Space node must be item-not-found: {items}"
    );

    let _ = admin.close().await;
}

/// Community-domain pubsub only serves the server-managed feed,
/// stories, and calendar nodes; any other node name is `item-not-found`
/// on publish, items, and retract alike.
#[tokio::test]
async fn community_unknown_node_paths_are_item_not_found() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "community-unknown-1").await;
    let bogus_node = "urn:bogus:community:node";

    let publish = iq_set_to(
        &mut admin,
        "community-bogus-pub",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{bogus_node}"><item id="x"><payload xmlns="urn:bogus:payload"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        is_item_not_found(&publish),
        "community publish to unknown node must be item-not-found: {publish}"
    );

    let items = iq_get_to(
        &mut admin,
        "community-bogus-items",
        COMMUNITY_JID,
        &items_body(bogus_node),
    )
    .await;
    assert!(
        is_item_not_found(&items),
        "community items for unknown node must be item-not-found: {items}"
    );

    let retract = iq_set_to(
        &mut admin,
        "community-bogus-retract",
        COMMUNITY_JID,
        &retract_body(bogus_node, "x"),
    )
    .await;
    assert!(
        is_item_not_found(&retract),
        "community retract on unknown node must be item-not-found: {retract}"
    );

    let _ = admin.close().await;
}

/// Extension-route pubsub reads resolve the node against the registered
/// extension route descriptors; a node matching no route is
/// `item-not-found`.
#[tokio::test]
async fn extension_route_items_unknown_node_is_item_not_found() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "ext-route-unknown-1").await;

    let items = iq_get_to(
        &mut admin,
        "ext-route-bogus-items",
        EXTENSIONS_JID,
        &items_body("urn:bogus:extension:node"),
    )
    .await;
    assert!(
        is_item_not_found(&items),
        "extension-route items for unknown node must be item-not-found: {items}"
    );

    let _ = admin.close().await;
}
