//! RSVP via xCal ATTENDEE — sibling pubsub item integration tests.
//!
//! Verifies that a non-admin user (no Publisher affiliation on the
//! events node) can publish a well-formed RSVP item for someone
//! else's master event, and that malformed RSVP shapes fall through
//! to the standard admin-only gate and are rejected.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const COMMUNITY_JID: &str = "community.localhost";
const EVENTS_NODE: &str = "urn:xmpp:calendar:0";
const FEED_NODE: &str = "urn:xmpp:pubsub-social-feed:1";
const NS_XCAL: &str = "urn:ietf:params:xml:ns:xcal";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

/// Start a server with an extra "bob" account so we can test the RSVP
/// path from a non-owner. Returns (server, admin client, bob client).
async fn setup() -> (TestServer, WsXmppClient, WsXmppClient) {
    let bob_password = "bob-rsvp-pw";
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password)]);

    let admin_pw = server.fixed_account_password().to_string();
    let admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        ADMIN,
        &admin_pw,
        &format!("admin-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connect");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connect");
    (server, admin, bob)
}

#[tokio::test]
async fn non_owner_can_publish_rsvp_for_master_event() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut admin, mut bob) = setup().await;

    let event_id = format!("evt-{}", uuid::Uuid::new_v4());

    // Admin (server owner) publishes the master event.
    admin
        .send(&format!(
            r#"<iq type="set" id="cal-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{EVENTS_NODE}">
                  <item id="{event_id}">
                    <vcalendar xmlns="{NS_XCAL}">
                      <version>2.0</version>
                      <vevent>
                        <uid>{event_id}</uid>
                        <dtstamp>2026-06-01T12:00:00Z</dtstamp>
                        <dtstart><date-time>2026-06-05T19:00:00Z</date-time></dtstart>
                        <summary>Friday Game Night</summary>
                        <organizer>xmpp:admin@localhost</organizer>
                      </vevent>
                    </vcalendar>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send master publish");
    let publish_result = admin
        .recv_matching(|frame| frame.contains("cal-publish"))
        .await
        .expect("master publish result");
    assert!(
        publish_result.contains("type='result'"),
        "master publish must succeed: {publish_result}"
    );

    // Bob (a regular user, no Publisher affiliation on the events
    // node) publishes their RSVP as a sibling item.
    let rsvp_id = format!("{event_id}-rsvp-bob");
    bob.send(&format!(
        r#"<iq type="set" id="rsvp-1" to="{COMMUNITY_JID}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{EVENTS_NODE}">
              <item id="{rsvp_id}">
                <vcalendar xmlns="{NS_XCAL}">
                  <vevent>
                    <uid>{event_id}</uid>
                    <attendee partstat="ACCEPTED">xmpp:bob@localhost</attendee>
                  </vevent>
                </vcalendar>
              </item>
            </publish>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("send rsvp publish");
    let rsvp_result = bob
        .recv_matching(|frame| frame.contains("rsvp-1"))
        .await
        .expect("rsvp publish result");
    assert!(
        rsvp_result.contains("type='result'"),
        "rsvp publish must succeed for non-owner: {rsvp_result}"
    );

    // Items query — both the master and the RSVP should appear.
    admin
        .send(&format!(
            r#"<iq type="get" id="cal-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{EVENTS_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = admin
        .recv_matching(|frame| frame.contains("cal-items") && frame.contains("<vevent"))
        .await
        .expect("items response");
    assert!(
        items_response.contains(&event_id),
        "items missing master id: {items_response}"
    );
    assert!(
        items_response.contains(&rsvp_id),
        "items missing RSVP sibling id: {items_response}"
    );
    assert!(
        items_response.contains("xmpp:bob@localhost"),
        "items missing attendee URI: {items_response}"
    );
    assert!(
        items_response.contains("partstat='ACCEPTED'")
            || items_response.contains("partstat='ACCEPTED'"),
        "items missing ACCEPTED partstat: {items_response}"
    );

    let _ = admin.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn malformed_rsvp_for_other_user_is_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut admin, mut bob) = setup().await;

    let event_id = format!("evt-{}", uuid::Uuid::new_v4());

    // Admin publishes a master event.
    admin
        .send(&format!(
            r#"<iq type="set" id="cal-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{EVENTS_NODE}">
                  <item id="{event_id}">
                    <vcalendar xmlns="{NS_XCAL}">
                      <vevent>
                        <uid>{event_id}</uid>
                        <dtstart><date-time>2026-06-05T19:00:00Z</date-time></dtstart>
                        <summary>Game Night</summary>
                      </vevent>
                    </vcalendar>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send master publish");
    let _ = admin
        .recv_matching(|frame| frame.contains("cal-publish"))
        .await
        .expect("master publish result");

    // Bob tries to RSVP on behalf of admin — attendee URI doesn't
    // match Bob's session. Must fall through to the admin-only gate
    // and be rejected with Forbidden.
    let attack_id = format!("{event_id}-rsvp-admin");
    bob.send(&format!(
        r#"<iq type="set" id="rsvp-spoof" to="{COMMUNITY_JID}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{EVENTS_NODE}">
              <item id="{attack_id}">
                <vcalendar xmlns="{NS_XCAL}">
                  <vevent>
                    <uid>{event_id}</uid>
                    <attendee partstat="ACCEPTED">xmpp:admin@localhost</attendee>
                  </vevent>
                </vcalendar>
              </item>
            </publish>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("send spoofed rsvp");
    let spoof_result = bob
        .recv_matching(|frame| frame.contains("rsvp-spoof"))
        .await
        .expect("spoof result");
    assert!(
        spoof_result.contains("type='error'") || spoof_result.contains("<error"),
        "spoofed RSVP must be rejected: {spoof_result}"
    );

    // Bob tries to publish a non-RSVP-shaped item under their own
    // RSVP id (carries a SUMMARY, which is a master-only field).
    // Must also fall through to the admin gate and be rejected.
    let rich_id = format!("{event_id}-rsvp-bob");
    bob.send(&format!(
        r#"<iq type="set" id="rsvp-rich" to="{COMMUNITY_JID}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{EVENTS_NODE}">
              <item id="{rich_id}">
                <vcalendar xmlns="{NS_XCAL}">
                  <vevent>
                    <uid>{event_id}</uid>
                    <summary>Hostile takeover</summary>
                    <attendee partstat="ACCEPTED">xmpp:bob@localhost</attendee>
                  </vevent>
                </vcalendar>
              </item>
            </publish>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("send rich rsvp");
    let rich_result = bob
        .recv_matching(|frame| frame.contains("rsvp-rich"))
        .await
        .expect("rich result");
    assert!(
        rich_result.contains("type='error'") || rich_result.contains("<error"),
        "RSVP item with master-event fields must be rejected: {rich_result}"
    );

    let _ = admin.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn rsvp_publish_bridges_to_social_feed() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut admin, mut bob) = setup().await;

    let event_id = format!("evt-{}", uuid::Uuid::new_v4());

    // Admin publishes the master event with a recognisable summary.
    admin
        .send(&format!(
            r#"<iq type="set" id="cal-publish" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <publish node="{EVENTS_NODE}">
                  <item id="{event_id}">
                    <vcalendar xmlns="{NS_XCAL}">
                      <version>2.0</version>
                      <vevent>
                        <uid>{event_id}</uid>
                        <dtstart><date-time>2026-06-05T19:00:00Z</date-time></dtstart>
                        <summary>Friday Game Night</summary>
                      </vevent>
                    </vcalendar>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send master publish");
    let _ = admin
        .recv_matching(|frame| frame.contains("cal-publish"))
        .await
        .expect("master publish result");

    // Bob RSVPs ACCEPTED.
    let rsvp_id = format!("{event_id}-rsvp-bob");
    bob.send(&format!(
        r#"<iq type="set" id="rsvp-1" to="{COMMUNITY_JID}">
          <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="{EVENTS_NODE}">
              <item id="{rsvp_id}">
                <vcalendar xmlns="{NS_XCAL}">
                  <vevent>
                    <uid>{event_id}</uid>
                    <attendee partstat="ACCEPTED">xmpp:bob@localhost</attendee>
                  </vevent>
                </vcalendar>
              </item>
            </publish>
          </pubsub>
        </iq>"#
    ))
    .await
    .expect("send rsvp");
    let rsvp_result = bob
        .recv_matching(|frame| frame.contains("rsvp-1"))
        .await
        .expect("rsvp publish result");
    assert!(
        rsvp_result.contains("type='result'"),
        "rsvp publish must succeed: {rsvp_result}"
    );

    // Query the social feed — the bridge should have shadow-published
    // a "bob is going to Friday Game Night" entry tagged with the
    // RSVP source kind.
    admin
        .send(&format!(
            r#"<iq type="get" id="feed-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{FEED_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send feed query");
    let feed_response = admin
        .recv_matching(|frame| frame.contains("feed-items") && frame.contains("<entry"))
        .await
        .expect("feed response");

    assert!(
        feed_response.contains("<author><uri>xmpp:bob@localhost</uri></author>"),
        "feed entry must carry bob's bare JID: {feed_response}"
    );
    assert!(
        feed_response.contains("is going to Friday Game Night"),
        "feed entry must carry the RSVP summary: {feed_response}"
    );
    assert!(
        feed_response.contains("kind='rsvp'") || feed_response.contains("kind='rsvp'"),
        "feed entry must tag the RSVP source kind: {feed_response}"
    );

    let _ = admin.close().await;
    let _ = bob.close().await;
}
