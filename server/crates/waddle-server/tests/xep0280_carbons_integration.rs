//! XEP-0280 Message Carbons integration over the WebSocket transport.
//!
//! This covers the app-facing `waddle-server` WebSocket path, which is
//! the only supported XMPP C2S transport.

mod ws_common;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";

async fn enable_carbons(client: &mut WsXmppClient, id: &str) -> Result<(), String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#
        ))
        .await?;
    let _ = client.recv_matching(|frame| frame.contains(id)).await?;
    Ok(())
}

async fn enable_resumption(client: &mut WsXmppClient) -> Result<String, String> {
    client
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await?;
    let enabled = client
        .recv_matching(|frame| frame.contains("<enabled"))
        .await?;
    attr_value(&enabled, "id").ok_or_else(|| format!("enabled missing id: {enabled}"))
}

fn attr_value(frame: &str, attr: &str) -> Option<String> {
    let double = format!("{attr}=\"");
    if let Some(start) = frame.find(&double).map(|start| start + double.len()) {
        let end = frame[start..].find('"')?;
        return Some(frame[start..start + end].to_string());
    }
    let single = format!("{attr}='");
    let start = frame.find(&single).map(|start| start + single.len())?;
    let end = frame[start..].find('\'')?;
    Some(frame[start..start + end].to_string())
}

#[tokio::test]
async fn sent_carbon_delivered_to_opted_in_sibling_over_websocket() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();

    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("desktop-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("desktop connection");
    enable_carbons(&mut desktop, "carbons-enable-desktop")
        .await
        .expect("enable carbons");

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");

    phone
        .send(
            r#"<message xmlns="jabber:client" to="ghost@localhost" type="chat" id="ws-carbon-1"><body>websocket sent carbon proof</body></message>"#,
        )
        .await
        .expect("send dm");

    let carbon = desktop
        .recv_matching(|frame| {
            frame.contains("urn:xmpp:carbons:2")
                && frame.contains("<sent")
                && frame.contains("websocket sent carbon proof")
        })
        .await
        .expect("desktop receives sent carbon");

    assert!(
        carbon.contains("urn:xmpp:carbons:2"),
        "expected carbon namespace in frame: {carbon}"
    );

    let _ = phone.close().await;
    let _ = desktop.close().await;
}

#[tokio::test]
async fn sent_carbon_replays_to_detached_resumable_sibling() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let desktop_resource = format!("desktop-detached-{}", uuid::Uuid::new_v4());

    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &desktop_resource,
    )
    .await
    .expect("desktop connection");
    enable_carbons(&mut desktop, "carbons-enable-detached")
        .await
        .expect("enable carbons");
    let stream_id = enable_resumption(&mut desktop)
        .await
        .expect("enable resumption");
    drop(desktop);

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");
    let mut resumed = None;
    let mut replay = None;
    for attempt in 0..20 {
        let body = format!("detached carbon proof {attempt}");
        phone
            .send(&format!(
                r#"<message xmlns="jabber:client" to="ghost@localhost" type="chat" id="ws-carbon-detached-{attempt}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send dm");

        let mut candidate = WsXmppClient::connect(&server.ws_url())
            .await
            .expect("resume connection");
        candidate
            .authenticate(DOMAIN, USERNAME, &password)
            .await
            .expect("authenticate resume connection");
        candidate
            .send(&format!(
                r#"<resume xmlns="urn:xmpp:sm:3" previd="{stream_id}" h="0"/>"#
            ))
            .await
            .expect("send resume");
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            candidate.recv_matching(|frame| frame.contains(&body)),
        )
        .await
        {
            Ok(Ok(frame)) => {
                replay = Some(frame);
                resumed = Some(candidate);
                break;
            }
            _ => {
                drop(candidate);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    let replay = replay.expect("detached carbon replay");
    assert!(
        replay.contains("urn:xmpp:carbons:2") && replay.contains("<sent"),
        "expected sent carbon replay: {replay}"
    );

    let _ = phone.close().await;
    if let Some(resumed) = resumed {
        let _ = resumed.close().await;
    }
}
