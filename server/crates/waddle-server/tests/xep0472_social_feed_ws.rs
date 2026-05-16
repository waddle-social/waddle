//! XEP-0472 Pubsub Social Feed integration tests over WebSocket.
//!
//! Verifies the server-side bootstrap: the spaces service hosts the
//! global `urn:xmpp:pubsub-social-feed:0` node, advertises the
//! namespace on disco#info, and accepts publish + items-query for
//! `<entry/>` payloads built per XEP-0472 §3.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const COMMUNITY_JID: &str = "community.localhost";
const FEED_NODE: &str = "urn:xmpp:pubsub-social-feed:0";
const NS_SOCIAL_FEED: &str = "urn:xmpp:pubsub-social-feed:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0472-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn community_disco_info_advertises_social_feed_namespace() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    client
        .send(&format!(
            r#"<iq type="get" id="disco-feed" to="{COMMUNITY_JID}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send disco#info");
    let frame = client
        .recv_matching(|frame| frame.contains("disco-feed") && frame.contains("<feature"))
        .await
        .expect("disco#info response");
    assert!(
        frame.contains(&format!("var=\"{NS_SOCIAL_FEED}\""))
            || frame.contains(&format!("var='{NS_SOCIAL_FEED}'")),
        "spaces disco#info missing social-feed namespace: {frame}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn social_feed_publish_and_items_round_trip() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    // Publish a feed entry to the bootstrapped community feed node.
    // The bootstrap creates the node + sets `spaces_public` config
    // (Open access, Publisher publish-model) at server startup, and
    // `seed_spaces_admin_affiliations` gives admin Owner affiliation
    // across all spaces nodes so admin can publish.
    let post_id = format!("post-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<iq type="set" id="feed-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="{post_id}">
                    <entry xmlns="{NS_SOCIAL_FEED}">
                      <title>Launch day</title>
                      <body>The community feed is live!</body>
                      <author>admin@localhost</author>
                      <published>2026-05-16T12:00:00Z</published>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("feed-publish"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type=\"result\""),
        "publish must succeed against the bootstrapped feed node: {publish_result}"
    );

    // Items query — the published entry should round-trip with the
    // typed `<entry/>` payload intact.
    client
        .send(&format!(
            r#"<iq type="get" id="feed-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{FEED_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = client
        .recv_matching(|frame| frame.contains("feed-items") && frame.contains("<entry"))
        .await
        .expect("items response");
    assert!(
        items_response.contains(&post_id),
        "items query missing published id: {items_response}"
    );
    assert!(
        items_response.contains("Launch day"),
        "items query lost title: {items_response}"
    );
    assert!(
        items_response.contains("The community feed is live!"),
        "items query lost body: {items_response}"
    );

    let _ = client.close().await;
}
