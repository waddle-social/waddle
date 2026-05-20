//! XEP-0501 Pubsub Stories integration tests over WebSocket.
//!
//! Verifies the server bootstrap: the spaces service hosts the
//! global `urn:xmpp:stories:0` node, advertises the namespace on
//! disco#info, and accepts publish + items-query for `<story/>`
//! payloads built per XEP-0501.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const COMMUNITY_JID: &str = "community.localhost";
const STORIES_NODE: &str = "urn:xmpp:stories:0";
const NS_STORIES: &str = "urn:xmpp:stories:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0501-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn community_disco_info_advertises_stories_namespace() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    client
        .send(&format!(
            r#"<iq type="get" id="disco-stories" to="{COMMUNITY_JID}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send disco#info");
    let frame = client
        .recv_matching(|frame| frame.contains("disco-stories") && frame.contains("<feature"))
        .await
        .expect("disco#info response");
    assert!(
        frame.contains(&format!("var='{NS_STORIES}'"))
            || frame.contains(&format!("var='{NS_STORIES}'")),
        "spaces disco#info missing stories namespace: {frame}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn stories_publish_and_items_round_trip() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    // Publish a story to the bootstrapped community stories node.
    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let expires = "2030-01-01T12:00:00Z";
    client
        .send(&format!(
            r#"<iq type="set" id="story-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="{story_id}">
                    <story xmlns="{NS_STORIES}" expires="{expires}">
                      <body>Look at this!</body>
                      <media-url>https://example.com/photo.jpg</media-url>
                      <author>admin@localhost</author>
                    </story>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("story-publish"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type='result'"),
        "publish must succeed against the bootstrapped stories node: {publish_result}"
    );

    // Items query — the published story should round-trip with the
    // typed `<story/>` payload intact (id, expires, body, media-url).
    client
        .send(&format!(
            r#"<iq type="get" id="story-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{STORIES_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = client
        .recv_matching(|frame| frame.contains("story-items") && frame.contains("<story"))
        .await
        .expect("items response");
    assert!(
        items_response.contains(&story_id),
        "items query missing published id: {items_response}"
    );
    assert!(
        items_response.contains("Look at this!"),
        "items query lost body: {items_response}"
    );
    assert!(
        items_response.contains("https://example.com/photo.jpg"),
        "items query lost media-url: {items_response}"
    );
    assert!(
        items_response.contains(expires),
        "items query lost expires attr: {items_response}"
    );

    let _ = client.close().await;
}
