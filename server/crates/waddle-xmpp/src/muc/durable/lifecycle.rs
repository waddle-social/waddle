//! Typed room lifecycle and commit coordinates for room-claim fencing (#1644).
//!
//! A room JID is intentionally not sufficient to identify durable room state:
//! XEP-0045 permits a destroyed room to be created again with the same JID.
//! The durable create path in #1645 therefore mints a new lifecycle for every
//! create-to-destroy/tombstone incarnation. Revisions and post-commit effects
//! are then scoped to that lifecycle, keeping stale work from an older
//! incarnation distinguishable from the newly created room.
//!
//! This is a dark foundation slice. It deliberately contains no SQL mapping,
//! serialization, or effect algebra: #1645 owns the first durable writer,
//! #1646 owns the typed outbox algebra, and #1647 owns projection resync.

use std::fmt;

use super::RoomClaimFenceContext;

mod commit {
    use super::{RoomClaimFenceContext, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};

    /// Proof returned by the durable commit path before any in-memory apply.
    /// Only this submodule can construct it directly.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RoomMutationCommit {
        fence: RoomClaimFenceContext,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
    }

    impl RoomMutationCommit {
        pub fn fence(&self) -> &RoomClaimFenceContext {
            &self.fence
        }

        pub fn lifecycle(&self) -> RoomLifecycleId {
            self.lifecycle
        }

        pub fn revision(&self) -> RoomRevision {
            self.revision
        }
    }

    /// A transient, one-use capability authorizing an ephemeral projection
    /// after a durable commit.
    #[must_use = "the authorization must be consumed to project its durable commit"]
    #[derive(Debug)]
    pub struct EphemeralProjectionAuthorization {
        commit: RoomMutationCommit,
    }

    impl EphemeralProjectionAuthorization {
        /// Consume this one-use capability and recover the durable-commit
        /// proof that authorizes the ephemeral projection.
        pub fn consume(self) -> RoomMutationCommit {
            self.commit
        }
    }

    /// The sole public mint path for a room durable-commit proof.
    pub(super) fn mint_room_mutation_commit(
        fence: RoomClaimFenceContext,
        coordinates: RoomCommittedCoordinates,
    ) -> RoomMutationCommit {
        RoomMutationCommit {
            fence,
            lifecycle: coordinates.lifecycle,
            revision: coordinates.revision,
        }
    }

    /// Mint the one-use authorization paired with a durable commit proof.
    pub(super) fn authorize_ephemeral_projection(
        commit: RoomMutationCommit,
    ) -> EphemeralProjectionAuthorization {
        EphemeralProjectionAuthorization { commit }
    }
}

pub use commit::{EphemeralProjectionAuthorization, RoomMutationCommit};

pub(crate) fn mint_room_mutation_commit(
    fence: RoomClaimFenceContext,
    coordinates: RoomCommittedCoordinates,
) -> RoomMutationCommit {
    commit::mint_room_mutation_commit(fence, coordinates)
}

pub(crate) fn authorize_ephemeral_projection(
    commit: RoomMutationCommit,
) -> EphemeralProjectionAuthorization {
    commit::authorize_ephemeral_projection(commit)
}

/// Identifies one room incarnation, from durable creation until the room is
/// destroyed or tombstoned. The #1645 durable create path mints this value so
/// a later room recreated at the same JID cannot be confused with its former
/// incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomLifecycleId(uuid::Uuid);

impl RoomLifecycleId {
    /// Mint a fresh, time-ordered lifecycle identity for one room incarnation.
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub const fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl fmt::Display for RoomLifecycleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A monotonic authoritative-state revision within one [`RoomLifecycleId`].
/// Comparing revisions from different lifecycles is meaningless: a recreated
/// room starts a new revision sequence even when it reuses the same room JID.
/// The #1645 durable-commit path advances this coordinate under its exact
/// claim fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomRevision(i64);

impl RoomRevision {
    /// The revision of a newly created room lifecycle.
    pub fn initial() -> Self {
        Self(1)
    }

