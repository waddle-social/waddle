//! XEP-0045 voice → LiveKit media-grant enforcement (#1593/#1594)
//! against a live waddle-server with `LIVEKIT_*` envs set.
//!
//! Dedicated XEP-0045 suite for the voice-derived media-authorization
//! behavior: an occupant's §7.5 voice decides their SFU publish
//! grants, occupancy is the precondition for call participation, and
//! the `participant_joined` webhook is the enforcement point for
//! tokens minted before a voice change. The SFU side is observed on a
//! mock LiveKit admin API (`LIVEKIT_WS_URL` pointed at a local
//! wiremock; the server derives the admin REST base from it).
//!
//! The cross-replica half of #1594 — the same enforcement relayed to
//! the room's claim owner — needs a two-node cluster and lives in
//! `clustering_cluster_e2e.rs`
//! (`participant_joined_webhook_reasserts_grants_on_foreign_room_owner`);
//! this suite pins the single-node XEP-0045 semantics both paths
//! share.

use waddle_ws_test_support as ws_common;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const NS_MUC: &str = "http://jabber.org/protocol/muc";
const DOMAIN: &str = "localhost";
const ALICE: &str = "alice";
const ALICE_PW: &str = "alice-pw-12345";
const LIVEKIT_WEBHOOK_SECRET: &str = "test-webhook-secret-with-at-least-32-bytes";

/// LiveKit env set whose admin REST origin is the given mock server.
fn livekit_envs(ws_url: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("LIVEKIT_API_KEY", "APItestkeyxep0045"),
        (
            "LIVEKIT_API_SECRET",
            "test-secret-with-at-least-32-bytes-of-payload",
        ),
        ("LIVEKIT_WS_URL", ws_url),
        ("LIVEKIT_TURN_HOST", "turn.example.test"),
        (
            "LIVEKIT_TURN_SHARED_SECRET",
            "turn-shared-secret-value-also-long-enough",
        ),
        ("LIVEKIT_WEBHOOK_SECRET", LIVEKIT_WEBHOOK_SECRET),
        // Instant-room creation is owner-gated; the fixture account
        // must be allowed to create the room it joins.
        ("WADDLE_SERVER_OWNER_LOCALPARTS", ALICE),
    ]
}

async fn fake_livekit_admin() -> (wiremock::MockServer, &'static str) {
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&mock)
        .await;
    let ws_url: &'static str =
        Box::leak(mock.uri().replacen("http://", "ws://", 1).into_boxed_str());
    (mock, ws_url)
}

/// MUC join `<presence/>` built with the typed XML builder (repo
/// XML-generation rule: never construct XML with `format!`).
fn muc_join_presence(room: &str, nick: &str) -> Element {
    Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/{nick}"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build()
}

fn livekit_webhook_auth(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let claims = json!({
        "sha256": BASE64_STANDARD.encode(hasher.finalize()),
        "exp": (chrono::Utc::now() + chrono::Duration::seconds(60)).timestamp(),
        "iat": chrono::Utc::now().timestamp(),
    });
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(LIVEKIT_WEBHOOK_SECRET.as_bytes()),
    )
    .expect("sign LiveKit webhook");
    format!("Bearer {token}")
}

async fn post_participant_joined(server: &TestServer, room: &str, identity: &str) {
    let body = serde_json::to_vec(&json!({
        "id": format!("EV_{}", uuid::Uuid::new_v4()),
        "event": "participant_joined",
        "room": { "name": room },
        "participant": { "identity": identity },
    }))
    .expect("webhook body");
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/livekit/webhook", server.http_base_url()))
        .header("Authorization", livekit_webhook_auth(&body))
        .body(body)
        .send()
        .await
        .expect("post LiveKit webhook");
    assert!(
        response.status().is_success(),
        "participant_joined webhook must be acknowledged, got {}",
        response.status()
    );
}

/// Poll the mock admin API until a POST to `path` whose body contains
/// `needle` arrives, or fail with everything that was seen.
async fn await_admin_call(mock: &wiremock::MockServer, path: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let requests = mock.received_requests().await.unwrap_or_default();
        if requests.iter().any(|request| {
            request.url.path() == path && String::from_utf8_lossy(&request.body).contains(needle)
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected an admin call to {path} mentioning {needle}; saw: {:?}",
            requests
                .iter()
                .map(|request| request.url.path().to_string())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// §7.5-derived enforcement, grant half: a seated occupant with voice
/// who joins the SFU room gets their voice-derived grants pushed to
/// LiveKit when the `participant_joined` webhook is processed — the
/// stale-token enforcement point (a token minted before a voice
/// change carries the OLD permissions; the webhook re-derivation is
/// what corrects them).
#[tokio::test]
async fn occupant_join_webhook_pushes_voice_derived_grants() {
    let (mock, ws_url) = fake_livekit_admin().await;
    let server = TestServer::start_with_extra_envs(&[(ALICE, ALICE_PW)], &livekit_envs(ws_url));
    let mut alice = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ALICE, ALICE_PW, "ax")
        .await
        .expect("alice connects");

    let room = format!("voice-grants-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    alice
        .send(&String::from(&muc_join_presence(&room, "alice")))
        .await
        .expect("alice sends join presence");
    alice
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("alice's join completes");

    let identity = format!("{ALICE}@{DOMAIN}/ax");
    post_participant_joined(&server, &room, &identity).await;

    await_admin_call(
        &mock,
        "/twirp/livekit.RoomService/UpdateParticipant",
        &identity,
    )
    .await;
}

/// §7.5-derived enforcement, eviction half: occupancy is the
/// precondition for call participation, so a LiveKit participant the
/// room's authoritative occupant set does NOT contain is evicted from
/// the call rather than granted — a voluntary leave followed by a
/// LiveKit reconnect reaches no other teardown path.
#[tokio::test]
async fn non_occupant_join_webhook_evicts_from_the_call() {
    let (mock, ws_url) = fake_livekit_admin().await;
    let server = TestServer::start_with_extra_envs(&[(ALICE, ALICE_PW)], &livekit_envs(ws_url));
    let mut alice = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ALICE, ALICE_PW, "ax")
        .await
        .expect("alice connects");

    let room = format!("voice-evict-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    alice
        .send(&String::from(&muc_join_presence(&room, "alice")))
        .await
        .expect("alice sends join presence");
    alice
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("alice's join completes");

    // A full JID that is NOT an occupant of the room: the local actor
    // answers authoritatively, so this join must end in eviction.
    let ghost = format!("{ALICE}@{DOMAIN}/never-joined");
    post_participant_joined(&server, &room, &ghost).await;

    await_admin_call(
        &mock,
        "/twirp/livekit.RoomService/RemoveParticipant",
        &ghost,
    )
    .await;
    let requests = mock.received_requests().await.unwrap_or_default();
    assert!(
        !requests.iter().any(|request| {
            request.url.path() == "/twirp/livekit.RoomService/UpdateParticipant"
                && String::from_utf8_lossy(&request.body).contains(&ghost)
        }),
        "a non-occupant must never receive a grant push"
    );
}
