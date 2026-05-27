//! LiveKit SFU bridge for Waddle's XMPP-native A/V calling.
//!
//! This crate is the *only* place where LiveKit-specific concerns
//! live. Its public API speaks typed Waddle values
//! ([`CallId`], [`Identity`], [`JoinToken`], [`TurnCredential`]) so the
//! XMPP layer can stay LiveKit-agnostic.
//!
//! Two layers are exposed:
//!
//! 1. The [`SfuService`] trait — the abstract surface consumed by
//!    [`waddle_xmpp`]'s Jingle / `extdisco:2` handlers. It is sync
//!    because every operation is CPU-bound (HMAC / JWT signing) plus a
//!    constant-time [`dashmap`] update.
//! 2. The [`LiveKitSfu`] impl — the concrete bridge that signs JWTs
//!    against the `livekit-sfu-api-keys` Secret and tracks MUC focus
//!    state in-memory.
//!
//! See `server/crates/waddle-xmpp/src/protocol/handlers/jingle.rs` for
//! the consumer.

#![deny(unsafe_code)]

mod call;
mod config;
mod error;
mod livekit;
mod token;
mod turn;
mod webhook;

pub use call::{CallId, CallState, Identity, MediaCapabilities};
pub use config::{ApiKey, ApiSecret, FromEnvError, SfuConfig, TurnSharedSecret, WebsocketUrl};
pub use error::SfuError;
pub use livekit::LiveKitSfu;
pub use token::{JoinToken, Jti, Jwt, VideoGrant};
pub use turn::{TurnCredential, TurnHost, TurnPassword, TurnUsername};
pub use webhook::{
    verify_webhook_signature, LiveKitWebhookEvent, ParticipantEnvelope, ParticipantInfo,
    RoomEnvelope, RoomInfo, WebhookVerifyError,
};

/// Abstract SFU service consumed by the XMPP layer.
///
/// Stateless operations ([`Self::issue_join_token`],
/// [`Self::issue_turn_credentials`]) are sync because they are
/// CPU-only. Stateful operations
/// ([`Self::register_call_participant`] /
/// [`Self::unregister_call_participant`]) update an in-memory call
/// registry used for MUC focus accounting.
pub trait SfuService: Send + Sync + 'static {
    /// Mint a short-lived LiveKit join JWT for `identity` to enter
    /// `call_id` with the given media capabilities. The returned token
    /// carries the LiveKit websocket URL, the room name, and an
    /// expiry timestamp so callers can refresh proactively.
    fn issue_join_token(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    ) -> Result<JoinToken, SfuError>;

    /// Mint a short-lived TURN credential pair for `identity`.
    /// Credentials are HMAC-SHA1 over `<expiry_unix>:<identity>` per
    /// the time-limited-credentials draft that LiveKit's coturn
    /// configuration follows.
    fn issue_turn_credentials(&self, identity: &Identity) -> Result<TurnCredential, SfuError>;

    /// Record that `identity` has joined `call_id`. Idempotent: a
    /// repeat join is a no-op (the registry is a set).
    fn register_call_participant(&self, call_id: &CallId, identity: &Identity);

    /// Return whether `identity` is currently registered in
    /// `call_id`. Call teardown uses this as the authorization check
    /// before revoking any other participant's token state.
    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool;

    /// Record that `identity` has left `call_id` and report whether
    /// the call is still active. When the last participant leaves,
    /// the call entry is removed and [`CallState::Ended`] is
    /// returned, allowing the caller to clear the MUC presence
    /// extension. Also revokes every JWT the SFU has minted for the
    /// `(call_id, identity)` pair so a stolen token can't be replayed
    /// after the legitimate hangup. (Revocation is bookkeeping —
    /// LiveKit itself doesn't call back to verify jti, so the value
    /// is local-side replay-resistance + an audit trail; see
    /// [`Self::is_revoked`].)
    fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState;

    /// Returns `true` if the SFU has marked `jti` as revoked. Useful
    /// for tests + future LiveKit-cooperative validation: when
    /// LiveKit gains the ability to delegate token verification back
    /// to the issuer (e.g. via a webhook), this is the source of
    /// truth.
    fn is_revoked(&self, jti: &Jti) -> bool;

    /// LiveKit client-facing websocket URL (e.g.
    /// `wss://livekit.waddle.social`). Embedded verbatim into the
    /// `urn:waddle:transports:livekit:0` transport on outbound
    /// session stanzas.
    fn ws_url(&self) -> &WebsocketUrl;

    /// TURN host advertised via XEP-0215 (e.g.
    /// `turn.waddle.social`).
    fn turn_host(&self) -> &TurnHost;

    /// Shared secret used by [`verify_webhook_signature`] to validate
    /// inbound LiveKit webhook deliveries. Typically distinct from
    /// the JWT-signing API secret per LK operational guidance, but
    /// defaults to the API secret for dev parity.
    fn webhook_secret(&self) -> &ApiSecret;

    /// Snapshot the currently-registered identities for `call_id`.
    /// Used by the LiveKit webhook handler's `room_finished` arm to
    /// clean up any per-participant state that an earlier
    /// `participant_left` retry-exhausted before the room closed.
    /// Returns an empty vec when the call has no recorded participants
    /// (either never registered or already drained).
    fn participants_for_call(&self, call_id: &CallId) -> Vec<Identity>;
}
