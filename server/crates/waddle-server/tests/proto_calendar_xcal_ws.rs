//! xCal (`urn:ietf:params:xml:ns:xcal`) integration tests over
//! WebSocket. Calendar events use the XSF ProtoXEP "Calendaring
//! Extensions to Publish-Subscribe" wire shape: pubsub items wrap a
//! `<vcalendar><vevent/></vcalendar>` payload built from iCalendar
//! (RFC 5545) properties, with `<rrule/>` for recurrence.

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
    let resource = format!("calendar-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn community_disco_info_advertises_xcal_namespace() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    client
        .send(&format!(
            r#"<iq type="get" id="disco-cal" to="{COMMUNITY_JID}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send disco#info");
    let frame = client
        .recv_matching(|frame| frame.contains("disco-cal") && frame.contains("<feature"))
        .await
        .expect("disco#info response");
    assert!(
        frame.contains(&format!("var='{NS_XCAL}'")) || frame.contains(&format!("var='{NS_XCAL}'")),
        "community disco#info missing xCal namespace: {frame}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn xcal_recurring_event_publish_and_items_round_trip() {
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
                        <description>Weekly gaming</description>
                        <location>Voice #gaming</location>
                        <organizer>xmpp:admin@localhost</organizer>
                        <rrule>
                          <freq>WEEKLY</freq>
                          <interval>1</interval>
                          <byday><weekday>FR</weekday></byday>
                          <count>10</count>
                        </rrule>
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
        "publish must succeed against the bootstrapped events node: {publish_result}"
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
        "items query missing published id: {items_response}"
    );
    assert!(
        items_response.contains("Friday Game Night"),
        "items query lost summary: {items_response}"
    );
    assert!(
        items_response.contains("WEEKLY"),
        "items query lost RRULE FREQ: {items_response}"
    );
    assert!(
        items_response.contains("<weekday"),
        "items query lost BYDAY weekday element: {items_response}"
    );
    assert!(
        items_response.contains("FR"),
        "items query lost FR weekday: {items_response}"
    );
    assert!(
        items_response.contains("<count"),
        "items query lost RRULE COUNT element: {items_response}"
    );

    let _ = client.close().await;
}
