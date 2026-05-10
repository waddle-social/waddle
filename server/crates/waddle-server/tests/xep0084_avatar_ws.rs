//! XEP-0084 / XEP-0292 / XEP-0398 wire-conformance tests for the
//! OIDC profile/avatar publish chain.
//!
//! Tests drive the chain through the test-only HTTP endpoint
//! `POST /api/test/profile-publish` (gated on
//! `WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true`, which the harness sets).

mod ws_common;

use serde_json::json;
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

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id="{id}""#)) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishResp {
    photo_sha1_hex: Option<String>,
    photo_mime: Option<String>,
    photo_bytes_len: Option<usize>,
    published_avatar_data: bool,
    published_avatar_metadata: bool,
    published_avatar_removal: bool,
    mirrored_vcard_temp: bool,
    mirrored_vcard4: bool,
    removed_vcard_temp_photo: bool,
    removed_vcard_temp_fn: bool,
    removed_vcard4_photo: bool,
    removed_vcard4_fn: bool,
    photo_axis_guarded_by_user_managed: bool,
}

/// Builder for the test-seam JSON request. The on-the-wire shape
/// matches the typed `PhotoIntentDto` / `NameIntentDto` enums in
/// `profile_publish_route.rs`.
#[derive(Debug, Clone, Default)]
struct PublishReq {
    jid: String,
    photo: Option<serde_json::Value>,
    name: Option<serde_json::Value>,
}

impl PublishReq {
    fn for_jid(jid: impl Into<String>) -> Self {
        Self {
            jid: jid.into(),
            photo: None,
            name: None,
        }
    }
    fn set_photo_url(mut self, url: impl Into<String>) -> Self {
        self.photo = Some(json!({ "setFromUrl": url.into() }));
        self
    }
    fn remove_photo(mut self) -> Self {
        self.photo = Some(json!({ "removeIfOidcOwned": null }));
        self
    }
    fn set_name(mut self, s: impl Into<String>) -> Self {
        self.name = Some(json!({ "set": s.into() }));
        self
    }
    fn remove_name(mut self) -> Self {
        self.name = Some(json!({ "remove": null }));
        self
    }
    fn to_json(&self) -> serde_json::Value {
        let mut obj = json!({ "jid": self.jid });
        if let Some(ref p) = self.photo {
            obj["photo"] = p.clone();
        }
        if let Some(ref n) = self.name {
            obj["name"] = n.clone();
        }
        obj
    }
}

