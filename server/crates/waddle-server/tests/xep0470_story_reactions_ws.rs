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
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_STORIES: &str = "urn:xmpp:stories:0";
const NS_ATTACHMENTS: &str = "urn:xmpp:pubsub-attachments:1";
const NS_ATTACHMENTS_SUMMARY: &str = "urn:xmpp:pubsub-attachments:summary:1";
const STORY_SUMMARY_NODE: &str = "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:stories:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains("<iq")
        && (frame.contains(&format!("id='{id}'")) || frame.contains(&format!("id=\"{id}\"")))
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame_has_iq_id(frame, id))
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
        .recv_matching(|frame| frame_has_iq_id(frame, id))
        .await
        .expect("iq get response")
}

fn story_attachment_node(story_id: &str) -> String {
    format!(
        "{NS_ATTACHMENTS}/xmpp:community.localhost?;node=urn%3Axmpp%3Astories%3A0;item={story_id}"
    )
}

async fn wait_for_event_message(
    client: &mut WsXmppClient,
    node: &str,
    dur: std::time::Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                let is_event_msg = frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node='{node}'"#))
                        || frame.contains(&format!(r#"node="{node}""#)));
                if is_event_msg {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
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

    let subscribe_summary = iq_set_to(
        &mut alice,
        "summary-subscribe",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{STORY_SUMMARY_NODE}" jid="alice@localhost"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        subscribe_summary.contains("type='result'"),
        "summary node subscription should succeed for a community member: {subscribe_summary}"
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

    let pushed_summary = wait_for_event_message(
        &mut alice,
        STORY_SUMMARY_NODE,
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("subscriber should receive summary fan-out after a story reaction changes");
    assert!(
        pushed_summary.contains(NS_ATTACHMENTS_SUMMARY)
            && pushed_summary.contains(&format!("id='{story_id}'"))
            && pushed_summary.contains("<reaction count='1'>👍</reaction>")
            && pushed_summary.contains("<reaction count='1'>❤️</reaction>"),
        "pushed summary event should contain per-emoji counts for the story: {pushed_summary}"
    );

    let summary = iq_get_to(
        &mut alice,
        "reaction-summary",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORY_SUMMARY_NODE}"><item id="{story_id}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        summary.contains("type='result'")
            && summary.contains(NS_ATTACHMENTS_SUMMARY)
            && summary.contains(&format!("id='{story_id}'"))
            && summary.contains("<reaction count='1'>👍</reaction>")
            && summary.contains("<reaction count='1'>❤️</reaction>"),
        "reaction summary should contain per-emoji counts for the story: {summary}"
    );

    let noticed_publish = iq_set_to(
        &mut admin,
        "noticed-publish",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="admin@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <noticed/>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        noticed_publish.contains("type='result'"),
        "noticed attachment publish should succeed: {noticed_publish}"
    );

    let noticed_summary = iq_get_to(
        &mut alice,
        "noticed-summary",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORY_SUMMARY_NODE}"><item id="{story_id}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        noticed_summary.contains("<noticed count='1'/>")
            && noticed_summary.contains("<reaction count='1'>👍</reaction>")
            && noticed_summary.contains("<reaction count='1'>❤️</reaction>"),
        "summary should include noticed and preserve reaction counts: {noticed_summary}"
    );

    let direct_summary_publish = iq_set_to(
        &mut alice,
        "direct-summary-publish",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{STORY_SUMMARY_NODE}">
                <item id="{story_id}">
                  <summary xmlns="{NS_ATTACHMENTS_SUMMARY}">
                    <noticed count="99"/>
                  </summary>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        direct_summary_publish.contains("type='error'")
            && direct_summary_publish.contains("forbidden"),
        "summary node must be server-maintained only: {direct_summary_publish}"
    );

    let manual_attachment_create = iq_set_to(
        &mut alice,
        "manual-attachment-create",
        "alice@localhost",
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{attachment_node}"/></pubsub>"#),
    )
    .await;
    assert!(
        manual_attachment_create.contains("type='error'")
            && manual_attachment_create.contains("forbidden"),
        "manual attachment node creation must be rejected: {manual_attachment_create}"
    );

    let manual_summary_create = iq_set_to(
        &mut alice,
        "manual-summary-create",
        "alice@localhost",
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><create node="{STORY_SUMMARY_NODE}"/></pubsub>"#),
    )
    .await;
    assert!(
        manual_summary_create.contains("type='error'")
            && manual_summary_create.contains("forbidden"),
        "manual summary node creation must be rejected: {manual_summary_create}"
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

    let wrong_host_node = format!(
        "{NS_ATTACHMENTS}/xmpp:other.localhost?;node=urn%3Axmpp%3Astories%3A0;item={story_id}"
    );
    let wrong_host_publish = iq_set_to(
        &mut alice,
        "reaction-wrong-host",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{wrong_host_node}">
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
        wrong_host_publish.contains("type='error'") && wrong_host_publish.contains("item-not-found"),
        "attachment node with non-canonical target host must not use story carve-out: {wrong_host_publish}"
    );

    let extra_param_node = format!(
        "{NS_ATTACHMENTS}/xmpp:community.localhost?;node=urn%3Axmpp%3Astories%3A0;item={story_id};foo=bar"
    );
    let extra_param_publish = iq_set_to(
        &mut alice,
        "reaction-extra-param",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{extra_param_node}">
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
        extra_param_publish.contains("type='error'")
            && extra_param_publish.contains("item-not-found"),
        "attachment node with extra target params must not use story carve-out: {extra_param_publish}"
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

    let invalid_jid_publish = iq_set_to(
        &mut alice,
        "reaction-invalid-jid",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="not a jid">
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
        invalid_jid_publish.contains("type='error'") && invalid_jid_publish.contains("bad-request"),
        "invalid attachment item id must be rejected as bad-request: {invalid_jid_publish}"
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

    let invalid_jid_retract = iq_set_to(
        &mut alice,
        "reaction-retract-invalid-jid",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><retract node="{attachment_node}"><item id="not a jid"/></retract></pubsub>"#
        ),
    )
    .await;
    assert!(
        invalid_jid_retract.contains("type='error'") && invalid_jid_retract.contains("bad-request"),
        "invalid retract item id must be rejected as bad-request: {invalid_jid_retract}"
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

    let pushed_retract_summary = wait_for_event_message(
        &mut alice,
        STORY_SUMMARY_NODE,
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("subscriber should receive summary fan-out after a story reaction retracts");
    assert!(
        pushed_retract_summary.contains(NS_ATTACHMENTS_SUMMARY)
            && pushed_retract_summary.contains(&format!("id='{story_id}'"))
            && !pushed_retract_summary.contains("<reaction count='1'>👍</reaction>")
            && !pushed_retract_summary.contains("<reaction count='1'>❤️</reaction>")
            && pushed_retract_summary.contains("<noticed count='1'/>"),
        "pushed retract summary should remove Alice's reactions and preserve noticed counts: {pushed_retract_summary}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn retracting_story_removes_attachment_node_and_summary_item() {
    let _guard = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let admin_password = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &admin_password,
        "story-owner-cleanup",
    )
    .await
    .expect("admin connect");
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "story-member-cleanup",
    )
    .await
    .expect("alice connect");

    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let owner_story = iq_set_to(
        &mut admin,
        "cleanup-story",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{STORIES_NODE}">
                <item id="{story_id}">
                  <story xmlns="{NS_STORIES}" expires="2030-01-01T12:00:00Z">
                    <body>story that will be removed</body>
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

    let attachment_node = story_attachment_node(&story_id);
    let reaction_publish = iq_set_to(
        &mut alice,
        "cleanup-reaction",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="alice@localhost">
                  <attachments xmlns="{NS_ATTACHMENTS}">
                    <reactions><reaction>👍</reaction></reactions>
                  </attachments>
                </item>
              </publish>
            </pubsub>"#
        ),
    )
    .await;
    assert!(
        reaction_publish.contains("type='result'"),
        "member reaction publish should succeed before story retract: {reaction_publish}"
    );

    let summary_before_retract = iq_get_to(
        &mut alice,
        "cleanup-summary-before",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORY_SUMMARY_NODE}"><item id="{story_id}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        summary_before_retract.contains(&format!("id='{story_id}'"))
            && summary_before_retract.contains("<reaction count='1'>👍</reaction>"),
        "summary should contain the story before retract: {summary_before_retract}"
    );

    let subscribe_stories = iq_set_to(
        &mut alice,
        "cleanup-stories-subscribe",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{STORIES_NODE}" jid="alice@localhost"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        subscribe_stories.contains("type='result'"),
        "stories node subscription should succeed for a community member: {subscribe_stories}"
    );

    let subscribe_summaries = iq_set_to(
        &mut alice,
        "cleanup-summaries-subscribe",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{STORY_SUMMARY_NODE}" jid="alice@localhost"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        subscribe_summaries.contains("type='result'"),
        "summary node subscription should succeed for a community member: {subscribe_summaries}"
    );

    let story_retract = iq_set_to(
        &mut admin,
        "cleanup-story-retract",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><retract node="{STORIES_NODE}"><item id="{story_id}"/></retract></pubsub>"#
        ),
    )
    .await;
    assert!(
        story_retract.contains("type='result'"),
        "owner story retract should succeed: {story_retract}"
    );

    let pushed_story_retract =
        wait_for_event_message(&mut alice, STORIES_NODE, std::time::Duration::from_secs(2))
            .await
            .expect("subscriber should receive story retract fan-out");
    assert!(
        pushed_story_retract.contains(&format!("id='{story_id}'"))
            && pushed_story_retract.contains("<retract"),
        "story retract notification should identify the removed story: {pushed_story_retract}"
    );
    let pushed_summary_retract = wait_for_event_message(
        &mut alice,
        STORY_SUMMARY_NODE,
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("summary subscriber should receive story summary retract fan-out");
    assert!(
        pushed_summary_retract.contains(&format!("id='{story_id}'"))
            && pushed_summary_retract.contains("<retract"),
        "summary retract notification should identify the removed story: {pushed_summary_retract}"
    );

    let story_after_retract = iq_get_to(
        &mut alice,
        "cleanup-story-after",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORIES_NODE}"><item id="{story_id}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        story_after_retract.contains("type='result'")
            && !story_after_retract.contains(&format!("id='{story_id}'")),
        "story reads should no longer surface the retracted story: {story_after_retract}"
    );

    let summary_after_retract = iq_get_to(
        &mut alice,
        "cleanup-summary-after",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORY_SUMMARY_NODE}"><item id="{story_id}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        summary_after_retract.contains("type='result'")
            && !summary_after_retract.contains(&format!("id='{story_id}'"))
            && !summary_after_retract.contains("<reaction count='1'>👍</reaction>"),
        "summary reads should no longer surface the retracted story: {summary_after_retract}"
    );

    let attachment_after_retract = iq_get_to(
        &mut alice,
        "cleanup-attachment-after",
        COMMUNITY_JID,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{attachment_node}"/></pubsub>"#),
    )
    .await;
    assert!(
        attachment_after_retract.contains("type='result'")
            && !attachment_after_retract.contains("alice@localhost")
            && !attachment_after_retract.contains("<attachments")
            && !attachment_after_retract.contains("<reaction"),
        "attachment reads should no longer surface orphaned reaction items: {attachment_after_retract}"
    );

    let reaction_after_retract = iq_set_to(
        &mut alice,
        "cleanup-reaction-after",
        COMMUNITY_JID,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}">
              <publish node="{attachment_node}">
                <item id="alice@localhost">
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
        reaction_after_retract.contains("type='error'")
            && reaction_after_retract.contains("item-not-found"),
        "reacting to a retracted story must not resurrect the attachment node: {reaction_after_retract}"
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}
