//! XEP-0470 Pubsub Attachments on XEP-0501 stories.
//!
//! Verifies the pull-based story reaction spine: story publishes remain
//! owner-gated, while a non-owner member can publish/retract their own
//! `<attachments/>` item under the story attachment node.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const COMMUNITY_JID: &str = "community.localhost";
const STORIES_NODE: &str = "urn:xmpp:stories:0";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_STORIES: &str = "urn:xmpp:stories:0";
const NS_ATTACHMENTS: &str = "urn:xmpp:pubsub-attachments:1";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!("id='{id}'")) && frame.contains("<iq"))
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
        .recv_matching(|frame| frame.contains(&format!("id='{id}'")) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

fn story_attachment_node(story_id: &str) -> String {
    format!(
        "{NS_ATTACHMENTS}/xmpp:community.localhost?;node=urn%3Axmpp%3Astories%3A0;item={story_id}"
    )
}

#[tokio::test]
async fn member_can_publish_read_and_retract_story_reaction_attachment() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let admin_password = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_password,
        "story-owner",
    )
    .await
    .expect("admin connect");
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "story-member",
    )
    .await
    .expect("alice connect");

    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let owner_story = iq_set_to(
        &mut admin,
        "owner-story",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{STORIES_NODE}">
                <item id="{story_id}">
                  <story xmlns="{NS_STORIES}" expires="2030-01-01T12:00:00Z">
                    <body>story with reactions</body>
                    <author>admin@localhost</author>
                  </story>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        owner_story.contains("type='result'"),
        "owner story publish should succeed: {owner_story}"
    );

    let member_story = iq_set_to(
        &mut alice,
        "member-story",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{STORIES_NODE}">
                <item id="member-{story_id}">
                  <story xmlns="{NS_STORIES}" expires="2030-01-01T12:00:00Z">
                    <body>must not publish</body>
                    <author>alice@localhost</author>
                  </story>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        member_story.contains("type='error'") && member_story.contains("forbidden"),
        "non-owner story publish must remain forbidden: {member_story}"
    );

    let attachment_node = story_attachment_node(&story_id);
    let reaction_publish = iq_set_to(
        &mut alice,
        "reaction-publish",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="alice@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <reactions>
                      <reaction>👍</reaction>
                      <reaction>❤️</reaction>
                    </reactions>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        reaction_publish.contains("type='result'"),
        "member reaction publish should succeed via story carve-out: {reaction_publish}"
    );

    let missing_node = story_attachment_node("missing-story");
    let missing_story_publish = iq_set_to(
        &mut alice,
        "reaction-missing-story",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{missing_node}">
                <item id="alice@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <reactions><reaction>👀</reaction></reactions>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        missing_story_publish.contains("type='error'")
            && missing_story_publish.contains("item-not-found"),
        "reaction attachment for a missing story must fail: {missing_story_publish}"
    );

    let crafted_node = format!(
        "{NS_ATTACHMENTS}/xmpp:community.localhost?;node=urn%3Axmpp%3Astories%3A0evil;item={story_id}"
    );
    let crafted_publish = iq_set_to(
        &mut alice,
        "reaction-crafted-node",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{crafted_node}">
                <item id="alice@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <reactions><reaction>👀</reaction></reactions>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        crafted_publish.contains("type='error'") && crafted_publish.contains("item-not-found"),
        "crafted non-story attachment node must not use story carve-out: {crafted_publish}"
    );

    let spoof_publish = iq_set_to(
        &mut alice,
        "reaction-spoof",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="bob@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <reactions><reaction>🔥</reaction></reactions>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        spoof_publish.contains("type='error'") && spoof_publish.contains("bad-request"),
        "spoofed attachment item id must be rejected as bad-request: {spoof_publish}"
    );

    let bad_payload = iq_set_to(
        &mut alice,
        "reaction-bad-payload",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="alice@localhost">
                  <not-attachments xmlns="{NS_ATTACHMENTS}"/>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        bad_payload.contains("type='error'") && bad_payload.contains("bad-request"),
        "non-attachments payload must be rejected as bad-request: {bad_payload}"
    );

    let readback = iq_get_to(
        &mut alice,
        "reaction-items",
        COMMUNITY_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{attachment_node}"/></pubsub>"#),
    )
    .await;
    assert!(
        readback.contains("alice@localhost")
            && readback.contains("👍")
            && readback.contains("❤️")
            && readback.contains(NS_ATTACHMENTS),
        "reaction attachment should read back from auto-created node: {readback}"
    );

    let retract = iq_set_to(
        &mut alice,
        "reaction-retract",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><retract node="{attachment_node}"><item id="alice@localhost"/></retract></pubsub>"#
        ),
    )
    .await;
    assert!(
        retract.contains("type='result'"),
        "member should retract own story reaction item: {retract}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}
