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
const MEMBER_USERNAME: &str = "member";
const MEMBER_PASSWORD: &str = "xep0472-member-password";
const MEMBER2_USERNAME: &str = "member2";
const MEMBER2_PASSWORD: &str = "xep0472-member2-password";
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

/// Start a server with a regular (non-owner) member account seeded and
/// connect as that member. Only `admin` is a server owner
/// (`WADDLE_SERVER_OWNER_LOCALPARTS`), so `member` exercises the
/// member-write path on the social feed.
async fn setup_member() -> (TestServer, WsXmppClient) {
    let server = TestServer::start_with_extra_accounts(&[(MEMBER_USERNAME, MEMBER_PASSWORD)]);
    let resource = format!("xep0472-member-{}", uuid::Uuid::new_v4());
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        &resource,
    )
    .await
    .expect("connect and auth as member");
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
        frame.contains(&format!("var='{NS_SOCIAL_FEED}'"))
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
        publish_result.contains("type='result'"),
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

#[tokio::test]
async fn member_can_publish_to_social_feed() {
    // XEP-0472 §"Replying to a Post": "Anyone can publish a post" to a
    // shared feed node. A non-owner community member must be able to
    // post to the global social feed (the read path is already open).
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup_member().await;

    // Spoof BOTH the item `publisher` attribute and the payload
    // `<author>` to point at admin — the server MUST ignore both and
    // stamp the authenticated session JID instead (XEP-0060 §7.1.2.1 for
    // the publisher; author stamping prevents displayed-author spoofing).
    let post_id = format!("post-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<iq type="set" id="member-feed-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="{post_id}" publisher="admin@{DOMAIN}">
                    <entry xmlns="{NS_SOCIAL_FEED}">
                      <title>Hello from a member</title>
                      <body>Members can post to the feed now.</body>
                      <author>admin@{DOMAIN}</author>
                      <published>2026-06-17T12:00:00Z</published>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("member-feed-publish"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type='result'"),
        "a non-owner member must be allowed to publish to the social feed: {publish_result}"
    );

    // The member's post round-trips through an items query, stamped with
    // the server-derived publisher and author (not the spoofed admin JID).
    client
        .send(&format!(
            r#"<iq type="get" id="member-feed-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{FEED_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = client
        .recv_matching(|frame| frame.contains("member-feed-items") && frame.contains("<entry"))
        .await
        .expect("items response");
    assert!(
        items_response.contains(&post_id),
        "member's published post must round-trip through items: {items_response}"
    );
    assert!(
        items_response.contains(&format!("publisher='{MEMBER_USERNAME}@{DOMAIN}'")),
        "feed item must be stamped with the server-derived publisher: {items_response}"
    );
    assert!(
        items_response.contains(&format!(">{MEMBER_USERNAME}@{DOMAIN}<")),
        "feed entry author must be stamped with the authenticated JID: {items_response}"
    );
    assert!(
        !items_response.contains(&format!("admin@{DOMAIN}")),
        "spoofed publisher AND author must be overridden by the server: {items_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn member_cannot_clobber_another_members_feed_post() {
    // The feed is one shared, open-publish node with client-chosen item
    // ids. A member must not be able to overwrite another member's post by
    // reusing its id.
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        (MEMBER_USERNAME, MEMBER_PASSWORD),
        (MEMBER2_USERNAME, MEMBER2_PASSWORD),
    ]);
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        &format!("a-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect member A");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER2_USERNAME,
        MEMBER2_PASSWORD,
        &format!("b-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect member B");

    let post_id = format!("post-{}", uuid::Uuid::new_v4());
    alice
        .send(&format!(
            r#"<iq type="set" id="a-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="{post_id}">
                    <entry xmlns="{NS_SOCIAL_FEED}"><body>Original by A</body></entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("A publish");
    let a_result = alice
        .recv_matching(|frame| frame.contains("a-publish"))
        .await
        .expect("A result");
    assert!(
        a_result.contains("type='result'"),
        "member A publish must succeed: {a_result}"
    );

    bob.send(&format!(
        r#"<iq type="set" id="b-clobber" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="{post_id}">
                    <entry xmlns="{NS_SOCIAL_FEED}"><body>Hijacked by B</body></entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
    ))
    .await
    .expect("B publish");
    let b_result = bob
        .recv_matching(|frame| frame.contains("b-clobber"))
        .await
        .expect("B result");
    assert!(
        b_result.contains("type='error'") && b_result.contains("forbidden"),
        "member B must not overwrite member A's post: {b_result}"
    );

    // A's content is intact; B's clobber never landed.
    alice
        .send(&format!(
            r#"<iq type="get" id="verify-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{FEED_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("items query");
    let items = alice
        .recv_matching(|frame| frame.contains("verify-items") && frame.contains("<entry"))
        .await
        .expect("items response");
    assert!(
        items.contains("Original by A"),
        "member A's content must be intact: {items}"
    );
    assert!(
        !items.contains("Hijacked by B"),
        "member B's clobber must not have landed: {items}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}
