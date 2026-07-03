//! PEP → community feed bridge integration tests over WebSocket.
//!
//! Publishes a PEP mood update from a regular user and verifies that
//! a typed feed entry appears on the community feed node, carrying
//! the author's bare JID and a `<source kind='mood'/>` typed child.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const COMMUNITY_JID: &str = "community.localhost";
const FEED_NODE: &str = "urn:xmpp:pubsub-social-feed:1";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("pep-bridge-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn pep_mood_publish_emits_typed_feed_entry() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    // User publishes a mood to their PEP service (self-targeted: no
    // `to` attribute means the server treats it as PEP on the bound
    // user JID).
    client
        .send(
            r#"<iq type="set" id="pep-mood">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="http://jabber.org/protocol/mood">
                  <item>
                    <mood xmlns="http://jabber.org/protocol/mood">
                      <happy/>
                    </mood>
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
        )
        .await
        .expect("send PEP publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("pep-mood"))
        .await
        .expect("PEP publish result");
    assert!(
        publish_result.contains("type='result'"),
        "PEP publish must succeed: {publish_result}"
    );

    // Query the community feed node — the bridge should have
    // shadow-published an entry with author=admin and source kind=mood.
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
        items_response.contains("xmpp:admin@localhost"),
        "bridged entry must carry author URI: {items_response}"
    );
    assert!(
        items_response.contains("is feeling happy"),
        "bridged entry must carry the rendered summary: {items_response}"
    );
    assert!(
        items_response.contains("urn:waddle:feed-source:0"),
        "bridged entry must declare the source namespace: {items_response}"
    );
    assert!(
        items_response.contains("kind='mood'") || items_response.contains("kind='mood'"),
        "bridged entry must tag the source kind: {items_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn pep_mood_publish_is_throttled_on_identical_repeat() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    // First publish — should bridge.
    client
        .send(
            r#"<iq type="set" id="pep-mood-1">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="http://jabber.org/protocol/mood">
                  <item>
                    <mood xmlns="http://jabber.org/protocol/mood"><happy/></mood>
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
        )
        .await
        .expect("send first PEP publish");
    let _ = client
        .recv_matching(|frame| frame.contains("pep-mood-1"))
        .await
        .expect("first publish result");

    // Identical second publish within the cooldown — should NOT
    // generate a second feed entry.
    client
        .send(
            r#"<iq type="set" id="pep-mood-2">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="http://jabber.org/protocol/mood">
                  <item>
                    <mood xmlns="http://jabber.org/protocol/mood"><happy/></mood>
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
        )
        .await
        .expect("send second PEP publish");
    let _ = client
        .recv_matching(|frame| frame.contains("pep-mood-2"))
        .await
        .expect("second publish result");

    // Items query — should see exactly one bridged entry.
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
        .recv_matching(|frame| frame.contains("feed-items"))
        .await
        .expect("items response");

    // Count the bridged entries (items with our source-kind tag).
    let kind_hits = items_response.matches("kind='mood'").count()
        + items_response.matches("kind=\"mood\"").count();
    assert_eq!(
        kind_hits, 1,
        "exactly one bridged entry expected — throttle must suppress identical repeats: {items_response}"
    );

    let _ = client.close().await;
}
