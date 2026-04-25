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

    phone.close().await;
    desktop.close().await;
}
