//! LiveKit webhook ingestion: typed event payloads and signature
//! verification.
//!
//! LiveKit's webhook signing scheme (per the canonical docs at
//! <https://docs.livekit.io/home/server/webhooks/>) is a JWT carried in
//! the `Authorization` header, signed with HS256 using the API secret.
//! The JWT's payload includes a `sha256` claim that is the
//! base64-encoded SHA-256 digest of the raw request body. To accept a
//! webhook, the receiver MUST:
//!
//! 1. Verify the JWT signature with HS256 against the shared secret.
//! 2. Recompute SHA-256 over the raw body bytes and base64-encode the
//!    digest.
//! 3. Constant-time compare the recomputed digest against the JWT's
//!    `sha256` claim. A signature that matches a body the receiver
//!    didn't see is a replay attempt and MUST be rejected.
//! 4. Reject events outside the JWT's lifetime window (`iat`/`exp`).
//!
//! This module exposes [`verify_webhook_signature`] which performs
//! steps 1–4 and returns the parsed [`LiveKitWebhookEvent`] on success.
//! Replay-by-event-id is the caller's responsibility (a small bounded
//! `seen_event_ids` LRU at the HTTP handler is the canonical place).

use base64::engine::general_purpose::STANDARD as Base64Standard;
use base64::Engine;
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::{config::ApiSecret, ParticipantSid, RoomSid};

/// Errors that arise while verifying a LiveKit webhook delivery.
///
/// Surfaces all of them with explicit variants so the HTTP handler
/// can map each to the right response code (400 for malformed,
/// 401 for signature/body mismatch, 408/410 for time-window failure)
/// without losing the why-it-failed signal in logs.
#[derive(Debug, Error)]
pub enum WebhookVerifyError {
    #[error("missing Authorization header on LiveKit webhook")]
    MissingAuthorization,

    #[error("malformed Authorization header")]
    MalformedAuthorization,

