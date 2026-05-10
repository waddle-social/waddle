//! XEP-0084 / XEP-0292 / XEP-0398 wire-conformance tests for the
//! OIDC profile/avatar publish chain.
//!
//! Tests drive the chain through the test-only HTTP endpoint
//! `POST /api/test/profile-publish` (gated on
//! `WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true`, which the harness sets).

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
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
        .header("X-Waddle-Test-Token", server.test_profile_publish_token())
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

/// Variant of [`invoke_profile_publish`] that returns `(status, body)`
/// without panicking on non-2xx, so failure-path tests can assert
/// what the test seam does on bad input.
async fn invoke_profile_publish_raw(
    server: &TestServer,
    req: &PublishReq,
    token: Option<&str>,
) -> (reqwest::StatusCode, String) {
    let url = format!("{}/api/test/profile-publish", server.http_base_url());
    let client = reqwest::Client::new();
    let mut builder = client.post(&url).json(req);
    if let Some(t) = token {
        builder = builder.header("X-Waddle-Test-Token", t);
    }
    let resp = builder
        .send()
        .await
        .expect("POST /api/test/profile-publish");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

/// Build a 1×1 PNG with the proper signature so the magic-byte
/// check in `fetch.rs` accepts it.
fn tiny_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89,
    ]
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

    let png_bytes = tiny_png();
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

    // wiremock serves over plain HTTP on 127.0.0.1. The test seam's
    // FetchPolicy sets `block_non_global_ips=false` and
    // `allow_http_for_tests=true` so the loopback fixture is
    // reachable; production callers (the OIDC bridge) build a
    // `FetchPolicy::default()` instead.
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

    let png = tiny_png();
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
        vcard4.contains("xmpp:")
            && vcard4.contains("?pubsub")
            && vcard4.contains(&format!("node={NS_AVATAR_DATA}")),
        "vCard4 photo MUST reference the PEP avatar-data item, not embed bytes inline: {vcard4}"
    );
    assert!(
        !vcard4.contains("data:image/png;base64,"),
        "vCard4 MUST NOT inline base64 bytes (XEP-0163 fan-out bloat): {vcard4}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// Test 5 — non_png_content_type_is_rejected
// ============================================================================
//
// XEP-0084 §3.1 limits `<data/>` to `image/png`. The fetcher MUST
// reject any other Content-Type before publishing.

#[tokio::test]
async fn non_png_content_type_is_rejected() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(b"\xff\xd8\xff\xe0".to_vec()),
        )
        .mount(&mock)
        .await;

    let (status, body) = invoke_profile_publish_raw(
        &server,
        &PublishReq {
            jid: admin_bare,
            display_name: None,
            avatar_url: Some(format!("{}/jpeg.bin", mock.uri())),
        },
        Some(server.test_profile_publish_token()),
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "non-PNG MIME is a fetch-policy reject (422), got {status} {body}"
    );
}

// ============================================================================
// Test 6 — png_content_type_with_non_png_bytes_is_rejected
// ============================================================================
//
// A hostile origin can return `Content-Type: image/png` while
// serving HTML/SVG/JS bytes. The fetcher's magic-byte sniff MUST
// catch the lie before the bytes land in `urn:xmpp:avatar:data`.

#[tokio::test]
async fn png_content_type_with_non_png_bytes_is_rejected() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                // JPEG SOI + JFIF — definitely not a PNG signature.
                .set_body_bytes(
                    b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
                        .to_vec(),
                ),
        )
        .mount(&mock)
        .await;

    let (status, body) = invoke_profile_publish_raw(
        &server,
        &PublishReq {
            jid: admin_bare,
            display_name: None,
            avatar_url: Some(format!("{}/lying.png", mock.uri())),
        },
        Some(server.test_profile_publish_token()),
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "magic-byte mismatch is a fetch-policy reject (422), got {status} {body}"
    );
}

// ============================================================================
// Test 7 — same_avatar_published_twice_is_idempotent
// ============================================================================
//
// XEP-0060 / XEP-0084 §4.1.1: avatar items are keyed on SHA-1 of
// bytes. Re-publishing the same bytes results in the same item id
// and `max_items=1` evicts no rows that weren't already replaced —
// the wire-observable end state is identical.

