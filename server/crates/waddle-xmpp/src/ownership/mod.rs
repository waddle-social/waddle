//! Entity ownership claims (ADR-0017 Phase 3 Slice 1, element 4).
//!
//! Every claimed entity — a `UserActor`, a `RoomActor`, or a XEP-0198 SM
//! session — needs a real, epoch-fenced owner record so a cluster of nodes
//! can agree on exactly one live owner at a time. This module defines that
//! abstraction (the [`ClaimStore`] trait) plus the typed value types every
//! implementation and caller shares.
//!
//! **Unconditionally compiled — no `clustering` Cargo feature gate.**
//! `UserActor`/`RoomActor`/SM session code in this crate has no `clustering`
//! feature and must keep working (and keep needing *some* `ClaimStore`) in
//! every build, including a plain single-node SQLite deployment that never
//! touches Postgres. Gating the trait itself behind `clustering` would force
//! ordinary session/registry code to grow two code paths for "clustering
//! compiled" vs. not — exactly the kind of scattered conditional the ADR's
//! element-1 text forbids.
//!
//! - [`InProcessClaimStore`] (in [`in_process`]) is the trivial single-node
//!   implementation: there is only one node, so the contract is identical
//!   to the Postgres impl's — acquire succeeds iff the entity is
//!   currently unclaimed — but same-node contention (two connections
//!   racing to claim the same entity) is real and enforced, not a
//!   no-op.
//! - The Postgres CAS implementation lives downstream, in
//!   `waddle-server::clustering::claims::PostgresClaimStore`
//!   (`#[cfg(feature = "clustering")]`), implementing this crate's
//!   [`ClaimStore`] trait for a `waddle-server`-local type — legal under
//!   Rust's orphan rule (local type, foreign trait) without the trait
//!   itself needing to live downstream.
//! - [`resume`] defines [`ResumeIdentityProof`] and hosts its sole
//!   constructor — see that type's docs for why the consent-CAS steal
//!   path is compiler-enforced, not merely conventional. The type is
//!   deliberately declared inside `resume.rs`, not here: Rust's privacy
//!   rule extends a private field's visibility to the defining module's
//!   *descendants*, never to its ancestors or siblings, so defining it in
//!   `mod.rs` itself would let every sibling submodule (e.g.
//!   [`in_process`]) construct one directly — exactly the loophole moving
//!   the type down into `resume` closes.

pub mod in_process;
pub mod resume;

pub use in_process::InProcessClaimStore;
pub use resume::{verify_resume_identity, ResumeIdentityProof};

use async_trait::async_trait;
use std::time::Duration;

/// Closed set of claimable entity kinds (element 4). Serialized to `TEXT`
/// only at the SQL boundary (`entity_type` column) — never compared or
/// branched on as a bare string at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    UserActor,
    RoomActor,
    SmSession,
}

impl EntityType {
    /// The stable `TEXT` representation stored in `clustering_claims.entity_type`.
    /// This is the *only* place the enum touches a bare string.
    pub fn as_db_str(self) -> &'static str {
        match self {
            EntityType::UserActor => "user_actor",
            EntityType::RoomActor => "room_actor",
            EntityType::SmSession => "sm_session",
        }
    }

    /// Parse the stable `TEXT` representation back into the typed enum.
    /// Returns `None` for any value that isn't one of the three known
    /// strings — callers reading claims rows should treat that as a
    /// decode failure, not silently coerce to a default variant.
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "user_actor" => Some(EntityType::UserActor),
            "room_actor" => Some(EntityType::RoomActor),
            "sm_session" => Some(EntityType::SmSession),
            _ => None,
        }
    }
}

/// A claimable entity: its kind plus its non-secret identifier (a bare JID
/// for `UserActor`, a room JID for `RoomActor`, the SM-ID for `SmSession`).
/// Typed, never a bare `String` at call sites (typed-payloads hard rule).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entity {
    pub entity_type: EntityType,
    pub id: String,
}

impl Entity {
    pub fn new(entity_type: EntityType, id: impl Into<String>) -> Self {
        Self {
            entity_type,
            id: id.into(),
        }
    }
}

/// A claim's fencing generation. Every successful acquire/steal bumps this;
/// a durable write's fencing check compares the epoch it was granted against
/// the epoch currently on file, so a stale epoch can never authorize a
/// write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaimEpoch(pub i64);