async fn invoke_profile_publish(server: &TestServer, req: &PublishReq) -> PublishResp {
    let url = format!("{}/api/test/profile-publish", server.http_base_url());
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("X-Waddle-Test-Token", server.test_profile_publish_token())
        .json(&req.to_json())
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
/// without panicking on non-2xx.
async fn invoke_profile_publish_raw(
    server: &TestServer,
    req: &PublishReq,
    token: Option<&str>,
) -> (reqwest::StatusCode, String) {
    let url = format!("{}/api/test/profile-publish", server.http_base_url());
    let client = reqwest::Client::new();
    let mut builder = client.post(&url).json(&req.to_json());
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
// fn_only_publishes_to_vcard_temp_and_vcard4_with_no_avatar
// ============================================================================

#[tokio::test]
async fn fn_only_publishes_to_vcard_temp_and_vcard4_with_no_avatar() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-fn-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let resp = invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare).set_name("Test FN"),
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
// avatar_url_publishes_data_and_metadata_with_real_sha1
// ============================================================================

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

    let avatar_url = format!("{}/avatar.png", mock.uri());
    let resp = invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare).set_photo_url(&avatar_url),
    )
    .await;

    let sha1 = resp
        .photo_sha1_hex
        .clone()
        .expect("metadata MUST carry the SHA-1 id");
    assert_eq!(resp.photo_mime.as_deref(), Some("image/png"));
    assert_eq!(resp.photo_bytes_len, Some(png_bytes.len()));
    assert!(resp.published_avatar_data && resp.published_avatar_metadata);

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
    assert!(metadata.contains(r#"type="image/png""#));
    assert!(!metadata.contains(r#"url="#));

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
// empty_source_is_no_op
// ============================================================================

#[tokio::test]
async fn empty_source_is_no_op() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let resp = invoke_profile_publish(&server, &PublishReq::for_jid(admin_bare)).await;
    assert!(!resp.published_avatar_data);
    assert!(!resp.published_avatar_metadata);
    assert!(!resp.published_avatar_removal);
    assert!(!resp.mirrored_vcard_temp);
    assert!(!resp.mirrored_vcard4);
}

// ============================================================================
// combined_fn_plus_avatar_publishes_full_chain
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
        &PublishReq::for_jid(&admin_bare)
            .set_name("Combined")
            .set_photo_url(format!("{}/x.png", mock.uri())),
    )
    .await;
    assert!(resp.published_avatar_data && resp.published_avatar_metadata);
    assert!(resp.mirrored_vcard_temp && resp.mirrored_vcard4);

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
    assert!(!vcard4.contains("data:image/png;base64,"));

    let _ = admin.close().await;
}

// ============================================================================
// non_png_content_type_is_rejected
// ============================================================================

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
        &PublishReq::for_jid(admin_bare).set_photo_url(format!("{}/jpeg.bin", mock.uri())),
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
// png_content_type_with_non_png_bytes_is_rejected
// ============================================================================

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
                .set_body_bytes(
                    b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
                        .to_vec(),
                ),
        )
        .mount(&mock)
        .await;

    let (status, body) = invoke_profile_publish_raw(
        &server,
        &PublishReq::for_jid(admin_bare).set_photo_url(format!("{}/lying.png", mock.uri())),
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
// same_avatar_published_twice_is_idempotent
// ============================================================================

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
        &PublishReq::for_jid(&admin_bare).set_photo_url(&url),
    )
    .await;
    let second = invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare).set_photo_url(&url),
    )
    .await;
    assert_eq!(
        first.photo_sha1_hex, second.photo_sha1_hex,
        "identical bytes MUST produce identical SHA-1 ids"
    );

    let sha1 = first.photo_sha1_hex.expect("first publish has hash");
    let metadata = iq_get_to(
        &mut admin,
        "avatar-meta-idem",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_METADATA}"/></pubsub>"#),
    )
    .await;
    assert!(metadata.contains(&format!(r#"id="{sha1}""#)));
    let _ = admin.close().await;
}

// ============================================================================
// fn_only_after_combined_preserves_photo
// ============================================================================

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
        &PublishReq::for_jid(&admin_bare)
            .set_name("First Name")
            .set_photo_url(format!("{}/seed.png", mock.uri())),
    )
    .await;
    invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare).set_name("Renamed"),
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
// test_seam_rejects_missing_token
// ============================================================================

#[tokio::test]
async fn test_seam_rejects_missing_token() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let (status, _) = invoke_profile_publish_raw(
        &server,
        &PublishReq::for_jid(admin_bare).set_name("X"),
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
}

// ============================================================================
// test_seam_rejects_jid_outside_allowlist
// ============================================================================

