//! XEP-0237: Roster Versioning over WebSocket C2S.

mod ws_common;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn connect_alice() -> (TestServer, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    (server, alice)
}

async fn roster_get(client: &mut WsXmppClient, id: &str, ver: Option<&str>) -> String {
    let ver_attr = ver
        .map(|ver| format!(r#" ver="{ver}""#))
        .unwrap_or_default();
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"{ver_attr}/></iq>"#
        ))
        .await
        .expect("send roster get");
    client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster get result")
}

fn roster_version(frame: &str) -> String {
    let marker = "ver=\"";
    let start = frame
        .find(marker)
        .unwrap_or_else(|| panic!("missing roster version: {frame}"))
        + marker.len();
    let end = frame[start..]
        .find('"')
        .unwrap_or_else(|| panic!("unterminated roster version: {frame}"));
    frame[start..start + end].to_string()
}

#[tokio::test]
async fn xep0237_matching_version_returns_empty_roster_result() {
    let (_server, mut alice) = connect_alice().await;

    let initial = roster_get(&mut alice, "xep237-initial", None).await;
    let version = roster_version(&initial);

    let unchanged = roster_get(&mut alice, "xep237-unchanged", Some(&version)).await;
    assert!(
        unchanged.contains("type=\"result\""),
        "expected empty roster result: {unchanged}"
    );
    assert!(
        !unchanged.contains("jabber:iq:roster") && !unchanged.contains("<item"),
        "matching ver must not include unchanged roster payload: {unchanged}"
    );

    alice.close().await;
}
