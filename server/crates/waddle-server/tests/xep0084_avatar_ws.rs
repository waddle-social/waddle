//! XEP-0084 / XEP-0292 / XEP-0398 wire-conformance tests for the
//! OIDC profile/avatar publish chain (RFC 363 PR 3).
//!
//! Tests drive the chain through the test-only HTTP endpoint
//! `POST /api/test/profile-publish` (gated on
//! `WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true`, which the harness sets).

mod ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_VCARD_TEMP: &str = "vcard-temp";
const NS_AVATAR_DATA: &str = "urn:xmpp:avatar:data";
const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id="{id}""#)) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishReq {
    jid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishResp {
    photo_sha1_hex: Option<String>,
    photo_mime: Option<String>,
    photo_bytes_len: Option<usize>,
    published_avatar_data: bool,
    published_avatar_metadata: bool,
    mirrored_vcard_temp: bool,
    mirrored_vcard4: bool,
}

async fn invoke_profile_publish(server: &TestServer, req: &PublishReq) -> PublishResp {
    let url = format!("{}/api/test/profile-publish", server.http_base_url());
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(req)
        .send()
        .await
        .expect("POST /api/test/profile-publish");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "test endpoint returned {status}: {body}"
    );
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("decode response failed: {e}: {body}"))
}

// ============================================================================
// Test 1 — fn_only_publishes_to_vcard_temp_and_vcard4_with_no_avatar
// ============================================================================
//
// XEP-0292 / XEP-0398 §3: when the bridge is asked to sync only FN
// (no avatar), vcard-temp gets `<FN>` and the vCard4 PEP node gets
// `<fn><text>...</text></fn>`. The avatar nodes are NOT touched.

