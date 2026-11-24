//! XEP-0377 (Blocking Command Reports) over the active WebSocket transport.
//!
//! A XEP-0191 `<block/>` whose `<item/>` carries a `<report/>` blocks the JID
//! exactly as it would without the report (§5); a malformed report is a
//! `bad-request` and blocks nothing; the server advertises
//! `urn:xmpp:reporting:1` because it handles reports.

use waddle_ws_test_support as ws_common;

use ws_common::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const NS_REPORTING: &str = "urn:xmpp:reporting:1";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("report-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

async fn request(client: &mut WsXmppClient, id: &str, xml: &str) -> Element {
    client.send(xml).await.expect("send iq");
    let frame = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("iq response");
    frame.parse().expect("response is well-formed XML")
}

async fn blocklist(client: &mut WsXmppClient, id: &str) -> Vec<String> {
    let reply = request(
        client,
        id,
        &format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><blocklist xmlns="urn:xmpp:blocking"/></iq>"#
        ),
    )
    .await;
    assert_eq!(reply.attr("type"), Some("result"));
    reply
        .children()
        .flat_map(|blocklist| blocklist.children())
        .filter_map(|item| item.attr("jid").map(str::to_owned))
        .collect()
}

fn assert_bad_request(reply: &Element) {
    assert_eq!(reply.attr("type"), Some("error"), "{reply:?}");
    let error = reply
        .children()
        .find(|child| child.name() == "error")
        .expect("error child");
    assert!(
        error.children().any(|c| c.name() == "bad-request"),
        "expected bad-request, got {error:?}"
    );
}

#[tokio::test]
async fn server_advertises_reporting_feature() {
    let (_server, mut client) = setup().await;
    let response = disco_info_query(&mut client, DOMAIN, "disco-reporting")
        .await
        .expect("disco#info response");
    assert!(
        response.contains(NS_REPORTING),
        "disco#info must advertise {NS_REPORTING}: {response}"
    );
    let _ = client.close().await;
}

#[tokio::test]
async fn block_with_spam_report_blocks_the_jid() {
    let (_server, mut client) = setup().await;
    let reply = request(
        &mut client,
        "br-spam",
        r#"<iq xmlns="jabber:client" type="set" id="br-spam"><block xmlns="urn:xmpp:blocking"><item jid="spammer@localhost"><report xmlns="urn:xmpp:reporting:1" reason="urn:xmpp:reporting:spam"><text xml:lang="en">Never came trouble to my house like this.</text><stanza-id xmlns="urn:xmpp:sid:0" by="spammer@localhost" id="28482-98726-73623"/><report-origin/><third-party/></report></item></block></iq>"#,
    )
    .await;
    assert_eq!(reply.attr("type"), Some("result"), "{reply:?}");
    assert_eq!(
        blocklist(&mut client, "br-spam-list").await,
        vec!["spammer@localhost".to_owned()]
    );
    let _ = client.close().await;
}

#[tokio::test]
async fn block_with_abuse_report_and_unregistered_reason_still_blocks() {
    let (_server, mut client) = setup().await;
    let reply = request(
        &mut client,
        "br-abuse",
        r#"<iq xmlns="jabber:client" type="set" id="br-abuse"><block xmlns="urn:xmpp:blocking"><item jid="troll@localhost"><report xmlns="urn:xmpp:reporting:1" reason="urn:xmpp:reporting:abuse"/></item><item jid="other@localhost"><report xmlns="urn:xmpp:reporting:1" reason="urn:example:custom-reason"/></item><item jid="plain@localhost"/></block></iq>"#,
    )
    .await;
    assert_eq!(reply.attr("type"), Some("result"), "{reply:?}");
    let mut blocked = blocklist(&mut client, "br-abuse-list").await;
    blocked.sort();
    assert_eq!(
        blocked,
        ["other@localhost", "plain@localhost", "troll@localhost"]
    );
    let _ = client.close().await;
}

#[tokio::test]
async fn report_without_reason_is_bad_request_and_blocks_nothing() {
    let (_server, mut client) = setup().await;
    let reply = request(
        &mut client,
        "br-noreason",
        r#"<iq xmlns="jabber:client" type="set" id="br-noreason"><block xmlns="urn:xmpp:blocking"><item jid="x@localhost"><report xmlns="urn:xmpp:reporting:1"/></item></block></iq>"#,
    )
    .await;
    assert_bad_request(&reply);
    assert!(blocklist(&mut client, "br-noreason-list").await.is_empty());
    let _ = client.close().await;
}

#[tokio::test]
async fn item_with_two_reports_is_bad_request() {
    let (_server, mut client) = setup().await;
    let reply = request(
        &mut client,
        "br-two",
        r#"<iq xmlns="jabber:client" type="set" id="br-two"><block xmlns="urn:xmpp:blocking"><item jid="x@localhost"><report xmlns="urn:xmpp:reporting:1" reason="urn:xmpp:reporting:spam"/><report xmlns="urn:xmpp:reporting:1" reason="urn:xmpp:reporting:abuse"/></item></block></iq>"#,
    )
    .await;
    assert_bad_request(&reply);
    assert!(blocklist(&mut client, "br-two-list").await.is_empty());
    let _ = client.close().await;
}

#[tokio::test]
async fn report_inside_unblock_is_bad_request() {
    let (_server, mut client) = setup().await;
    let reply = request(
        &mut client,
        "br-unblock",
        r#"<iq xmlns="jabber:client" type="set" id="br-unblock"><unblock xmlns="urn:xmpp:blocking"><item jid="x@localhost"><report xmlns="urn:xmpp:reporting:1" reason="urn:xmpp:reporting:spam"/></item></unblock></iq>"#,
    )
    .await;
    assert_bad_request(&reply);
    let _ = client.close().await;
}
