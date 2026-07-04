//! XEP-0472 Pubsub Social Feed integration tests over WebSocket.
//!
//! Verifies the server-side bootstrap: the spaces service hosts the
//! global `urn:xmpp:pubsub-social-feed:1` node, advertises the
//! namespace on disco#info, and accepts publish + items-query for
//! `<entry/>` payloads built per XEP-0472 §3.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const MEMBER_USERNAME: &str = "member";
const MEMBER_PASSWORD: &str = "xep0472-member-password";
const MEMBER2_USERNAME: &str = "member2";
const MEMBER2_PASSWORD: &str = "xep0472-member2-password";
const COMMUNITY_JID: &str = "community.localhost";
const FEED_NODE: &str = "urn:xmpp:pubsub-social-feed:1";
const NS_SOCIAL_FEED: &str = "urn:xmpp:pubsub-social-feed:1";
const NS_ATOM: &str = "http://www.w3.org/2005/Atom";

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

fn feed_entry_xml(item_id: &str, title: &str, body: &str, author: &str) -> String {
    format!(
        r#"<entry xmlns="{NS_ATOM}">
          <title type="text">{title}</title>
          <id>tag:localhost,2026:{item_id}</id>
          <published>2026-06-17T12:00:00Z</published>
          <updated>2026-06-17T12:00:00Z</updated>
          <content type="text">{body}</content>
          <author><uri>xmpp:{author}</uri></author>
        </entry>"#
    )
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
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            feed_entry_xml(
                &post_id,
                "Launch day",
                "The community feed is live!",
                "admin@localhost",
            )
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
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            feed_entry_xml(
                &post_id,
                "Hello from a member",
                "Members can post to the feed now.",
                &format!("admin@{DOMAIN}"),
            )
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
        items_response.contains(&format!("xmpp:{MEMBER_USERNAME}@{DOMAIN}")),
        "feed entry author URI must be stamped with the authenticated JID: {items_response}"
    );
    assert!(
        !items_response.contains(&format!("admin@{DOMAIN}")),
        "spoofed publisher AND author must be overridden by the server: {items_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn member_feed_publish_requires_atom_payload() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup_member().await;

    client
        .send(&format!(
            r#"<iq type="set" id="feed-missing-payload" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}"><item id="missing-feed-payload"/></publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send missing payload publish");
    let missing_payload = client
        .recv_matching(|frame| frame.contains("feed-missing-payload"))
        .await
        .expect("missing payload response");
    assert!(
        missing_payload.contains("type='error'") && missing_payload.contains("payload-required"),
        "feed publish without Atom entry must be payload-required: {missing_payload}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="feed-invalid-payload" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="invalid-feed-payload">
                    <entry xmlns="{NS_ATOM}">
                      <id>invalid-feed-payload</id>
                      <updated>2026-06-17T12:00:00Z</updated>
                      <content type="text">missing title</content>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send invalid payload publish");
    let invalid_payload = client
        .recv_matching(|frame| frame.contains("feed-invalid-payload"))
        .await
        .expect("invalid payload response");
    assert!(
        invalid_payload.contains("type='error'") && invalid_payload.contains("invalid-payload"),
        "feed publish with malformed Atom entry must be invalid-payload: {invalid_payload}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="feed-invalid-atom-updated" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{FEED_NODE}">
                  <item id="invalid-feed-updated">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">missing updated</title>
                      <id>invalid-feed-updated</id>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send invalid Atom publish");
    let invalid_atom = client
        .recv_matching(|frame| frame.contains("feed-invalid-atom-updated"))
        .await
        .expect("invalid Atom response");
    assert!(
        invalid_atom.contains("type='error'") && invalid_atom.contains("invalid-payload"),
        "feed publish missing required Atom fields must be invalid-payload: {invalid_atom}"
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
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            feed_entry_xml(
                &post_id,
                "Original by A",
                "Original by A",
                "member@localhost"
            )
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
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
        feed_entry_xml(
            &post_id,
            "Hijacked by B",
            "Hijacked by B",
            "member2@localhost"
        )
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
