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

use std::future::Future;
use std::pin::Pin;

mod admin;
mod call;
mod config;
mod correlation;
mod error;
mod livekit;
mod token;
mod turn;
mod webhook;

pub use admin::LiveKitAdmin;
pub use call::{CallId, CallState, Identity, MediaCapabilities};
pub use config::{ApiKey, ApiSecret, FromEnvError, SfuConfig, TurnSharedSecret, WebsocketUrl};
pub use correlation::{CallCorrelationId, CORRELATION_ID_HEX_LEN};
pub use error::SfuError;
pub use livekit::{LiveKitSfu, RECONCILE_GRACE_SECONDS};
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
    /// extension. Implementations also revoke every JWT the SFU has
    /// minted for the `(call_id, identity)` pair so a stolen token
    /// can't be replayed after the legitimate hangup, and may
    /// proactively notify the underlying SFU so the call doesn't
    /// linger until the SFU's own session-timeout — production
    /// [`LiveKitSfu`] does this via the LiveKit admin REST API.
    ///
    /// When `identity` was never registered for `call_id`, the
    /// implementation MUST NOT report [`CallState::Ended`] (returning
    /// `Active { remaining }` instead): a stale or replayed teardown
    /// must not trigger room-end broadcast or SFU-side room deletion
    /// for a call the caller does not actually own membership in.
    fn unregister_call_participant(&self, call_id: &CallId, identity: &Identity) -> CallState;

    /// Local-only cleanup driven by an SFU-originated signal that
    /// `identity` has already left `call_id` (e.g. LiveKit's
    /// `participant_left` webhook). Mirrors the bookkeeping side of
    /// [`Self::unregister_call_participant`] — registry removal +
    /// JWT revocation — but MUST NOT issue a corresponding admin
    /// call back to the SFU: the webhook is the acknowledgement
    /// that the SFU already evicted the participant, and a
    /// back-channel `RemoveParticipant` would amplify into a wasted
    /// round-trip plus a race window against quick rejoins.
    fn note_participant_left(&self, call_id: &CallId, identity: &Identity);

    /// Push replacement media grants to a live participant after an
    /// XEP-0045 voice change, without disconnecting them: losing voice
    /// becomes listen-only immediately (the SFU force-unpublishes
    /// their tracks), regaining it lets them publish without
    /// renegotiating. The remote leg is fire-and-forget like
    /// [`Self::unregister_call_participant`]'s.
    ///
    /// This runs unconditionally, exactly like
    /// `unregister_call_participant`'s `RemoveParticipant`: the SFU may
    /// know about a participant our per-process registry has lost
    /// track of (a reconnect after `participant_left`, a room actor
    /// that migrated between cluster nodes, reconciliation sweeps).
    /// Gating on local registration would make the *downgrade* fail
    /// open, which is the one direction that must never be skipped.
    ///
    /// A downgrade also revokes every outstanding JWT minted for the
    /// `(call_id, identity)` pair. NOTE: that revocation is local
    /// bookkeeping only — LiveKit derives permissions from the JWT at
    /// join time and never consults [`Self::is_revoked`], so a token
    /// minted before the downgrade still admits its holder with the
    /// old grants until `exp`. The mechanism that actually closes
    /// that window is re-asserting permissions when LiveKit reports
    /// the participant joined (`participant_joined` webhook); the
    /// revocation entry exists for the future
    /// LiveKit-cooperative-validation path and for local replay
    /// checks.
    fn update_participant_capabilities(
        &self,
        call_id: &CallId,
        identity: &Identity,
        capabilities: MediaCapabilities,
    );

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

/// Future returned by [`SfuReconciler::reconcile_active_calls`],
/// resolving to the `(call, identity)` pairs swept this pass. Named so
/// the boxed-future shape stays readable at the trait + impl sites.
pub type ReconcileFuture<'a> = Pin<Box<dyn Future<Output = Vec<(CallId, Identity)>> + Send + 'a>>;

/// Periodic reconciliation against the SFU's ground truth.
///
/// Separate from [`SfuService`] because it is inherently async (an
/// HTTP round-trip to LiveKit's `ListParticipants`) whereas
/// `SfuService` is deliberately sync. A background task in
/// `waddle-server` drives this on an interval so that a lost
/// `participant_left` / `room_finished` webhook delivery cannot leave
/// a permanent ghost in the registry (and therefore in MUC presence):
/// LiveKit, not our in-memory registry, is the authority on who is
/// actually connected.
pub trait SfuReconciler: Send + Sync + 'static {
    /// Sweep registry entries LiveKit no longer reports as connected,
    /// respecting a registration grace window so still-connecting
    /// participants are not mistaken for ghosts. Returns the
    /// `(call, identity)` pairs swept so the caller can clear the
    /// corresponding MUC Muji presence idempotently.
    fn reconcile_active_calls(&self, grace: chrono::Duration) -> ReconcileFuture<'_>;

    /// The identities the SFU itself reports as currently connected to
    /// `call_id`, or an empty vec when the SFU does not know the room.
    ///
    /// Authoritative and cluster-wide, unlike
    /// [`SfuService::participants_for_call`], which only sees the
    /// calling process's registry. Any convergence decision that must
    /// hold across nodes — notably the voice-grant reconciliation
    /// backstop, which runs on whichever node claims a room and not
    /// necessarily the node that registered the participant — MUST use
    /// this rather than the local registry, or it silently skips
    /// participants and fails open.
    fn live_participants<'a>(&'a self, call_id: &'a CallId) -> LiveParticipantsFuture<'a>;
}

/// Future returned by [`SfuReconciler::live_participants`].
pub type LiveParticipantsFuture<'a> = Pin<Box<dyn Future<Output = Vec<Identity>> + Send + 'a>>;