    #[error("LiveKit webhook JWT failed signature or claim validation")]
    Jwt(#[source] jsonwebtoken::errors::Error),

    #[error("LiveKit webhook JWT did not carry a sha256 body-hash claim")]
    MissingBodyHash,

    #[error("LiveKit webhook JWT body-hash claim did not match the request body")]
    BodyHashMismatch,

    #[error("LiveKit webhook body failed to parse as JSON event payload")]
    BodyJson(#[source] serde_json::Error),
}

/// JWT claims LiveKit signs with the API secret. Only the fields the
/// verifier actually consumes are typed; LiveKit may add others
/// (`exp`/`iat`/`nbf` are validated by `jsonwebtoken` and don't need
/// dedicated fields here).
#[derive(Debug, Deserialize)]
struct WebhookClaims {
    /// Base64-encoded SHA-256 of the request body. Mandatory per the
    /// LiveKit signing spec.
    sha256: Option<String>,
}

/// Verify a LiveKit webhook delivery and return the parsed event.
///
/// `auth_header` is the value of the `Authorization` request header.
/// `body` is the *raw* request body bytes — re-encoded JSON would not
/// match the hash claim. The handler MUST capture the body before
/// any framework deserialization that might re-stringify it.
pub fn verify_webhook_signature(
    secret: &ApiSecret,
    auth_header: Option<&str>,
    body: &[u8],
) -> Result<LiveKitWebhookEvent, WebhookVerifyError> {
    let raw = auth_header.ok_or(WebhookVerifyError::MissingAuthorization)?;
    let token = strip_bearer(raw).ok_or(WebhookVerifyError::MalformedAuthorization)?;

    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    // LiveKit's webhook JWT omits `aud` / `iss`, so those stay
    // unrequired — but `exp` must be present: `validate_exp` only
    // checks the claim when it exists, and a signed token without
    // `exp` would otherwise verify forever (unbounded replay window).
    validation.required_spec_claims = std::collections::HashSet::from(["exp".to_owned()]);
    validation.validate_exp = true;
    validation.validate_nbf = true;
    // `leeway` is in seconds; allow a small clock-skew window so a
    // few-second drift between LK and our host doesn't bounce
    // legitimate deliveries.
    validation.leeway = 30;

    let key = DecodingKey::from_secret(secret.as_bytes());
    let data =
        decode::<WebhookClaims>(token, &key, &validation).map_err(WebhookVerifyError::Jwt)?;
    let claimed = data
        .claims
        .sha256
        .ok_or(WebhookVerifyError::MissingBodyHash)?;

    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    let computed = Base64Standard.encode(digest);

    // Constant-time compare to avoid timing oracles on the body hash.
    // `subtle::ConstantTimeEq` returns false for mismatched lengths
    // without a length-leaking early exit, so the comparison is
    // genuinely fixed-time across both equal- and unequal-length
    // inputs — robust even if LK ever switches to a non-base64 digest
    // shape that changes the byte length.
    if claimed.as_bytes().ct_eq(computed.as_bytes()).unwrap_u8() == 0 {
        return Err(WebhookVerifyError::BodyHashMismatch);
    }

    let event: LiveKitWebhookEvent =
        serde_json::from_slice(body).map_err(WebhookVerifyError::BodyJson)?;
    Ok(event)
}

fn strip_bearer(header: &str) -> Option<&str> {
    let trimmed = header.trim();
    if let Some(rest) = trimmed.strip_prefix("Bearer ") {
        return Some(rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix("bearer ") {
        return Some(rest.trim());
    }
    // LiveKit's own SDKs send the bare JWT without a `Bearer ` prefix
    // in older versions; accept that form too.
    if !trimmed.is_empty() && !trimmed.contains(' ') {
        return Some(trimmed);
    }
    None
}

/// Typed projection of LiveKit's documented webhook payloads.
///
/// LiveKit sends an envelope with `event` discriminator plus
/// event-specific `room` / `participant` / `track` / `egress_info`
/// children. Only the variants Waddle actually reacts to carry typed
/// data; the rest are caught by [`LiveKitWebhookEvent::Other`] so an
/// unrecognised future event type does not fail signature-verified
/// deliveries.
///
/// All variants also expose the envelope-level `id` (per LK docs:
/// "a unique identifier for the event, useful for deduplication") so
/// the HTTP handler can keep a bounded `seen_event_ids` LRU and drop
/// the up-to-five-times retried duplicates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event")]
pub enum LiveKitWebhookEvent {
    /// A participant has connected and reached the `active` state.
    /// Used to confirm presence; Waddle's existing
    /// `register_call_participant` already runs on the Jingle initiate
    /// path, so this is informational.
    #[serde(rename = "participant_joined")]
    ParticipantJoined(ParticipantEnvelope),

    /// A participant has left the room and LiveKit has completed
    /// cleanup. Source of truth for "this participant is no longer
    /// in the call." Drives the chat-side Muji presence clear via the
    /// HTTP handler → MUC room actor path.
    #[serde(rename = "participant_left")]
    ParticipantLeft(ParticipantEnvelope),

    /// A participant's signal channel reached `active` but the media
    /// connection later failed. Treated identically to
    /// [`Self::ParticipantLeft`] for the purposes of presence cleanup.
    #[serde(rename = "participant_connection_aborted")]
    ParticipantConnectionAborted(ParticipantEnvelope),

    /// The room has been closed by LiveKit (last participant left
    /// past the empty-room timeout, or an explicit `room.close()`).
    /// Cleans the SFU registry for the whole call and clears any
    /// surviving Muji presence in the corresponding MUC.
    #[serde(rename = "room_finished")]
    RoomFinished(RoomEnvelope),

    /// Unrecognised or informational events (`room_started`,
    /// `track_published`, `egress_*`, …). Captured so the
    /// signature-verified payload still parses cleanly; the handler
    /// can log + drop.
    #[serde(other)]
    Other,
}

impl LiveKitWebhookEvent {
    /// Envelope-level unique event id, used for deduplication of the
    /// up-to-5 LiveKit retries per delivery. `None` for events we
    /// don't react to (their envelopes weren't typed).
    pub fn event_id(&self) -> Option<&str> {
        match self {
            Self::ParticipantJoined(env)
            | Self::ParticipantLeft(env)
            | Self::ParticipantConnectionAborted(env) => env.id.as_deref(),
            Self::RoomFinished(env) => env.id.as_deref(),
            Self::Other => None,
        }
    }
}

/// Common shape for events that name a specific (room, participant).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParticipantEnvelope {
    /// Envelope-level event id per LK's documented webhook shape.
    /// Optional only because LK has shipped payloads without it
    /// historically; the dedupe path treats absence as "not
    /// deduplicable" rather than failing.
    pub id: Option<String>,
    pub room: RoomInfo,
    pub participant: ParticipantInfo,
}

/// Common shape for events that name a room without a participant
/// (`room_started`, `room_finished`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomEnvelope {
    pub id: Option<String>,
    pub room: RoomInfo,
}

/// LiveKit `Room` projection. Only the fields Waddle actually needs
/// (the human room name, which is also our [`crate::CallId`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomInfo {
    /// Human room name minted by Waddle. For MUC calls this is the
    /// scoped `<bare-jid>::<sid>` (1:1) or the MUC room JID (group),
    /// matching [`crate::CallId`].
    pub name: String,
    /// LiveKit-internal opaque room sid. Used to distinguish a stale
    /// webhook for an older room incarnation from a newer call reusing
    /// the same human room name.
    #[serde(default)]
    pub sid: Option<RoomSid>,
}

/// LiveKit `ParticipantInfo` projection. Only the identity field is
/// load-bearing for Waddle's bridge — it round-trips the stringified
/// [`jid::FullJid`] we minted into the JWT's `sub` claim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParticipantInfo {
    pub identity: String,
    #[serde(default)]
    pub sid: Option<ParticipantSid>,
    #[serde(default)]
    pub state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiSecret;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    fn fixture_secret() -> ApiSecret {
        ApiSecret::from_text("test-secret-with-at-least-32-bytes-of-entropy")
            .expect("fixture secret meets length minimum")
    }

