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

/// The lifecycle-local ordinal of one post-commit effect. Together with a
/// [`RoomLifecycleId`] and [`RoomRevision`], this is the third component of
/// #1646's effect-outbox key and preserves FIFO ordering within that exact
/// `(lifecycle, revision)` coordinate.
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

/// Proof returned by #1645's durable-commit path before any in-memory apply.
/// It records that this room's authoritative state committed at one
/// `(lifecycle, revision)` under this exact claim fence, so subsequent work
/// cannot accidentally treat an unfenced or stale mutation as authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMutationCommit {
    fence: RoomClaimFenceContext,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
}

impl RoomMutationCommit {
    pub fn new(
        fence: RoomClaimFenceContext,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
    ) -> Self {
        Self {
            fence,
            lifecycle,
            revision,
        }
    }

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

/// A transient, one-use capability authorizing an ephemeral projection after
/// a durable commit (#1647). Only the post-durable-commit path mints it; #1645
/// wires that real minting site. On a crash or claim loss this capability is
/// simply dropped and #1647 resynchronizes the projection.
///
/// It is deliberately neither `Clone` nor `Copy` and is never serialized:
/// safe Rust's compile-time linearity enforces one use because [`Self::consume`]
/// takes ownership, making a second consumption unrepresentable.
#[must_use = "the authorization must be consumed to project its durable commit"]
#[derive(Debug)]
pub struct EphemeralProjectionAuthorization {
    commit: RoomMutationCommit,
}

impl EphemeralProjectionAuthorization {
    /// Mint an authorization only after the supplied durable commit has
    /// succeeded; #1645 owns wiring the production minting site.
    pub fn new(commit: RoomMutationCommit) -> Self {
        Self { commit }
    }

    /// Consume this one-use capability and recover the durable-commit proof
    /// that authorizes the ephemeral projection.
    pub fn consume(self) -> RoomMutationCommit {
        self.commit
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
        let commit = RoomMutationCommit::new(fence.clone(), lifecycle, revision);

        assert_eq!(commit.fence(), &fence);
        assert_eq!(commit.lifecycle(), lifecycle);
        assert_eq!(commit.revision(), revision);
    }

    #[test]
    fn authorization_consumes_the_exact_commit() {
        let commit = RoomMutationCommit::new(
            claim_fence(),
            RoomLifecycleId::generate(),
            RoomRevision::initial(),
        );
        let expected = commit.clone();

        assert_eq!(
            EphemeralProjectionAuthorization::new(commit).consume(),
            expected
        );
    }
}
