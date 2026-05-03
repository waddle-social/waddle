//! XEP-0313 MAM integration tests over WebSocket.
//!
//! Each test starts its own isolated waddle-server on dynamic ports.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{disco_info_query, TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

/// Start an isolated server and connect an authenticated client.
async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("test-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, &resource)
            .await
            .expect("Failed to connect and authenticate");
    (server, client)
}

fn mam_query_xml(id: &str, archive_jid: &str, max: Option<u32>) -> String {
    let rsm = max
        .map(|m| format!(r#"<set xmlns="http://jabber.org/protocol/rsm"><max>{m}</max></set>"#))
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

fn mam_query_ids_xml(id: &str, archive_jid: &str, ids: &[&str]) -> String {
    let id_values = ids
        .iter()
        .map(|archive_id| format!(r#"<value>{archive_id}</value>"#))
        .collect::<String>();
    format!(
        r#"<iq type="set" id="{id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>urn:xmpp:mam:2</value></field><field var="ids">{id_values}</field></x></query></iq>"#
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

fn extract_result_id(frame: &str) -> Option<String> {
    let result = &frame[frame.find("<result")?..];
    if let Some(start) = result.find(" id=\"") {
        let value = &result[start + 5..];
        return Some(value[..value.find('"')?].to_string());
    }
    if let Some(start) = result.find(" id='") {
        let value = &result[start + 5..];
        return Some(value[..value.find('\'')?].to_string());
    }
    None
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
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
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
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
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
    let page2_bodies: Vec<String> = page2.iter().filter_map(|f| extract_mam_body(f)).collect();
    assert_eq!(
        page2_bodies,
        vec!["page msg 3".to_string(), "page msg 4".to_string()],
        "Page 2 should contain the next two messages"
    );
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
    let page3_bodies: Vec<String> = page3.iter().filter_map(|f| extract_mam_body(f)).collect();
    assert_eq!(
        page3_bodies,
        vec!["page msg 5".to_string()],
        "Page 3 should contain only the final message"
    );
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
    assert_eq!(
        all_bodies,
        vec![
            "page msg 1".to_string(),
            "page msg 2".to_string(),
            "page msg 3".to_string(),
            "page msg 4".to_string(),
            "page msg 5".to_string()
        ]
    );

    client.close().await;
}

#[tokio::test]
async fn personal_archive_disco_advertises_extended_mam_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let archive_jid = format!("{USERNAME}@{DOMAIN}");

    let response = disco_info_query(&mut client, &archive_jid, "mam-disco-personal")
        .await
        .expect("personal archive disco#info response");

    assert!(
        response.contains("urn:xmpp:mam:2#extended"),
        "personal archive disco missing urn:xmpp:mam:2#extended: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn personal_archive_mam_form_includes_extended_fields() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let archive_jid = format!("{USERNAME}@{DOMAIN}");

    client
        .send(&format!(
            r#"<iq type="get" id="mam-form-personal" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send mam form request");
    let response = client
        .recv_matching(|frame| frame.contains("mam-form-personal"))
        .await
        .expect("mam form response");

    for field in ["before-id", "after-id", "ids"] {
        assert!(
            response.contains(&format!("var=\"{field}\""))
                || response.contains(&format!("var='{field}'")),
            "mam form missing {field}: {response}"
        );
    }
    assert!(
        response.contains("http://jabber.org/protocol/xdata-validate"),
        "mam form missing XEP-0122 validation namespace: {response}"
    );
    assert!(
        response.contains("datatype=\"xs:string\"") || response.contains("datatype='xs:string'"),
        "mam form missing ids datatype: {response}"
    );
    assert!(response.contains("<open") || response.contains("<open/>"));

    client.close().await;
}

#[tokio::test]
async fn room_disco_advertises_extended_mam_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-disco-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    let response = disco_info_query(&mut client, &room, "mam-disco-room")
        .await
        .expect("room disco#info response");

    assert!(
        response.contains("urn:xmpp:mam:2#extended"),
        "room disco missing urn:xmpp:mam:2#extended: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn room_mam_ids_query_returns_only_requested_messages() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let room = format!("mam-ids-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    let bodies = ["ids one", "ids two", "ids three"];
    for body in &bodies {
        client
            .send(&format!(
                r#"<message type="groupchat" to="{room}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send message");
        client
            .recv_matching(|frame| frame.contains(body))
            .await
            .expect("echo");
    }

    let initial = query_mam(
        &mut client,
        &mam_query_xml(
            &format!("ids-seed-{}", uuid::Uuid::new_v4()),
            &room,
            Some(10),
        ),
    )
    .await
    .expect("seed mam query");
    let result_frames: Vec<&str> = initial
        .iter()
        .map(|frame| frame.as_str())
        .filter(|frame| frame.contains("<forwarded"))
        .collect();
    let selected_ids = [
        extract_result_id(result_frames[2]).expect("third archive id"),
        extract_result_id(result_frames[0]).expect("first archive id"),
    ];

    let filtered = query_mam(
        &mut client,
        &mam_query_ids_xml(
            &format!("ids-filter-{}", uuid::Uuid::new_v4()),
            &room,
            &[selected_ids[0].as_str(), selected_ids[1].as_str()],
        ),
    )
    .await
    .expect("ids mam query");
    let filtered_bodies: Vec<String> = filtered
        .iter()
        .filter_map(|frame| extract_mam_body(frame))
        .collect();

    assert_eq!(
        filtered_bodies,
        vec!["ids one".to_string(), "ids three".to_string()]
    );

    client.close().await;
}

// =========================================================================
// DM MAM
// =========================================================================

#[tokio::test]
async fn dm_archived_in_sender_personal_archive() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
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
    // Verify the body appears in a MAM result frame (not just any echo)
    assert!(
        frames.iter().any(|f| {
            f.contains(r#"<result xmlns="urn:xmpp:mam:2""#)
                || f.contains(r#"<result xmlns='urn:xmpp:mam:2'"#)
        } && f.contains("<forwarded")
            && f.contains(&unique_body)),
        "DM body not found in MAM result frames"
    );

    client.close().await;
}
