//! Injected gate dependencies for the T1 drain: room policy, DND, and
//! the XEP-0513 active-mention TTL window.

use super::*;

/// T1 lookup of XEP-0045 room policy state needed to project a
/// candidate's conversation kind for the XEP-0492 evaluator.
///
/// At T0 (candidate emission) we record the message-derived
/// [`NotificationClass`] only. At T1 (outbox dispatch) the evaluator
/// needs to know whether the room is members-only to pick
/// [`crate::notification_settings_projection::ConversationKind::PrivateGroup`]
/// or [`crate::notification_settings_projection::ConversationKind::PublicGroup`]
/// — the kind drives both the XEP-0492 default level and the projection
/// store lookup.
///
/// Returning `Ok(None)` signals "room not currently live" — the T1
/// evaluator treats this as an *unknown* signal (not a public one)
/// and defers the candidate via the policy-error backoff so the next
/// drain pass can retry once the actor is reachable. Slice 2 will
/// replace the live-actor lookup with a durable T1 projection of
/// MUC config that does not have this hole.
#[async_trait::async_trait]
pub trait RoomPolicyStore: Send + Sync {
    async fn room_members_only(
        &self,
        room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError>;
}

/// Zero-state [`RoomPolicyStore`] for DM emission paths.
///
/// The T0 emission gate for direct messages calls
/// [`evaluate_push_gate_at_dispatch`] on a candidate whose class is
/// [`NotificationClass::DirectMessage`] or
/// [`NotificationClass::DirectMessageMention`]. Those arms never
/// dispatch into `room_policy`, so the trait object is held only to
/// satisfy the typed signature. This adapter encodes that no-op shape
/// once at the type level; if the evaluator ever did consult it for a
/// DM, it would surface as [`T1PushDispatchOutcome::DeferUnknownRoomPolicy`]
/// rather than a silent default — fail-loud per the slice 1 design.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRoomPolicy;

#[async_trait::async_trait]
impl RoomPolicyStore for NoopRoomPolicy {
    async fn room_members_only(
        &self,
        _room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        Ok(None)
    }
}

/// Typed recipient-level Do Not Disturb state, consulted at T1 push
/// dispatch.
///
/// `Inactive` means the recipient is NOT in DND; the evaluator
/// proceeds with the XEP-0492 / XEP-0191 / XEP-0513 / XEP-0334 gates.
/// `Active` means the evaluator MUST suppress the candidate with
/// [`SuppressedReason::WaddleDnd`]. The DND state is a recipient-state
/// read (not a message-frozen fact), so the consultation belongs at
/// T1 alongside XEP-0492 — the same race-window semantics apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndState {
    Inactive,
    Active,
}

/// T1 lookup of the recipient's Waddle DnD state.
///
/// Production implementation lives in [`crate::dnd_reader::PepDndReader`],
/// which reads the durable [`crate::dnd_projection::DndProjectionStore`]
/// projection of the user's `urn:waddle:dnd:0` PEP item and resolves
/// the typed [`DndState`] via the pure evaluator in
/// [`waddle_xmpp::xep::xep_waddle_dnd`].
///
/// The T0 emit path keeps the [`NoopDndReader`] below — DND is a T1
/// recipient-state read and is intentionally not consulted at emit
/// time (see the stage check at the call site).
#[async_trait::async_trait]
pub trait DndReader: Send + Sync {
    async fn dnd_state(&self, user: &BareJid) -> Result<DndState, NotificationOutboxError>;
}

/// [`DndReader`] that reports every user as not-in-DND.
///
/// Used at the T0 emit call sites
/// ([`crate::server::routes::interpret::offline_delivery`],
/// [`crate::server::routes::interpret::groupchat_inbox`]) where the
/// evaluator's typed signature requires a reader but DND consultation
/// is skipped by the [`PushEvalStage::T1Drain`] guard. Production
/// T1 drain (`session_janitors`) uses [`crate::dnd_reader::PepDndReader`]
/// instead.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDndReader;

#[async_trait::async_trait]
impl DndReader for NoopDndReader {
    async fn dnd_state(&self, _user: &BareJid) -> Result<DndState, NotificationOutboxError> {
        Ok(DndState::Inactive)
    }
}

/// Typed bundle of T1 recipient-state readers consulted by
/// [`NotificationOutboxStore::drain_pending_candidates_into_outbox`].
///
/// Bundling these reduces the drain method's argument count below
/// the clippy `too_many_arguments` threshold without losing
/// explicitness — each field is a trait object, so call sites pass
/// distinct concrete implementations rather than a single composite
/// dependency.
#[derive(Copy, Clone)]
pub struct NotificationDrainDeps<'a> {
    pub room_policy: &'a dyn RoomPolicyStore,
    pub dnd_reader: &'a dyn DndReader,
    pub activity_reader: &'a dyn NotificationActivityReader,
}

impl<'a> NotificationDrainDeps<'a> {
    pub fn new(
        room_policy: &'a dyn RoomPolicyStore,
        dnd_reader: &'a dyn DndReader,
        activity_reader: &'a dyn NotificationActivityReader,
    ) -> Self {
        Self {
            room_policy,
            dnd_reader,
            activity_reader,
        }
    }
}

/// Default TTL window for the XEP-0513 `<active/>` push filter (5
/// minutes). A recipient whose
/// [`crate::notification_activity::NotificationActivity::last_active_at_ms`]
/// is older than `now - ACTIVE_MENTION_TTL_MS` is treated as
/// "currently not active" and the T1 evaluator suppresses
/// [`NotificationClass::ActiveChannelMention`] candidates with
/// [`SuppressedReason::Xep0513ActiveMiss`].
pub const DEFAULT_ACTIVE_MENTION_TTL_SECONDS: u64 = 300;

/// Lower bound for the operator-tunable TTL (1 second). A value of 0
/// would suppress *every* `ActiveChannelMention` regardless of
/// activity — almost certainly an operator misconfiguration — so we
/// clamp to a minimum of one second.
pub const MIN_ACTIVE_MENTION_TTL_SECONDS: u64 = 1;

/// Upper bound for the operator-tunable TTL (24 hours). Anything
/// beyond this is effectively "disable the filter"; deployments that
/// want that should remove the `ActiveChannelMention` candidate at the
/// emission boundary rather than turning the gate into a no-op.
pub const MAX_ACTIVE_MENTION_TTL_SECONDS: u64 = 86_400;

/// Environment variable an operator sets to tune the XEP-0513
/// `<active/>` mention TTL window (seconds). Public because it is an
/// operational interface; see [`active_mention_ttl_ms_from_env`].
pub const WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV: &str = "WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS";

/// Reads `WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS` and clamps to the
/// [`MIN_ACTIVE_MENTION_TTL_SECONDS`, `MAX_ACTIVE_MENTION_TTL_SECONDS`]
/// window. Unparseable or unset values fall back to
/// [`DEFAULT_ACTIVE_MENTION_TTL_SECONDS`].
pub fn active_mention_ttl_ms_from_env() -> i64 {
    let seconds = std::env::var(WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_ACTIVE_MENTION_TTL_SECONDS)
        .clamp(
            MIN_ACTIVE_MENTION_TTL_SECONDS,
            MAX_ACTIVE_MENTION_TTL_SECONDS,
        );
    i64::try_from(seconds.saturating_mul(1_000)).unwrap_or(i64::MAX)
}