    /// Advance this lifecycle-local revision, failing closed on integer
    /// overflow so a durable writer cannot wrap back to an older coordinate.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub const fn as_i64(self) -> i64 {
        self.0
    }

    /// Restore a revision read from durable storage, rejecting values that
    /// violate the one-based revision invariant.
    pub fn from_stored(value: i64) -> Option<Self> {
        (value >= 1).then_some(Self(value))
    }
}

/// The ordinal of one post-commit effect, scoped to a single committed
/// `(lifecycle, revision)` coordinate: each committed revision emits its
/// effects as ordinals `0, 1, 2, …`. Together with [`RoomLifecycleId`] and
/// [`RoomRevision`] it is the third component of #1646's effect-outbox key;
/// the per-lifecycle FIFO #1646 requires is the order of
/// `(RoomRevision, RoomEffectOrdinal)` within one lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomEffectOrdinal(i64);

impl RoomEffectOrdinal {
    /// The ordinal of the first effect emitted for a committed revision.
    pub fn first() -> Self {
        Self(0)
    }

    /// Advance the effect ordinal, failing closed on integer overflow rather
    /// than allowing an outbox key to wrap and overwrite older work.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub const fn as_i64(self) -> i64 {
        self.0
    }

    /// Restore an ordinal read from durable storage, rejecting negative
    /// values because the first outbox effect is ordinal zero.
    pub fn from_stored(value: i64) -> Option<Self> {
        (value >= 0).then_some(Self(value))
    }
}

/// Typed vocabulary for the room lifecycle `state` column. The #1645 first
/// durable writer owns the database-string mapping, keeping this dark #1644
/// slice free of persistence encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomLifecycleState {
    Active,
    Dormant,
    Tombstoned,
}

impl RoomLifecycleState {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            RoomLifecycleState::Active => "active",
            RoomLifecycleState::Dormant => "dormant",
            RoomLifecycleState::Tombstoned => "tombstoned",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "active" => Some(RoomLifecycleState::Active),
            "dormant" => Some(RoomLifecycleState::Dormant),
            "tombstoned" => Some(RoomLifecycleState::Tombstoned),
            _ => None,
        }
    }
}

/// Plain committed lifecycle/revision coordinate returned by the durable
/// commit path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomCommittedCoordinates {
    pub lifecycle: RoomLifecycleId,
    pub revision: RoomRevision,
}

/// Typed destroy-attempt identity for the registry pre-seal protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DestroyAttemptId(uuid::Uuid);

impl DestroyAttemptId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub const fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

/// A typed outbox envelope for #1646, keyed by `(RoomLifecycleId,
/// RoomRevision, RoomEffectOrdinal)` and delivered in strict per-lifecycle
/// FIFO order. #1646 instantiates `E` with its typed effect algebra instead
/// of a closed enum here: post-commit effects are currently produced at both
/// actor handlers and websocket handlers emitting recipient-specific stanzas,
/// so freezing variants in this dark slice would be incorrect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomEffectIntent<E> {
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    ordinal: RoomEffectOrdinal,
    effect: E,
}

impl<E> RoomEffectIntent<E> {
    pub fn new(
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
        ordinal: RoomEffectOrdinal,
        effect: E,
    ) -> Self {
        Self {
            lifecycle,
            revision,
            ordinal,
            effect,
        }
    }

    pub fn lifecycle(&self) -> RoomLifecycleId {
        self.lifecycle
    }

    pub fn revision(&self) -> RoomRevision {
        self.revision
    }

    pub fn ordinal(&self) -> RoomEffectOrdinal {
        self.ordinal
    }

    pub fn effect(&self) -> &E {
        &self.effect
    }