/// A node's cluster identity, as seen by the claims CAS. Distinct from
/// `waddle-server::clustering::NodeId` (which is `clustering`-feature gated
/// and libp2p-flavored): this type is unconditionally compiled, so it is
/// plain data with no dependency on the swarm subsystem. `node_epoch` is
/// freshly generated on every process start and never reused, mirroring the
/// keypair-slot lease's `LeaseIdentity` (ADR-0017 element 3/4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeIdentity {
    pub node_id: String,
    pub node_epoch: String,
}

impl NodeIdentity {
    pub fn new(node_id: impl Into<String>, node_epoch: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            node_epoch: node_epoch.into(),
        }
    }

    /// A fixed local identity for single-node/no-clustering deployments,
    /// where [`InProcessClaimStore`] is the only `ClaimStore` in play and
    /// node identity is not a meaningful concept (there is only one node).
    pub fn local() -> Self {
        Self::new("local", "local")
    }
}

/// Closed enum of staleness sources `steal_stale` accepts — not a
/// free-form predicate builder, so a caller cannot invent a third
/// staleness definition at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalePredicate {
    /// The owner-stale predicate (element 4): the owning node's
    /// `clustering_nodes` row is missing entirely, its committed `expired`
    /// flag is set, or its `node_epoch` no longer matches — realized as a
    /// `NOT EXISTS` correlated subquery, never a raw `heartbeat < now() -
    /// ttl` comparison (see `waddle-server::clustering::claims::PostgresClaimStore::steal_stale`
    /// for the exact SQL).
    OwnerStale,
    /// The steal-intent predicate (ADR-0017 Phase 3 Slice 3): `EXISTS
    /// (SELECT 1 FROM steal_intents WHERE entity=$e AND created_at < now()
    /// - $intent_ttl)`. Realized by
    /// `waddle-server::clustering::claims::PostgresClaimStore::steal_stale`.
    /// Per the three-rule steal-variant block (ADR-0017 Phase 3 plan, Slice
    /// 3), this variant never applies to `EntityType::SmSession` claims —
    /// implementations reject that combination with
    /// [`ClaimError::SmSessionExcludedFromStealIntent`].
    StealIntentExpired { intent_ttl: Duration },
}

/// `ClaimStore` failures. Typed per the repo's typed-payloads hard rule —
/// never a bare `String` masquerading as structured data. [`ClaimError::Backend`]
/// is the one necessary exception: `ClaimStore` lives in `waddle-xmpp`, which
/// cannot name a `waddle-server` error type (that would be an illegal reverse
/// dependency), so a Postgres-backed implementation's own richly-typed error
/// enum is converted to its `Display` text at this boundary — a human-facing
/// diagnostic, not a structured payload.
#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    /// The backing store's own error, converted to its `Display` text.
    #[error("claim store backend error: {0}")]
    Backend(String),

    /// `acquire` lost the `INSERT ... ON CONFLICT DO NOTHING` race: another
    /// node already holds this entity.
    #[error("entity already claimed by another node")]
    AlreadyClaimed,

    /// A `steal_stale` / `steal_for_resume` / `release` CAS affected zero
    /// rows: the observed epoch was stale, the staleness predicate was not
    /// satisfied (the owner is not actually stale), or the claim no longer
    /// exists.
    #[error("claim CAS lost the race (stale epoch, fresh owner, or claim gone)")]
    Conflict,

    /// In-process bookkeeping lock was poisoned by a panicking holder.
    #[error("claim store internal lock poisoned")]
    Poisoned,

    /// A steal-intent operation (`report_steal_intent`/`owner_steal_intents`/
    /// `clear_steal_intent`, ADR-0017 Phase 3 Slice 3) was asked to operate
    /// on an `EntityType::SmSession` claim. Per the three-rule steal-variant
    /// block (Slice 3 of the phase plan): steal-intents never touch
    /// SM-session claims — those are stolen exclusively via
    /// `steal_for_resume` (identity-bound resume, element 8) or, for
    /// dead-owner garbage collection, `steal_stale`'s `OwnerStale` predicate
    /// via the orphan reaper (Slice 5). Typed rejection, not a silently
    /// accepted no-op, so a caller cannot mistake this for "intent recorded."
    #[error(
        "SM-session claims are excluded from the steal-intent path (use steal_for_resume, \
         or the orphan reaper's OwnerStale predicate, instead)"
    )]
    SmSessionExcludedFromStealIntent,
}