#[tokio::test]
async fn test_seam_rejects_jid_outside_allowlist() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();

    let (status, _) = invoke_profile_publish_raw(
        &server,
        &PublishReq::for_jid(format!("ghost@{DOMAIN}")).set_name("X"),
        Some(server.test_profile_publish_token()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
}

// ============================================================================
// REMOVAL FLOW TESTS (PR4)
// ============================================================================

// ============================================================================
// avatar_removal_publishes_empty_metadata_and_strips_vcards
// ============================================================================
//
// XEP-0084 §4.3: removal publishes an empty `<metadata/>` element at
// item id `current`. Mirror surfaces (vcard-temp PHOTO, vCard4
// `<photo>`) MUST also be stripped so legacy clients drop their
// cached avatar.

#[tokio::test]
async fn avatar_removal_publishes_empty_metadata_and_strips_vcards() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "avatar-rm-1").await;
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

    // Seed: set an avatar + name first so removal has prior state.
    invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare)
            .set_name("Alice")
            .set_photo_url(format!("{}/seed.png", mock.uri())),
    )
    .await;

    // First removal: empty `<metadata/>` published, vCard PHOTO stripped.
    let rm1 =
        invoke_profile_publish(&server, &PublishReq::for_jid(&admin_bare).remove_photo()).await;
    assert!(
        rm1.published_avatar_removal,
        "first removal must publish empty <metadata/>: {rm1:?}"
    );
    assert!(
        !rm1.photo_axis_guarded_by_user_managed,
        "no user self-publish so guard MUST NOT fire: {rm1:?}"
    );
    assert!(
        rm1.removed_vcard_temp_photo && rm1.removed_vcard4_photo,
        "PHOTO must be stripped from both vcard surfaces: {rm1:?}"
    );
    assert!(
        !rm1.removed_vcard_temp_fn && !rm1.removed_vcard4_fn,
        "FN must be untouched on photo-only removal: {rm1:?}"
    );

    let metadata = iq_get_to(
        &mut admin,
        "avatar-meta-rm-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_METADATA}"/></pubsub>"#),
    )
    .await;
    assert!(
        metadata.contains(r#"id="current""#)
            && (metadata.contains(r#"<metadata xmlns="urn:xmpp:avatar:metadata"/>"#)
                || metadata.contains(r#"<metadata xmlns='urn:xmpp:avatar:metadata'/>"#)),
        "removal metadata MUST be empty `<metadata xmlns='urn:xmpp:avatar:metadata'/>` at id='current' per XEP-0084 §4.3: {metadata}"
    );

    let vcard_temp = iq_get_to(
        &mut admin,
        "vcard-temp-rm-1",
        &admin_bare,
        r#"<vCard xmlns="vcard-temp"/>"#,
    )
    .await;
    assert!(
        !vcard_temp.contains("<PHOTO"),
        "vcard-temp <PHOTO> must be stripped on removal: {vcard_temp}"
    );

    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-rm-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(
        !vcard4.contains("<photo"),
        "vCard4 <photo> must be stripped on removal: {vcard4}"
    );

    // Second removal: idempotent — empty `<metadata/>` is already
    // published, so no new wire publish should fire.
    let rm2 =
        invoke_profile_publish(&server, &PublishReq::for_jid(&admin_bare).remove_photo()).await;
    assert!(
        !rm2.published_avatar_removal,
        "second removal MUST be idempotent (no extra publish): {rm2:?}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// fn_removal_strips_fn_from_both_vcard_surfaces
// ============================================================================

#[tokio::test]
async fn fn_removal_strips_fn_from_both_vcard_surfaces() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "fn-rm-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare).set_name("To Be Removed"),
    )
    .await;

    let rm = invoke_profile_publish(&server, &PublishReq::for_jid(&admin_bare).remove_name()).await;
    assert!(
        rm.removed_vcard_temp_fn && rm.removed_vcard4_fn,
        "FN must be stripped from both vcard surfaces: {rm:?}"
    );
    assert!(
        !rm.removed_vcard_temp_photo && !rm.removed_vcard4_photo,
        "PHOTO must be untouched on FN-only removal: {rm:?}"
    );

    let vcard_temp = iq_get_to(
        &mut admin,
        "vcard-temp-fnrm-1",
        &admin_bare,
        r#"<vCard xmlns="vcard-temp"/>"#,
    )
    .await;
    assert!(
        !vcard_temp.contains("<FN"),
        "vcard-temp <FN> must be stripped: {vcard_temp}"
    );
    assert!(
        !vcard_temp.contains("To Be Removed"),
        "removed FN value must not linger: {vcard_temp}"
    );

    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-fnrm-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(
        !vcard4.contains("<fn"),
        "vCard4 <fn> must be stripped: {vcard4}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// cross_axis_remove_photo_plus_set_fn_applies_both
// ============================================================================
//
// Cross-axis regression test: when one publish call asks to
// `RemoveIfOidcOwned` PHOTO AND `Set` FN simultaneously, both axes
// must take effect. A prior version of the chain dropped the new
// FN value when paired with photo-removal because the
// `match (photo_op, name_op)` had no `(RemovePublished, NameOp::Set)`
// arm. The current `mirror_*` helpers handle this by computing
// `after_set` from `(photo_set, name_set)` independently and then
// applying the strip pass on top.

#[tokio::test]
async fn cross_axis_remove_photo_plus_set_fn_applies_both() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "cross-axis-1").await;
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

    // Seed: PHOTO + initial FN.
    invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare)
            .set_name("Original FN")
            .set_photo_url(format!("{}/seed.png", mock.uri())),
    )
    .await;

    // Single publish: remove PHOTO + set new FN.
    let mixed = invoke_profile_publish(
        &server,
        &PublishReq::for_jid(&admin_bare)
            .remove_photo()
            .set_name("Renamed"),
    )
    .await;
    assert!(
        mixed.published_avatar_removal,
        "PHOTO removal must fire: {mixed:?}"
    );
    assert!(
        mixed.removed_vcard_temp_photo && mixed.removed_vcard4_photo,
        "PHOTO must be stripped from both vcard surfaces: {mixed:?}"
    );

    let vcard_temp = iq_get_to(
        &mut admin,
        "vcard-temp-cross-1",
        &admin_bare,
        r#"<vCard xmlns="vcard-temp"/>"#,
    )
    .await;
    assert!(
        !vcard_temp.contains("<PHOTO"),
        "PHOTO stripped: {vcard_temp}"
    );
    assert!(
        vcard_temp.contains("<FN") && vcard_temp.contains("Renamed"),
        "new FN MUST be applied alongside PHOTO removal: {vcard_temp}"
    );
    assert!(
        !vcard_temp.contains("Original FN"),
        "old FN MUST be replaced: {vcard_temp}"
    );

    let vcard4 = iq_get_to(
        &mut admin,
        "vcard4-cross-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="urn:xmpp:vcard4"/></pubsub>"#),
    )
    .await;
    assert!(
        !vcard4.contains("<photo") && vcard4.contains("Renamed"),
        "vCard4: photo stripped, new FN applied: {vcard4}"
    );

    let _ = admin.close().await;
}

// ============================================================================
// avatar_removal_no_op_when_no_prior_avatar
// ============================================================================
//
// Trigger discipline: if there's no prior avatar, RemoveIfOidcOwned
// MUST NOT publish an empty `<metadata/>`. First-OIDC-login users
// (who never had an avatar) shouldn't generate spurious removal
// events.

#[tokio::test]
async fn avatar_removal_no_op_when_no_prior_avatar() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let resp =
        invoke_profile_publish(&server, &PublishReq::for_jid(admin_bare).remove_photo()).await;
    assert!(
        !resp.published_avatar_removal,
        "no prior avatar => no removal publish: {resp:?}"
    );
    assert!(
        !resp.photo_axis_guarded_by_user_managed,
        "user-managed guard didn't fire — `Unknown` source falls through to the prior-item check: {resp:?}"
    );
}