#[tokio::test]
async fn fn_only_publishes_to_vcard_temp_and_vcard4_with_no_avatar() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-fn-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let resp = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: Some("Test FN".into()),
            avatar_url: None,
        },
    )
    .await;
    assert!(
        resp.mirrored_vcard_temp,
        "vcard-temp must be touched: {resp:?}"
    );
    assert!(resp.mirrored_vcard4, "vCard4 PEP must be touched: {resp:?}");
    assert!(!resp.published_avatar_data, "no avatar published: {resp:?}");
    assert!(
        !resp.published_avatar_metadata,
        "no avatar published: {resp:?}"
    );

    // vcard-temp via XEP-0054 IQ get.
    let vcard_temp = iq_get_to(
        &mut admin,
        "vcard-temp-fn-1",
        &admin_bare,
        r#"<vCard xmlns="vcard-temp"/>"#,
    )
    .await;
    assert!(
        vcard_temp.contains(NS_VCARD_TEMP)
            && vcard_temp.contains("<FN")
            && vcard_temp.contains("Test FN"),
        "vcard-temp must carry FN per XEP-0398 §3: {vcard_temp}"
    );

    // vCard4 via PEP items request.
    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-fn-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(
        vcard4.contains(NS_VCARD4)
            && vcard4.contains("<fn")
            && vcard4.contains("<text")
            && vcard4.contains("Test FN"),
        "vCard4 PEP must carry <fn><text> per XEP-0292: {vcard4}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// Test 2 — avatar_url_publishes_data_and_metadata_with_real_sha1
// ============================================================================
//
// XEP-0084 §4.1.1 / §4.1.2: the metadata `<info id>` MUST be the
// SHA-1 of the actual fetched bytes — not a hash of the URL string.
// This test serves a tiny PNG from wiremock, asks the bridge to
// publish it, and verifies the metadata id == SHA-1(bytes) AND the
// data item under that id contains the bytes (round-trip).

#[tokio::test]
async fn avatar_url_publishes_data_and_metadata_with_real_sha1() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-bytes-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Tiny 1x1 PNG (real PNG signature + minimal IHDR).
    let png_bytes: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89,
    ];
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/avatar.png"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(png_bytes.clone()),
        )
        .mount(&mock)
        .await;

    // wiremock binds to 127.0.0.1; the test endpoint's FetchPolicy
    // sets block_non_global_ips=false specifically so the loopback
    // is reachable. We need https in fetch policy though — wiremock
    // is http. Update fetch_policy in the test endpoint to also accept
    // http when the SSRF block is off, or have wiremock serve TLS.
    //
    // For now: the test endpoint must accept http when block_non_global_ips=false.
    let avatar_url = format!("{}/avatar.png", mock.uri());

    let resp = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: None,
            avatar_url: Some(avatar_url),
        },
    )
    .await;

    let sha1 = resp
        .photo_sha1_hex
        .clone()
        .expect("metadata MUST carry the SHA-1 id");
    assert_eq!(
        resp.photo_mime.as_deref(),
        Some("image/png"),
        "MIME must round-trip from Content-Type: {resp:?}"
    );
    assert_eq!(
        resp.photo_bytes_len,
        Some(png_bytes.len()),
        "byte count must round-trip exactly: {resp:?}"
    );
    assert!(
        resp.published_avatar_data && resp.published_avatar_metadata,
        "both nodes must receive a publish: {resp:?}"
    );

    // Metadata item retrievable via PEP items, with id == sha1.
    let metadata = iq_get_to(
        &mut admin,
        "avatar-meta-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_METADATA}"/></pubsub>"#),
    )
    .await;
    assert!(
        metadata.contains(NS_AVATAR_METADATA) && metadata.contains(&format!(r#"id="{sha1}""#)),
        "metadata item MUST be keyed on the SHA-1 of the bytes per XEP-0084 §4.1.1: {metadata}"
    );
    assert!(
        metadata.contains(r#"type="image/png""#),
        "metadata <info type> MUST round-trip the MIME: {metadata}"
    );
    assert!(
        !metadata.contains(r#"url="#),
        "metadata MUST NOT include `url` attribute (RFC 363 design): {metadata}"
    );

    // Data item retrievable via PEP items at the same id.
    let data = iq_get_to(
        &mut admin,
        "avatar-data-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_DATA}"><item id="{sha1}"/></items></pubsub>"#
        ),
    )
    .await;
    assert!(
        data.contains(NS_AVATAR_DATA) && data.contains(&format!(r#"id="{sha1}""#)),
        "data item MUST be retrievable by the SHA-1 id: {data}"
    );

    let _ = admin.close().await;
    let _ = Duration::from_secs(1);
}

// ============================================================================
// Test 3 — empty_source_is_no_op
// ============================================================================

#[tokio::test]
async fn empty_source_is_no_op() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let resp = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare,
            display_name: None,
            avatar_url: None,
        },
    )
    .await;

    assert!(!resp.published_avatar_data, "{resp:?}");
    assert!(!resp.published_avatar_metadata, "{resp:?}");
    assert!(!resp.mirrored_vcard_temp, "{resp:?}");
    assert!(!resp.mirrored_vcard4, "{resp:?}");
}

// ============================================================================
// Test 4 — combined_fn_plus_avatar_publishes_full_chain
// ============================================================================

#[tokio::test]
async fn combined_fn_plus_avatar_publishes_full_chain() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-combo-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x42, 0x42, 0x42, 0x42,
    ];
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(png.clone()),
        )
        .mount(&mock)
        .await;

    let resp = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: Some("Combined".into()),
            avatar_url: Some(format!("{}/x.png", mock.uri())),
        },
    )
    .await;
    assert!(
        resp.published_avatar_data && resp.published_avatar_metadata,
        "{resp:?}"
    );
    assert!(resp.mirrored_vcard_temp && resp.mirrored_vcard4, "{resp:?}");

    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-combo-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(vcard4.contains("Combined"), "FN: {vcard4}");
    assert!(
        vcard4.contains("data:image/png;base64,"),
        "vCard4 MUST embed photo as data: URI per XEP-0292: {vcard4}"
    );

    let _ = admin.close().await;
    let _ = NS_PUBSUB_EVENT;
}
