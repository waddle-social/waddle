//! XEP-0501 Pubsub Stories integration tests over WebSocket.
//!
//! Verifies the server bootstrap: the spaces service hosts the
//! global `urn:xmpp:pubsub-social-feed:stories:0` node, advertises the namespace on
//! disco#info, and accepts publish + items-query for Atom `<entry/>`
//! payloads built per XEP-0501.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const MEMBER_USERNAME: &str = "member";
const MEMBER_PASSWORD: &str = "xep0501-member-password";
const OTHER_MEMBER_USERNAME: &str = "other";
const OTHER_MEMBER_PASSWORD: &str = "xep0501-other-password";
const COMMUNITY_JID: &str = "community.localhost";
const STORIES_NODE: &str = "urn:xmpp:pubsub-social-feed:stories:0";
const NS_STORIES: &str = "urn:xmpp:pubsub-social-feed:stories:0";
const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
const NS_WADDLE_STORIES: &str = "urn:waddle:stories:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn story_entry_xml(
    item_id: &str,
    body: &str,
    media_url: &str,
    media_type: &str,
    author: &str,
    expires: &str,
) -> String {
    format!(
        r#"<entry xmlns="{NS_ATOM}">
          <title type="text">{body}</title>
          <id>{item_id}</id>
          <published>2026-06-01T12:00:00Z</published>
          <updated>2026-06-01T12:00:00Z</updated>
          <content type="text">{body}</content>
          <author><uri>xmpp:{author}</uri></author>
          <link rel="enclosure" href="{media_url}" type="{media_type}"/>
          <expires xmlns="{NS_WADDLE_STORIES}">{expires}</expires>
        </entry>"#
    )
}

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
async fn stories_node_config_declares_xep0501_pubsub_type() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    client
        .send(&format!(
            r#"<iq type="get" id="stories-node-config" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                <configure node="{STORIES_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send stories node configure get");
    let frame = client
        .recv_matching(|frame| frame.contains("stories-node-config"))
        .await
        .expect("stories node configure response");
    assert!(
        frame.contains("type='result'")
            && frame.contains("var='pubsub#type'")
            && frame.contains(&format!("<value>{STORIES_NODE}</value>")),
        "stories node configure form must declare the XEP-0501 pubsub#type: {frame}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn member_story_publish_requires_story_payload() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(MEMBER_USERNAME, MEMBER_PASSWORD)]);
    let resource = format!("xep0501-member-invalid-{}", uuid::Uuid::new_v4());
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        &resource,
    )
    .await
    .expect("connect and auth as member");

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-missing-payload" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}"><item id="missing-payload"/></publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send missing payload publish");
    let missing_payload = client
        .recv_matching(|frame| frame.contains("member-story-missing-payload"))
        .await
        .expect("missing payload response");
    assert!(
        missing_payload.contains("type='error'") && missing_payload.contains("payload-required"),
        "story publish without an Atom entry must be payload-required: {missing_payload}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-invalid-payload" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="invalid-payload">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">not a story</title>
                      <id>invalid-payload</id>
                      <updated>2026-06-01T12:00:00Z</updated>
                      <content type="text">missing enclosure</content>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send invalid payload publish");
    let invalid_payload = client
        .recv_matching(|frame| frame.contains("member-story-invalid-payload"))
        .await
        .expect("invalid payload response");
    assert!(
        invalid_payload.contains("type='error'") && invalid_payload.contains("invalid-payload"),
        "story publish with non-story payload must be invalid-payload: {invalid_payload}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-empty-payload" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="empty-story">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">no media</title>
                      <id>empty-story</id>
                      <updated>2026-06-01T12:00:00Z</updated>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send empty story publish");
    let empty_payload = client
        .recv_matching(|frame| frame.contains("member-story-empty-payload"))
        .await
        .expect("empty story response");
    assert!(
        empty_payload.contains("type='error'") && empty_payload.contains("invalid-payload"),
        "story publish with no enclosure must be invalid-payload: {empty_payload}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-missing-updated" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="missing-updated-story">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">missing updated</title>
                      <id>missing-updated-story</id>
                      <link rel="enclosure" href="https://example.com/photo.jpg" type="image/jpeg"/>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send story missing updated publish");
    let missing_updated = client
        .recv_matching(|frame| frame.contains("member-story-missing-updated"))
        .await
        .expect("story missing updated response");
    assert!(
        missing_updated.contains("type='error'") && missing_updated.contains("invalid-payload"),
        "story publish missing required Atom fields must be invalid-payload: {missing_updated}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-blank-href" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="blank-href-story">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">blank media</title>
                      <id>blank-href-story</id>
                      <updated>2026-06-01T12:00:00Z</updated>
                      <link rel="enclosure" href="   " type="image/jpeg"/>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send story blank href publish");
    let blank_href = client
        .recv_matching(|frame| frame.contains("member-story-blank-href"))
        .await
        .expect("story blank href response");
    assert!(
        blank_href.contains("type='error'") && blank_href.contains("invalid-payload"),
        "story publish with blank media href must be invalid-payload: {blank_href}"
    );

    client
        .send(&format!(
            r#"<iq type="set" id="member-story-bad-published" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="bad-published-story">
                    <entry xmlns="{NS_ATOM}">
                      <title type="text">bad published</title>
                      <id>bad-published-story</id>
                      <updated>2026-06-01T12:00:00Z</updated>
                      <published>not-a-date</published>
                      <link rel="enclosure" href="https://example.com/photo.jpg" type="image/jpeg"/>
                    </entry>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send story bad published publish");
    let bad_published = client
        .recv_matching(|frame| frame.contains("member-story-bad-published"))
        .await
        .expect("story bad published response");
    assert!(
        bad_published.contains("type='error'") && bad_published.contains("invalid-payload"),
        "story publish with invalid published timestamp must be invalid-payload: {bad_published}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn member_can_retract_own_story_but_not_another_member_story() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        (MEMBER_USERNAME, MEMBER_PASSWORD),
        (OTHER_MEMBER_USERNAME, OTHER_MEMBER_PASSWORD),
    ]);
    let mut member = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        &format!("xep0501-member-retract-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect and auth as member");
    let mut other = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        OTHER_MEMBER_USERNAME,
        OTHER_MEMBER_PASSWORD,
        &format!("xep0501-other-retract-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect and auth as other member");

    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    member
        .send(&format!(
            r#"<iq type="set" id="member-story-for-retract" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="{story_id}">
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            story_entry_xml(
                &story_id,
                "member-owned story",
                "https://example.com/member-owned-story.jpg",
                "image/jpeg",
                "member@localhost",
                "2030-01-01T12:00:00Z",
            )
        ))
        .await
        .expect("send publish");
    let publish_result = member
        .recv_matching(|frame| frame.contains("member-story-for-retract"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type='result'"),
        "member story publish must succeed before retract: {publish_result}"
    );

    other
        .send(&format!(
            r#"<iq type="set" id="other-member-story-retract" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <retract node="{STORIES_NODE}"><item id="{story_id}"/></retract>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send other member retract");
    let other_retract = other
        .recv_matching(|frame| frame.contains("other-member-story-retract"))
        .await
        .expect("other retract response");
    assert!(
        other_retract.contains("type='error'") && other_retract.contains("forbidden"),
        "another member must not retract a member-owned story: {other_retract}"
    );

    member
        .send(&format!(
            r#"<iq type="set" id="own-member-story-retract" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <retract node="{STORIES_NODE}"><item id="{story_id}"/></retract>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send own retract");
    let own_retract = member
        .recv_matching(|frame| frame.contains("own-member-story-retract"))
        .await
        .expect("own retract response");
    assert!(
        own_retract.contains("type='result'"),
        "member should retract own story: {own_retract}"
    );

    member
        .send(&format!(
            r#"<iq type="get" id="own-member-story-after-retract" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{STORIES_NODE}"><item id="{story_id}"/></items>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let after_retract = member
        .recv_matching(|frame| frame.contains("own-member-story-after-retract"))
        .await
        .expect("items response after retract");
    assert!(
        after_retract.contains("type='result'") && !after_retract.contains(&story_id),
        "retracted member story should not be returned: {after_retract}"
    );

    let _ = member.close().await;
    let _ = other.close().await;
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
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            story_entry_xml(
                &story_id,
                "Look at this!",
                "https://example.com/photo.jpg",
                "image/jpeg",
                "admin@localhost",
                expires,
            )
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
    // typed Atom payload intact (id, expires, body, enclosure link).
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
        .recv_matching(|frame| frame.contains("story-items") && frame.contains("<entry"))
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
        "items query lost enclosure href: {items_response}"
    );
    assert!(
        items_response.contains(expires),
        "items query lost expires attr: {items_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn member_can_publish_story_media() {
    // Stories are community-broadcast content. A non-owner authenticated
    // member may publish media, and the service stamps the author from
    // the authenticated session rather than trusting the payload.
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(MEMBER_USERNAME, MEMBER_PASSWORD)]);
    let resource = format!("xep0501-member-{}", uuid::Uuid::new_v4());
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        MEMBER_USERNAME,
        MEMBER_PASSWORD,
        &resource,
    )
    .await
    .expect("connect and auth as member");

    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<iq type="set" id="member-story-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{STORIES_NODE}">
                  <item id="{story_id}">
                    {}
                  </item>
                </publish>
              </pubsub>
            </iq>"#,
            story_entry_xml(
                &story_id,
                "From the new office",
                "https://example.com/member-photo.jpg",
                "image/jpeg",
                "spoofed@localhost",
                "2030-01-01T12:00:00Z",
            )
        ))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("member-story-publish"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type='result'"),
        "member story media publish must succeed: {publish_result}"
    );

    client
        .send(&format!(
            r#"<iq type="get" id="member-story-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{STORIES_NODE}"><item id="{story_id}"/></items>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = client
        .recv_matching(|frame| frame.contains("member-story-items") && frame.contains("<entry"))
        .await
        .expect("items response");
    assert!(
        items_response.contains("From the new office"),
        "items query lost body: {items_response}"
    );
    assert!(
        items_response.contains("https://example.com/member-photo.jpg"),
        "items query lost enclosure href: {items_response}"
    );
    assert!(
        items_response.contains(&format!("{MEMBER_USERNAME}@{DOMAIN}")),
        "server did not stamp member author: {items_response}"
    );
    assert!(
        !items_response.contains("spoofed@localhost"),
        "server trusted spoofed story author: {items_response}"
    );

    let _ = client.close().await;
}
