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

pub use admin::{ListedRoom, LiveKitAdmin, RoomOccupancy};
pub use call::{
    CallGeneration, CallId, CallState, CallTeardownIntentLite, Identity, MediaCapabilities,
    ObservedCallSids, ParticipantSid, RoomSid, SidObservationDirection, SidObservationDisposition,
    TeardownDisposition, TeardownTargetLite,
};
pub use config::{ApiKey, ApiSecret, FromEnvError, SfuConfig, TurnSharedSecret, WebsocketUrl};
pub use correlation::{CallCorrelationId, CORRELATION_ID_HEX_LEN};
pub use error::SfuError;
pub use livekit::{
    LiveKitSfu, LiveKitTeardownExecutor, TeardownExecution, TeardownFailureSink,
    RECONCILE_CONCURRENCY, RECONCILE_GRACE_SECONDS,
};
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

    /// Record that `identity` has joined `call_id`. Idempotent within
    /// one call generation: a repeat join is a no-op.
    fn register_call_participant(&self, call_id: &CallId, identity: &Identity);

    /// Register a participant learned from an authoritative
    /// `participant_joined` webhook and atomically learn its observed
    /// room/participant SIDs. Unlike token issuance, this only restores
    /// local bookkeeping after a process restart.
    ///
    /// If an existing call entry has a conflicting room SID, the event
    /// MUST return [`SidObservationDisposition::RoomRotationPending`]
    /// without mutation. The webhook can then be redelivered after an
    /// authoritative room listing rotates the stored incarnation.
    fn register_call_participant_observed(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: &ObservedCallSids,
    ) -> SidObservationDisposition;

    /// Return whether `identity` is currently registered in
    /// `call_id`. Call teardown uses this as the authorization check
    /// before revoking any other participant's token state.
    fn has_call_participant(&self, call_id: &CallId, identity: &Identity) -> bool;

    /// Wall-clock instant `identity`'s CURRENT registration in
    /// `call_id` was recorded, or `None` when not registered (or when
    /// the implementation does not track registration times). The
    /// durable teardown drain compares this against an intent's
    /// creation time: only a registration that POSTDATES the intent
    /// proves a rejoin — a mere live registration can equally mean
    /// the departure this intent represents was never applied on this
    /// node (#1449 review N1).
    fn participant_registered_at(
        &self,
        _call_id: &CallId,
        _identity: &Identity,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        None
    }

    /// Wall-clock instant `identity` most recently received a locally
    /// minted join token for its CURRENT registration in `call_id`, or
    /// `None` when not registered (or when the implementation does not
    /// track mint times). Higher layers can combine this with
    /// [`Self::participant_registered_at`] to distinguish a
    /// freshly-observed participant that this node never minted for
    /// from one that rejoined through a local token issuance.
    fn participant_last_minted_at(
        &self,
        _call_id: &CallId,
        _identity: &Identity,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        None
    }

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
    fn unregister_call_participant(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition;

    /// Targeted revocation of ONE issued JWT for the pair (#1444):
    /// moves `jti` into the revocation set without touching the
    /// participant's registration or their other issuances, then
    /// schedules the same generation/SID-guarded `RemoveParticipant`
    /// convergence path that a full unregister uses when the
    /// revocation empties the pair's active issuance window (the
    /// downgrade-to-nothing case). LiveKit never consults our revoked
    /// map on join, so active enforcement for a live holder has to
    /// happen through the admin API rather than by local bookkeeping
    /// alone.
    ///
    /// If the revoked token is first used only AFTER this local
    /// revocation, convergence still comes from that guarded eject
    /// once the live participant becomes observable again (for
    /// example through a later participant-join observation or room
    /// adoption/reconciliation).
    ///
    /// Implementations MUST ignore a `jti` that is not currently
    /// tracked in the pair's issued window: the identifier arrives
    /// from an unverified claim in the bounced stanza, and recording
    /// revocations for JTIs the server never minted would let crafted
    /// undeliverable IQs grow the revocation set without bound.
    fn revoke_issued_token(&self, call_id: &CallId, identity: &Identity, jti: &Jti);

    /// Local-only cleanup driven by an SFU-originated signal that
    /// `identity` has already left `call_id` (e.g. LiveKit's
    /// `participant_left` webhook). Mirrors the bookkeeping side of
    /// [`Self::unregister_call_participant`] — registry removal +
    /// JWT revocation — but MUST NOT issue a corresponding admin
    /// call back to the SFU: the webhook is the acknowledgement
    /// that the SFU already evicted the participant, and a
    /// back-channel `RemoveParticipant` would amplify into a wasted
    /// round-trip plus a race window against quick rejoins.
    fn note_participant_left(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
    ) -> TeardownDisposition;

    /// Learn observed room/participant sids for an existing
    /// `(call_id, identity)` without changing membership or issuing
    /// admin calls. Used by SID-bearing informational events such as
    /// `participant_joined` so later teardown can reject an older call
    /// incarnation reusing the same human room name.
    ///
    /// Implementations MUST NOT create new call or participant entries
    /// from this method. If either is unknown, the observation is
    /// ignored and reported as applied. A join-side room-SID mismatch
    /// is pending until reconciliation rotates the stored incarnation;
    /// a leave-side mismatch is stale and remains a no-op.
    fn observe_call_participant_sids(
        &self,
        call_id: &CallId,
        identity: &Identity,
        observed_sids: Option<&ObservedCallSids>,
        direction: SidObservationDirection,
    ) -> SidObservationDisposition;

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

/// Future returned by [`SfuReconciler::reconcile_active_calls`]. Named
/// so the boxed-future shape stays readable at the trait + impl sites.
pub type ReconcileFuture<'a> = Pin<Box<dyn Future<Output = ReconcilePassSummary> + Send + 'a>>;

/// Typed, telemetry-free result of one LiveKit reconciliation pass.
/// `waddle-server` translates these values into process metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcilePassSummary {
    pub swept: Vec<(CallId, Identity)>,
    pub rooms_examined: u64,
    pub rooms_adopted: u64,
    pub rooms_swept: u64,
    pub occupancy_failures: u64,
}

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
    /// Discover active LiveKit rooms, adopt missing registry entries,
    /// and sweep entries LiveKit no longer reports as connected while
    /// respecting the registration grace window. The summary includes
    /// swept identities for idempotent MUC Muji presence cleanup.
    fn reconcile_active_calls(&self, grace: chrono::Duration) -> ReconcileFuture<'_>;

    /// The identities the SFU itself reports as currently connected to
    /// `call_id`. `Some(vec![])` means the SFU confirmed nobody is
    /// connected; `None` means the SFU could not be reached, so absence
    /// is UNCONFIRMED.
    ///
    /// Authoritative and cluster-wide, unlike
    /// [`SfuService::participants_for_call`], which only sees the
    /// calling process's registry. Any convergence decision that must
    /// hold across nodes — notably the voice-grant reconciliation
    /// backstop, which runs on whichever node claims a room and not
    /// necessarily the node that registered the participant — MUST use
    /// this rather than the local registry, or it silently skips
    /// participants and fails open.
    ///
    /// Callers MUST NOT treat `None` as "nobody is connected": doing so
    /// lets a LiveKit outage silently disable the convergence that
    /// depends on this, which is a security-relevant backstop.
    fn live_participants<'a>(&'a self, call_id: &'a CallId) -> LiveParticipantsFuture<'a>;
}

/// Future returned by [`SfuReconciler::live_participants`].
pub type LiveParticipantsFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Vec<Identity>>> + Send + 'a>>;