#[tokio::test]
async fn same_avatar_published_twice_is_idempotent() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-idem-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let png = tiny_png();
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(png.clone()),
        )
        .mount(&mock)
        .await;
    let url = format!("{}/idem.png", mock.uri());

    let first = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: None,
            avatar_url: Some(url.clone()),
        },
    )
    .await;
    let second = invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: None,
            avatar_url: Some(url),
        },
    )
    .await;
    assert_eq!(
        first.photo_sha1_hex, second.photo_sha1_hex,
        "identical bytes MUST produce identical SHA-1 ids: {first:?} vs {second:?}"
    );

    // Metadata still has exactly one item, and it carries that id.
    let sha1 = first.photo_sha1_hex.expect("first publish has hash");
    let metadata = iq_get_to(
        &mut admin,
        "avatar-meta-idem",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_METADATA}"/></pubsub>"#),
    )
    .await;
    assert!(
        metadata.contains(&format!(r#"id="{sha1}""#)),
        "after idempotent re-publish the metadata id MUST be unchanged: {metadata}"
    );
    let _ = admin.close().await;
}

// ============================================================================
// Test 8 — fn_only_after_combined_preserves_photo
// ============================================================================
//
// XEP-0398 RMW invariant: a subsequent FN-only publish MUST NOT
// drop the existing PHOTO from vcard-temp or vCard4.

#[tokio::test]
async fn fn_only_after_combined_preserves_photo() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-preserve-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let png = tiny_png();
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(png.clone()),
        )
        .mount(&mock)
        .await;

    invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: Some("First Name".into()),
            avatar_url: Some(format!("{}/seed.png", mock.uri())),
        },
    )
    .await;
    invoke_profile_publish(
        &server,
        &PublishReq {
            jid: admin_bare.clone(),
            display_name: Some("Renamed".into()),
            avatar_url: None,
        },
    )
    .await;

    let vcard_temp = iq_get_to(
        &mut admin,
        "vcard-temp-preserve-1",
        &admin_bare,
        r#"<vCard xmlns="vcard-temp"/>"#,
    )
    .await;
    assert!(
        vcard_temp.contains("Renamed") && !vcard_temp.contains("First Name"),
        "FN must be replaced: {vcard_temp}"
    );
    assert!(
        vcard_temp.contains("<PHOTO") && vcard_temp.contains("<BINVAL"),
        "PHOTO must be preserved on FN-only follow-up: {vcard_temp}"
    );

    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-preserve-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(
        vcard4.contains("Renamed")
            && vcard4.contains("<photo")
            && vcard4.contains("?pubsub")
            && vcard4.contains(&format!("node={NS_AVATAR_DATA}")),
        "vCard4 must preserve the photo PEP-item URI on FN-only follow-up: {vcard4}"
    );
    let _ = admin.close().await;
}

// ============================================================================
// Test 9 — test_seam_rejects_missing_token
// ============================================================================
//
// Defense-in-depth: the test seam MUST refuse to publish without
// the `X-Waddle-Test-Token` header, even when the env-flag is on.

#[tokio::test]
async fn test_seam_rejects_missing_token() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let (status, _) = invoke_profile_publish_raw(
        &server,
        &PublishReq {
            jid: admin_bare,
            display_name: Some("X".into()),
            avatar_url: None,
        },
        None,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "missing token MUST yield 401, got {status}"
    );
}

// ============================================================================
// Test 10 — test_seam_rejects_jid_outside_allowlist
// ============================================================================
//
// Defense-in-depth: even with a valid token, the seam MUST refuse
// any JID that isn't a configured fixed-account JID.

#[tokio::test]
async fn test_seam_rejects_jid_outside_allowlist() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();

    let (status, _) = invoke_profile_publish_raw(
        &server,
        &PublishReq {
            jid: format!("ghost@{DOMAIN}"),
            display_name: Some("X".into()),
            avatar_url: None,
        },
        Some(server.test_profile_publish_token()),
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::FORBIDDEN,
        "non-allowlisted JID MUST yield 403, got {status}"
    );
}