// ============================================================================
// avatar_removal_fans_out_empty_metadata_event_to_subscriber
// ============================================================================
//
// XEP-0163 §3 / XEP-0060 §7.1: subscribers MUST receive a
// `<message><event>` notification when a PEP item is published —
// including the XEP-0084 §4.3 empty-`<metadata/>` removal shape.
// Without explicit fan-out (the load-bearing PR3 fix) the removal
// is silent. This is the regression test for that wiring.

const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

#[tokio::test]
async fn avatar_removal_fans_out_empty_metadata_event_to_subscriber() {
    use std::time::Duration;
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let admin = admin_client(&server, "avatar-fanout-admin").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");
    let bob_bare = format!("bob@{DOMAIN}");

    // Seed an avatar so the metadata node exists with `oidc_pep_node_config`
    // (Open access, max_items=1) — without this, bob's <subscribe/> would
    // be denied.
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
        &PublishReq::for_jid(&admin_bare).set_photo_url(format!("{}/seed.png", mock.uri())),
    )
    .await;

    // Bob subscribes to admin's avatar-metadata node via plain
    // XEP-0060 `<subscribe/>`. Open access lets any peer subscribe.
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "avatar-fanout-bob",
    )
    .await
    .expect("bob connect");
    let sub_resp = iq_set_to(
        &mut bob,
        "avatar-fanout-sub",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{NS_AVATAR_METADATA}" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        sub_resp.contains(r#"type="result""#),
        "subscribe must succeed (Open access): {sub_resp}"
    );

    // OIDC removal — empty `<metadata/>` published; fan-out MUST
    // deliver the event to bob.
    let rm =
        invoke_profile_publish(&server, &PublishReq::for_jid(&admin_bare).remove_photo()).await;
    assert!(rm.published_avatar_removal, "removal must publish: {rm:?}");

    // Bob receives the `<message><event>` carrying the empty
    // `<metadata/>` payload at item id `current`.
    let event = wait_for_event_message(&mut bob, NS_AVATAR_METADATA, Duration::from_secs(2))
        .await
        .expect("bob MUST receive the empty-metadata fan-out event");
    assert!(
        event.contains(r#"id="current""#) || event.contains(r#"id='current'"#),
        "fan-out item id must be `current` per §4.3: {event}"
    );
    assert!(
        event.contains(r#"<metadata xmlns="urn:xmpp:avatar:metadata"/>"#)
            || event.contains(r#"<metadata xmlns='urn:xmpp:avatar:metadata'/>"#),
        "fan-out payload MUST be the empty `<metadata/>` removal shape: {event}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

async fn wait_for_event_message(
    client: &mut WsXmppClient,
    node: &str,
    dur: std::time::Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node="{node}""#))
                        || frame.contains(&format!(r#"node='{node}'"#)))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}

// ============================================================================
// user_self_published_avatar_is_protected_from_oidc_removal
// ============================================================================
//
// The load-bearing claim of RFC 363's user-managed guard: when a
// user has self-published via wire XEP-0084, the bridge marks
// `users.avatar_source = 'user'`, and `RemoveIfOidcOwned`
// suppresses the empty-metadata publish on the next OIDC reconcile.

#[tokio::test]
async fn user_self_published_avatar_is_protected_from_oidc_removal() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "guard-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Step 1: simulate the user self-publishing their own avatar via
    // wire XEP-0084. The publish-time hook in pubsub_dispatch will
    // flip `users.avatar_source = 'user'`.
    let png = tiny_png();
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    let png_b64 = BASE64.encode(&png);
    let user_set = iq_set_to(
        &mut admin,
        "user-pub-data",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{NS_AVATAR_DATA}"><item id="user-self-1"><data xmlns="{NS_AVATAR_DATA}">{png_b64}</data></item></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        user_set.contains(r#"type="result""#),
        "user self-publish must succeed: {user_set}"
    );

    // Also publish the metadata so `avatar_metadata_present` returns
    // true (otherwise the guard never even has anything to evaluate
    // against).
    let user_meta = iq_set_to(
        &mut admin,
        "user-pub-meta",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{NS_AVATAR_METADATA}"><item id="user-self-1"><metadata xmlns="{NS_AVATAR_METADATA}"><info id="user-self-1" type="image/png" bytes="{}"/></metadata></item></publish></pubsub>"#,
            png.len()
        ),
    )
    .await;
    assert!(user_meta.contains(r#"type="result""#));

    // Step 2: OIDC reconcile asks to remove the avatar. Guard should
    // fire — `published_avatar_removal=false`,
    // `photo_axis_guarded_by_user_managed=true`.
    let resp =
        invoke_profile_publish(&server, &PublishReq::for_jid(&admin_bare).remove_photo()).await;
    assert!(
        !resp.published_avatar_removal,
        "user-managed guard MUST suppress the empty-metadata publish: {resp:?}"
    );
    assert!(
        resp.photo_axis_guarded_by_user_managed,
        "guard flag MUST be set so telemetry can observe the suppression: {resp:?}"
    );

    // The user's metadata item is still there.
    let metadata = iq_get_to(
        &mut admin,
        "guard-meta-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{NS_AVATAR_METADATA}"/></pubsub>"#),
    )
    .await;
    assert!(
        metadata.contains(r#"id="user-self-1""#),
        "user-published metadata item MUST survive the suppressed removal: {metadata}"
    );

    let _ = admin.close().await;
}