/// Entity ownership claims: which node currently owns a given `UserActor`/
/// `RoomActor`/SM-session entity, under which fencing epoch.
///
/// Node heartbeat/expire/demotion-reconciliation is a **separate**,
/// per-node concern (a future `NodeLeaseStore`-style trait, ADR-0017 Phase 3
/// Slice 2) — not part of this trait, matching the ADR's own "heartbeats
/// are per node, not per entity" framing.
///
/// [`fence`](ClaimStore::fence) is **advisory-only, never the write-path
/// fencing mechanism**: it opens its own transaction on its own connection
/// and answers "do I still hold this claim right now," useful for a caller
/// like a health-ask handler that wants a point-in-time answer with no write
/// attached. Every fenced write (the Postgres-fenced `SmPersistenceStorage`,
/// durable MUC writes, ISR consume — all later slices) issues its own
/// inline `SELECT ... FOR SHARE` on its own `Database::begin()` transaction
/// instead of calling `fence()`, because the fencing lock and the write it
/// guards must share one connection/one transaction — a lock taken on a
/// *different* connection than the write protects nothing. `fence()` exists
/// purely for non-write-path callers that want an advisory point-in-time
/// check.
#[async_trait]
pub trait ClaimStore: Send + Sync {
    /// Create the backing schema if it does not exist. Idempotent.
    async fn ensure_schema(&self) -> Result<(), ClaimError>;

    /// Acquire a fresh claim on `entity` for `me`. Fails with
    /// [`ClaimError::AlreadyClaimed`] if another node already holds it.
    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError>;

    /// Steal `entity` from a stale owner (element 4's owner-stale predicate,
    /// or — from Slice 3 — the steal-intent predicate). `observed` must
    /// match the claim's current epoch or the CAS loses the race
    /// ([`ClaimError::Conflict`]).
    ///
    /// This is deliberately a *different* method from
    /// [`steal_for_resume`](Self::steal_for_resume): it cannot express the
    /// no-staleness (consent-only) CAS variant, so a caller here can never
    /// displace a fresh-lease owner without going through the identity-bound
    /// resume path.
    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError>;

    /// Steal `entity` via the consent/epoch-only CAS (element 4's third CAS
    /// variant), authorized exclusively by an identity-checked resume
    /// (element 8). Requires a [`ResumeIdentityProof`], which only
    /// [`resume::verify_resume_identity`] can mint — so no caller outside
    /// the resume path can even name this method meaningfully.
    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError>;

    /// Advisory, own-transaction check: does `me` still hold `entity` under
    /// epoch `mine` right now? See the trait-level doc for why this is
    /// never the write-path fencing mechanism.
    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError>;

    /// Release a held claim (epoch-gated, best-effort: releasing a claim
    /// already stolen out from under `me` is a no-op, not an error).
    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError>;

    /// Batched release for graceful drain (~18k modeled claims, ADR-0017
    /// Phase 3 Slice 10) — one round-trip, not one-at-a-time. Releases every
    /// entity in `entities` currently held by `me`, regardless of each
    /// entity's individual epoch (drain does not need per-entity epoch
    /// pinning — only "still owned by me, whatever the epoch").
    ///
    /// **Plan-sanctioned ABA window**: this release is blind to `entity`'s
    /// individual `claim_epoch`, matching only on `(node_id, node_epoch)` —
    /// so if an entity queued into `entities` (because its final write
    /// already committed) is *re-claimed by this same node* at a higher
    /// epoch before the batched DELETE actually runs (e.g. a resumed
    /// XEP-0198 session legitimately steals back onto this node via
    /// `steal_for_resume` — no staleness required — while this node is
    /// still draining but not yet gone), the stale batch entry deletes that
    /// brand-new, genuinely-live claim too. Slice 2's draining-node marker
    /// (`NodeLeaseStore::mark_draining` stops this node from *acquiring*
    /// new claims once draining, narrowing but not eliminating the window)
    /// and Slice 10's batch-construction ordering (an entity enters the
    /// batch only after its final fenced write's transaction has committed,
    /// keeping the window as short as possible) are the mitigations — not a
    /// full closure of the race. See Slice 10's Tests paragraph for the
    /// interleaving this implies for the drain test suite.
    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_db_str_round_trips() {
        for variant in [
            EntityType::UserActor,
            EntityType::RoomActor,
            EntityType::SmSession,
        ] {
            assert_eq!(EntityType::from_db_str(variant.as_db_str()), Some(variant));
        }
    }

    #[test]
    fn entity_type_from_db_str_rejects_unknown_values() {
        assert_eq!(EntityType::from_db_str("not-a-real-entity-type"), None);
    }

    #[test]
    fn node_identity_local_is_stable() {
        assert_eq!(NodeIdentity::local(), NodeIdentity::local());
    }

    #[test]
    fn claim_epoch_orders_by_value() {
        assert!(ClaimEpoch(0) < ClaimEpoch(1));
    }
}
