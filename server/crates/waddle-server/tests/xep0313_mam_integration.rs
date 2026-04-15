//! XEP-0313 MAM integration tests over WebSocket.
//!
//! A single waddle-server instance is shared across all tests. Each test
//! uses unique room names to avoid interference.

mod ws_common;

use std::sync::LazyLock;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const PASSWORD: &str = "admin";

/// One server per XEP test file. Each test uses unique room names and
/// message bodies to avoid interference.
static SERVER: LazyLock<TestServer> = LazyLock::new(TestServer::start);

async fn connect() -> WsXmppClient {
    let resource = format!("test-{}", uuid::Uuid::new_v4());
    WsXmppClient::connect_and_auth(&SERVER.ws_url(), DOMAIN, USERNAME, PASSWORD, &resource)
        .await
        .expect("Failed to connect and authenticate")
}

fn mam_query_xml(id: &str, archive_jid: &str, max: Option<u32>) -> String {
    let rsm = max
        .map(|m| {
            format!(
                r#"<set xmlns="http://jabber.org/protocol/rsm"><max>{m}</max></set>"#
            )
        })
        .unwrap_or_default();
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2">{rsm}</query></iq>"#
    )
}

fn mam_query_after_xml(id: &str, archive_jid: &str, max: u32, after: &str) -> String {
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"><set xmlns="http://jabber.org/protocol/rsm"><max>{max}</max><after>{after}</after></set></query></iq>"#
    )
}

async fn query_mam(client: &mut WsXmppClient, query_xml: &str) -> Result<Vec<String>, String> {
    client.send(query_xml).await?;
    client
        .recv_until(|frame| frame.contains("urn:xmpp:mam:2") && frame.contains("<fin"))
        .await
}

fn extract_mam_body(frame: &str) -> Option<String> {
    let start = frame.find("<body>")?;
    let end = frame.find("</body>")?;
    Some(frame[start + 6..end].to_string())
}

fn extract_fin_last(frame: &str) -> Option<String> {
    let start = frame.find("<last>")?;
    let end = frame.find("</last>")?;
    Some(frame[start + 6..end].to_string())
}

// =========================================================================
// MUC MAM
// =========================================================================

#[tokio::test]
async fn muc_message_archived_and_queryable() {
    let mut client = connect().await;
    let room = format!("mam-test-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Join
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    // Send 3 messages, wait for the specific echo after each
    let bodies = ["Hello MAM one", "Hello MAM two", "Hello MAM three"];
    for body in &bodies {
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send message");
        let expected = body.to_string();
        client
            .recv_matching(|f| f.contains(&expected))
            .await
            .expect("echo");
    }

    // Query MAM
    let q_id = format!("q-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut client, &mam_query_xml(&q_id, &room, Some(50)))
        .await
        .expect("MAM query failed");

    let fin = frames.last().expect("No frames from MAM");
    assert!(fin.contains("<fin"), "Last frame should be <fin>: {fin}");
    assert!(
        fin.contains("complete") && fin.contains("true"),
        "Expected complete='true': {fin}"
    );

    let result_frames: Vec<&str> = frames
        .iter()
        .map(|f| f.as_str())
        .filter(|f| f.contains("<forwarded"))
        .collect();
    assert_eq!(result_frames.len(), 3, "Expected 3 MAM results");

    let returned_bodies: Vec<String> = result_frames
        .iter()
        .filter_map(|f| extract_mam_body(f))
        .collect();
    for body in &bodies {
        assert!(
            returned_bodies.iter().any(|b| b == body),
            "Missing '{body}' in {returned_bodies:?}"
        );
    }

    client.close().await;
}

#[tokio::test]
async fn muc_mam_pagination() {
    let mut client = connect().await;
    let room = format!("mam-page-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Join
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("join responses");

    // Send 5 messages, wait for each specific echo
    for i in 1..=5 {
        let body = format!("page msg {i}");
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send");
        client
            .recv_matching(|f| f.contains(&body))
            .await
            .expect("echo");
    }

    // Page 1: max=2
    let q1 = format!("p1-{}", uuid::Uuid::new_v4());
    let page1 = query_mam(&mut client, &mam_query_xml(&q1, &room, Some(2)))
        .await
        .expect("page 1");
    let fin1 = page1.last().expect("no fin page 1");
    assert!(
        !(fin1.contains("complete") && fin1.contains("true")),
        "Page 1 should not be complete"
    );
    assert_eq!(
        page1.iter().filter(|f| f.contains("<forwarded")).count(),
        2,
        "Page 1 should have 2 results"
    );
    let last1 = extract_fin_last(fin1).expect("<last> in page 1 fin");

    // Page 2: max=2, after last1
    let q2 = format!("p2-{}", uuid::Uuid::new_v4());
    let page2 = query_mam(&mut client, &mam_query_after_xml(&q2, &room, 2, &last1))
        .await
        .expect("page 2");
    assert_eq!(
        page2.iter().filter(|f| f.contains("<forwarded")).count(),
        2,
        "Page 2 should have 2 results"
    );
    let last2 = extract_fin_last(page2.last().expect("fin page 2")).expect("<last> page 2");

    // Page 3: remaining 1
    let q3 = format!("p3-{}", uuid::Uuid::new_v4());
    let page3 = query_mam(&mut client, &mam_query_after_xml(&q3, &room, 2, &last2))
        .await
        .expect("page 3");
    assert_eq!(
        page3.iter().filter(|f| f.contains("<forwarded")).count(),
        1,
        "Page 3 should have 1 result"
    );
    let fin3 = page3.last().expect("fin page 3");
    assert!(
        fin3.contains("complete") && fin3.contains("true"),
        "Page 3 should be complete"
    );

    // All 5 bodies across pages
    let all_bodies: Vec<String> = page1
        .iter()
        .chain(page2.iter())
        .chain(page3.iter())
        .filter_map(|f| extract_mam_body(f))
        .collect();
    assert_eq!(all_bodies.len(), 5);

    client.close().await;
}

// =========================================================================
// DM MAM
// =========================================================================

#[tokio::test]
async fn dm_archived_in_sender_personal_archive() {
    let mut client = connect().await;
    let bare_jid = format!("{USERNAME}@{DOMAIN}");

    // Self-DM so it archives in personal archive
    let unique_body = format!("dm-test-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<message type="chat" to="{bare_jid}"><body>{unique_body}</body></message>"#
        ))
        .await
        .expect("send DM");

    // Consume any echo/carbon
    let _ = client
        .recv_timeout(std::time::Duration::from_millis(500))
        .await;

    // Query personal archive
    let q_id = format!("dm-{}", uuid::Uuid::new_v4());
    let frames = query_mam(&mut client, &mam_query_xml(&q_id, &bare_jid, Some(50)))
        .await
        .expect("DM MAM query failed");

    assert!(
        frames.last().expect("no fin").contains("<fin"),
        "Last frame should be <fin>"
    );
    assert!(
        frames.iter().any(|f| f.contains(&unique_body)),
        "DM body not found in personal archive"
    );

    client.close().await;
}