    fn body_sha256_claim(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(body);
        let digest = hasher.finalize();
        Base64Standard.encode(digest)
    }

    fn sign_claims(secret: &ApiSecret, claims: &serde_json::Value) -> String {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode test JWT")
    }

    fn sign_for_body(secret: &ApiSecret, body: &[u8]) -> String {
        let claims = json!({
            "sha256": body_sha256_claim(body),
            "exp": (chrono::Utc::now() + chrono::Duration::seconds(60)).timestamp(),
            "iat": chrono::Utc::now().timestamp(),
        });
        sign_claims(secret, &claims)
    }

    #[test]
    fn verifies_a_well_formed_participant_left_payload() {
        let secret = fixture_secret();
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "id": "EV_test_1",
            "room": { "name": "general@muc.test", "sid": "RM_xxx" },
            "participant": {
                "identity": "alice@waddle.test/desktop",
                "sid": "PA_xxx",
                "state": "DISCONNECTED",
            },
        }))
        .unwrap();
        let auth = format!("Bearer {}", sign_for_body(&secret, &body));

        let event = verify_webhook_signature(&secret, Some(&auth), &body).expect("verify");
        match event {
            LiveKitWebhookEvent::ParticipantLeft(env) => {
                assert_eq!(env.id.as_deref(), Some("EV_test_1"));
                assert_eq!(env.room.name, "general@muc.test");
                assert_eq!(
                    env.room.sid,
                    Some(RoomSid::new("RM_xxx").expect("valid typed room sid"))
                );
                assert_eq!(env.participant.identity, "alice@waddle.test/desktop");
                assert_eq!(
                    env.participant.sid,
                    Some(ParticipantSid::new("PA_xxx").expect("valid typed participant sid"))
                );
            }
            other => panic!("expected ParticipantLeft, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_sid_shapes_during_payload_parse() {
        let secret = fixture_secret();
        let body = serde_json::to_vec(&json!({
            "event": "participant_left",
            "room": { "name": "general@muc.test", "sid": "" },
            "participant": { "identity": "alice@waddle.test/desktop", "sid": "PA_xxx" },
        }))
        .unwrap();
        let auth = format!("Bearer {}", sign_for_body(&secret, &body));

        let err = verify_webhook_signature(&secret, Some(&auth), &body).unwrap_err();
        assert!(matches!(err, WebhookVerifyError::BodyJson(_)));
    }

    #[test]
    fn accepts_bare_jwt_without_bearer_prefix() {
        let secret = fixture_secret();
        let body =
            serde_json::to_vec(&json!({ "event": "room_finished", "room": { "name": "x" } }))
                .unwrap();
        let token = sign_for_body(&secret, &body);
        verify_webhook_signature(&secret, Some(&token), &body).expect("bare JWT also accepted");
    }

    #[test]
    fn rejects_missing_authorization() {
        let secret = fixture_secret();
        let body = b"{}";
        let err = verify_webhook_signature(&secret, None, body).unwrap_err();
        assert!(matches!(err, WebhookVerifyError::MissingAuthorization));
    }

    #[test]
    fn rejects_a_payload_that_does_not_match_the_signed_hash() {
        let secret = fixture_secret();
        let signed_for = serde_json::to_vec(&json!({"event": "participant_left", "room": {"name": "a"}, "participant": {"identity": "x"}})).unwrap();
        let auth = format!("Bearer {}", sign_for_body(&secret, &signed_for));
        // Send a different body than the one whose hash is in the JWT.
        let tampered =
            serde_json::to_vec(&json!({"event": "room_finished", "room": {"name": "b"}})).unwrap();
        let err = verify_webhook_signature(&secret, Some(&auth), &tampered).unwrap_err();
        assert!(matches!(err, WebhookVerifyError::BodyHashMismatch));
    }

    #[test]
    fn rejects_a_jwt_without_an_exp_claim() {
        let secret = fixture_secret();
        let body =
            serde_json::to_vec(&json!({"event": "room_finished", "room": {"name": "x"}})).unwrap();
        let claims = json!({
            "sha256": body_sha256_claim(&body),
            "iat": chrono::Utc::now().timestamp(),
        });
        let auth = format!("Bearer {}", sign_claims(&secret, &claims));
        let err = verify_webhook_signature(&secret, Some(&auth), &body).unwrap_err();
        assert!(
            matches!(
                &err,
                WebhookVerifyError::Jwt(jwt)
                    if matches!(
                        jwt.kind(),
                        jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(claim)
                            if claim == "exp"
                    )
            ),
            "expected missing-exp rejection, got {err:?}"
        );
    }

    #[test]
    fn rejects_an_expired_jwt() {
        let secret = fixture_secret();
        let body =
            serde_json::to_vec(&json!({"event": "room_finished", "room": {"name": "x"}})).unwrap();
        let claims = json!({
            "sha256": body_sha256_claim(&body),
            // Older than the 30s clock-skew leeway, so it must bounce.
            "exp": (chrono::Utc::now() - chrono::Duration::seconds(120)).timestamp(),
            "iat": (chrono::Utc::now() - chrono::Duration::seconds(180)).timestamp(),
        });
        let auth = format!("Bearer {}", sign_claims(&secret, &claims));
        let err = verify_webhook_signature(&secret, Some(&auth), &body).unwrap_err();
        assert!(matches!(err, WebhookVerifyError::Jwt(_)));
    }

    #[test]
    fn rejects_a_jwt_signed_with_a_different_secret() {
        let secret = fixture_secret();
        let other = ApiSecret::from_text("different-test-secret-that-is-also-long-enough").unwrap();
        let body =
            serde_json::to_vec(&json!({"event": "room_finished", "room": {"name": "x"}})).unwrap();
        let auth = format!("Bearer {}", sign_for_body(&other, &body));
        let err = verify_webhook_signature(&secret, Some(&auth), &body).unwrap_err();
        assert!(matches!(err, WebhookVerifyError::Jwt(_)));
    }

    #[test]
    fn unknown_event_kinds_parse_to_other_variant() {
        let secret = fixture_secret();
        let body = serde_json::to_vec(&json!({ "event": "egress_started" })).unwrap();
        let auth = format!("Bearer {}", sign_for_body(&secret, &body));
        let event = verify_webhook_signature(&secret, Some(&auth), &body).expect("verify");
        assert_eq!(event, LiveKitWebhookEvent::Other);
        assert_eq!(event.event_id(), None);
    }

    #[test]
    fn event_id_round_trips_through_typed_variants() {
        let env = ParticipantEnvelope {
            id: Some("EV_42".into()),
            room: RoomInfo {
                name: "r".into(),
                sid: None,
            },
            participant: ParticipantInfo {
                identity: "alice@waddle.test/desktop".into(),
                sid: None,
                state: None,
            },
        };
        let event = LiveKitWebhookEvent::ParticipantLeft(env);
        assert_eq!(event.event_id(), Some("EV_42"));
    }
}
