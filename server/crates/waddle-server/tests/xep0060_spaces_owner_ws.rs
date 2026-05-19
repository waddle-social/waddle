//! XEP-0060 §8 owner-namespace operations against the Spaces service.
//!
//! Verifies that server owners (configured via `WADDLE_SERVER_OWNER_LOCALPARTS`)
//! can `<configure/>` get/set, `<purge/>`, and `<affiliations/>` set against
//! Spaces nodes after the seeding wired in for issue #241.

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const SPACES_JID: &str = "spaces.localhost";
const MUC_DOMAIN: &str = "muc.localhost";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
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
    frame.contains(r#"type='result'"#) || frame.contains(r#"type='result'"#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type='error'"#)
}

#[tokio::test]
async fn server_owner_can_configure_get_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-cfg-get-1").await;

    let resp = iq_get_to(
        &mut admin,
        "spaces-cfg-get",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="general"/></pubsub>"#),
    )
    .await;

    assert!(
        is_result(&resp),
        "expected configure-get result, got: {resp}"
    );
    assert!(
        resp.contains(r#"xmlns='jabber:x:data'"#) || resp.contains(r#"xmlns='jabber:x:data'"#),
        "expected data form in configure-get response, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn server_owner_can_configure_set_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-cfg-set-1").await;

    let resp = iq_set_to(
        &mut admin,
        "spaces-cfg-set",
        SPACES_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="general"><x xmlns="jabber:x:data" type="submit"><field var="pubsub#max_items"><value>200</value></field></x></configure></pubsub>"#
        ),
    )
    .await;

    assert!(
        is_result(&resp),
        "expected configure-set result, got: {resp}"
    );
    let cfg = iq_get_to(
        &mut admin,
        "spaces-cfg-set-readback",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="general"/></pubsub>"#),
    )
    .await;
    assert!(
        cfg.contains("pubsub#access_model") && cfg.contains(">open<"),
        "partial configure-set must preserve the public Spaces access model: {cfg}"
    );
    assert!(
        cfg.contains("pubsub#max_items") && cfg.contains(">200<"),
        "partial configure-set should apply the submitted max_items field: {cfg}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn server_owner_can_purge_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-purge-1").await;

    let resp = iq_set_to(
        &mut admin,
        "spaces-purge",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><purge node="general"/></pubsub>"#),
    )
    .await;

    assert!(is_result(&resp), "expected purge result, got: {resp}");
    let _ = admin.close().await;
}

#[tokio::test]
async fn server_owner_can_set_affiliations_on_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-aff-1").await;

    let resp = iq_set_to(
        &mut admin,
        "spaces-aff",
        SPACES_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><affiliations node="general"><affiliation jid="someone@localhost" affiliation="member"/></affiliations></pubsub>"#
        ),
    )
    .await;

    assert!(
        is_result(&resp),
        "expected affiliations-set result, got: {resp}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn newly_created_space_is_administrable_by_creator() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-create-cfg-1").await;
    let node = format!("issue241-{}", uuid::Uuid::new_v4());

    // Create the space.
    let create = iq_set_to(
        &mut admin,
        "spaces-create",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(is_result(&create), "expected create result, got: {create}");

    // Configure-get against the new node should work for the creator.
    let cfg = iq_get_to(
        &mut admin,
        "spaces-create-cfg",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(
        is_result(&cfg),
        "expected configure-get result on freshly-created space, got: {cfg}"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn server_owner_can_delete_a_spaces_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-delete-1").await;
    let node = format!("delete-{}", uuid::Uuid::new_v4());

    // Create the node, then delete it via the owner namespace.
    let create = iq_set_to(
        &mut admin,
        "spaces-delete-create",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(is_result(&create), "expected create result, got: {create}");

    let resp = iq_set_to(
        &mut admin,
        "spaces-delete",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><delete node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(is_result(&resp), "expected delete result, got: {resp}");
    let _ = admin.close().await;
}

#[tokio::test]
async fn non_owner_cannot_configure_general_space() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "spaces-non-owner-1",
    )
    .await
    .expect("alice connect");

    let resp = iq_get_to(
        &mut alice,
        "spaces-cfg-non-owner",
        SPACES_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="general"/></pubsub>"#),
    )
    .await;

    assert!(
        is_error(&resp),
        "expected forbidden error for non-owner, got: {resp}"
    );
    assert!(
        resp.contains("forbidden"),
        "expected <forbidden/> condition, got: {resp}"
    );
    let _ = alice.close().await;
}

// ============================================================================
// XEP-0060 §7.1 Spaces publish-time fan-out (#238)
// ============================================================================

async fn wait_for_event_message(
    client: &mut WsXmppClient,
    node: &str,
    dur: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node='{node}'"#))
                        || frame.contains(&format!(r#"node='{node}'"#)))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn spaces_publish_fans_event_to_subscriber() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "spaces-fanout-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Subscribe admin to the seeded `general` Spaces node (XEP-0503).
    let sub = iq_set_to(
        &mut admin,
        "spaces-fanout-sub",
        SPACES_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="general" jid="{admin_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(is_result(&sub), "subscribe to spaces.general: {sub}");

    // Publish a bookmark item pointing at the seeded `chat@muc.localhost`
    // managed room — this is the only way the Spaces publish path admits
    // a bookmark, since `handle_spaces_publish` enforces that the bookmark
    // JID resolves to an existing managed channel (XEP-0503 §4).
    let chat_room = format!("chat@{MUC_DOMAIN}");
    let pub_resp = iq_set_to(
        &mut admin,
        "spaces-fanout-pub",
        SPACES_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="general"><item id="{chat_room}"><conference xmlns="urn:xmpp:bookmarks:1" name="Chat"/></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        is_result(&pub_resp),
        "Spaces publish via handle_spaces_publish: {pub_resp}"
    );

    let event = wait_for_event_message(&mut admin, "general", Duration::from_secs(2))
        .await
        .expect("Spaces subscriber must receive §7.1 event (acceptance criterion of #238)");

    assert!(
        event.contains(r#"type='headline'"#) || event.contains(r#"type='headline'"#),
        "Spaces event must be headline (XEP-0060 §12.18): {event}"
    );
    assert!(
        event.contains(&format!(r#"from='{SPACES_JID}'"#))
            || event.contains(&format!(r#"from='{SPACES_JID}'"#)),
        "from must be the spaces service JID: {event}"
    );
    assert!(
        event.contains(&format!(r#"id='{chat_room}'"#))
            || event.contains(&format!(r#"id='{chat_room}'"#)),
        "event must carry the published bookmark item id: {event}"
    );

    let _ = admin.close().await;
}