    pub fn into_effect(self) -> E {
        self.effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

    fn claim_fence() -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, "lifecycle-test@rooms.example.test"),
            NodeIdentity::new("test-node", "test-incarnation"),
            ClaimEpoch(1),
        )
    }

    #[test]
    fn revision_is_one_based_monotonic_and_never_wraps() {
        let initial = RoomRevision::initial();
        let next = initial.next().expect("initial revision increments");

        assert_eq!(initial.as_i64(), 1);
        assert_eq!(next.as_i64(), 2);
        assert!(initial < next);
        assert_eq!(RoomRevision::from_stored(0), None);
        assert_eq!(RoomRevision::from_stored(-1), None);
        assert_eq!(RoomRevision::from_stored(1), Some(initial));
        assert_eq!(
            RoomRevision::from_stored(i64::MAX)
                .expect("the largest valid stored revision is representable")
                .next(),
            None
        );
    }

    #[test]
    fn effect_ordinal_is_zero_based_monotonic_and_never_wraps() {
        let first = RoomEffectOrdinal::first();

        assert_eq!(first.as_i64(), 0);
        assert_eq!(first.next().expect("first ordinal increments").as_i64(), 1);
        assert_eq!(RoomEffectOrdinal::from_stored(-1), None);
        assert_eq!(
            RoomEffectOrdinal::from_stored(i64::MAX)
                .expect("the largest valid stored ordinal is representable")
                .next(),
            None
        );
    }

    #[test]
    fn lifecycle_id_round_trips_and_displays_as_a_uuid() {
        let first = RoomLifecycleId::generate();
        let second = RoomLifecycleId::generate();
        let known = uuid::Uuid::now_v7();

        assert_ne!(first, second);
        assert_eq!(RoomLifecycleId::from_uuid(known).as_uuid(), known);
        assert_eq!(
            uuid::Uuid::parse_str(&first.to_string()).expect("display is a UUID"),
            first.as_uuid()
        );
    }

    #[test]
    fn mutation_commit_preserves_its_exact_fence_and_coordinate() {
        let fence = claim_fence();
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        let commit = mint_room_mutation_commit(
            fence.clone(),
            RoomCommittedCoordinates {
                lifecycle,
                revision,
            },
        );

        assert_eq!(commit.fence(), &fence);
        assert_eq!(commit.lifecycle(), lifecycle);
        assert_eq!(commit.revision(), revision);
    }

    #[test]
    fn effect_intent_envelope_preserves_its_key_and_effect() {
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        let ordinal = RoomEffectOrdinal::first();
        let intent = RoomEffectIntent::new(lifecycle, revision, ordinal, 7_u8);

        assert_eq!(intent.lifecycle(), lifecycle);
        assert_eq!(intent.revision(), revision);
        assert_eq!(intent.ordinal(), ordinal);
        assert_eq!(*intent.effect(), 7);
        assert_eq!(intent.into_effect(), 7);
    }

    #[test]
    fn authorization_consumes_the_exact_commit() {
        let commit = mint_room_mutation_commit(
            claim_fence(),
            RoomCommittedCoordinates {
                lifecycle: RoomLifecycleId::generate(),
                revision: RoomRevision::initial(),
            },
        );
        let expected = commit.clone();

        assert_eq!(authorize_ephemeral_projection(commit).consume(), expected);
    }

    #[test]
    fn lifecycle_state_db_strings_round_trip() {
        for state in [
            RoomLifecycleState::Active,
            RoomLifecycleState::Dormant,
            RoomLifecycleState::Tombstoned,
        ] {
            assert_eq!(
                RoomLifecycleState::from_db_str(state.as_db_str()),
                Some(state)
            );
        }
        assert_eq!(RoomLifecycleState::from_db_str("unknown"), None);
    }

    #[test]
    fn destroy_attempt_id_round_trips_and_displays_as_a_uuid() {
        let attempt = DestroyAttemptId::generate();
        let known = uuid::Uuid::now_v7();

        assert_eq!(DestroyAttemptId::from_uuid(known).as_uuid(), known);
        assert_ne!(attempt.as_uuid(), known);
    }
}
