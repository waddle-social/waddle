//! xCal RECURRENCE-ID overrides + EXDATE integration tests over
//! WebSocket. Verifies that a single pubsub item can carry a master
//! event plus sibling `<vevent>` overrides (each with their own
//! `<recurrence-id>`), and that EXDATE values round-trip on the
//! master. Per ProtoXEP §"Calendar Items", components sharing a UID
//! MUST live in the same item.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const COMMUNITY_JID: &str = "community.localhost";
const EVENTS_NODE: &str = "urn:xmpp:calendar:0";
const NS_XCAL: &str = "urn:ietf:params:xml:ns:xcal";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("calendar-overrides-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn vcalendar_item_with_master_overrides_and_exdate_round_trips() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let event_id = format!("evt-{}", uuid::Uuid::new_v4());

    client
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
                        <dtend><date-time>2026-06-05T22:00:00Z</date-time></dtend>
                        <summary>Friday Game Night</summary>
                        <rrule>
                          <freq>WEEKLY</freq>
                          <byday><weekday>FR</weekday></byday>
                          <count>8</count>
                        </rrule>
                        <exdate><date-time>2026-06-19T19:00:00Z</date-time></exdate>
                      </vevent>
                      <vevent>
                        <uid>{event_id}</uid>
                        <recurrence-id><date-time>2026-06-12T19:00:00Z</date-time></recurrence-id>
                        <summary>Special: Halo Tournament</summary>
                        <dtstart><date-time>2026-06-12T20:00:00Z</date-time></dtstart>
                      </vevent>
                    </vcalendar>
                  </item>
                </publish>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send publish");
    let publish_result = client
        .recv_matching(|frame| frame.contains("cal-publish"))
        .await
        .expect("publish result");
    assert!(
        publish_result.contains("type='result'"),
        "publish must succeed: {publish_result}"
    );

    client
        .send(&format!(
            r#"<iq type="get" id="cal-items" to="{COMMUNITY_JID}">
              <pubsub xmlns="http://jabber.org/protocol/pubsub">
                <items node="{EVENTS_NODE}"/>
              </pubsub>
            </iq>"#
        ))
        .await
        .expect("send items query");
    let items_response = client
        .recv_matching(|frame| frame.contains("cal-items") && frame.contains("<vevent"))
        .await
        .expect("items response");

    assert!(
        items_response.contains(&event_id),
        "items query missing event id: {items_response}"
    );
    assert!(
        items_response.contains("Friday Game Night"),
        "master summary missing: {items_response}"
    );
    assert!(
        items_response.contains("Special: Halo Tournament"),
        "override summary missing: {items_response}"
    );
    assert!(
        items_response.contains("<recurrence-id"),
        "override recurrence-id missing: {items_response}"
    );
    assert!(
        items_response.contains("<exdate"),
        "EXDATE missing on master: {items_response}"
    );
    assert!(
        items_response.contains("2026-06-12T20:00:00")
            || items_response.contains("2026-06-12T20:00:00Z"),
        "override dtstart missing: {items_response}"
    );

    let _ = client.close().await;
}
