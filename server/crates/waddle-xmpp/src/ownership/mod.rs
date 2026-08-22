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
mod observe;
pub mod resume;

pub use in_process::InProcessClaimStore;
pub use observe::{observed_claim_store, ObservedClaimStore};
pub use resume::{verify_resume_identity, ResumeIdentityProof};

use async_trait::async_trait;
use std::time::Duration;

/// Closed set of claimable entity kinds (element 4). Serialized to `TEXT`
/// only at the SQL boundary (`entity_type` column) — never compared or
/// branched on as a bare string at call sites. Also `Serialize`/
/// `Deserialize` (ADR-0017 Phase 3 Slice 7) so [`Entity`] can travel
/// wire-typed on the MUC Demote relay ask, the same way `SmSessionId`/
/// `BareJid` already do on `RelayResumeSteal` — `serde`'s derived
/// representation, not the SQL `as_db_str` encoding, which stays the
/// SQL-boundary-only mapping its own doc comment describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

/// Wire length bound on [`Entity::id`] (ADR-0017 Phase 3 Slice 7 FIX 9,
/// council-adjudicated). Mirrors
/// [`crate::pending_delivery::SM_SESSION_ID_MAX_LEN`]'s defensive rationale one
/// type over: `Entity` now travels wire-typed on the MUC Demote relay ask
/// (deviation 60), which is NOT a stanza and so never passes through the
/// bounded XML stanza codec — without a field-level bound of its own, a
/// malicious (or buggy) allowlisted peer could ship a multi-MB `id`.
/// RFC 7622 section 3.1 permits 1023 octets in each bare-JID part, plus
/// the `@` separator, so the bound must admit every valid 2047-octet bare
/// JID used for `UserActor` and `RoomActor` ownership.
pub const ENTITY_ID_MAX_LEN: usize = 2047;

/// [`Entity::id`] exceeded [`ENTITY_ID_MAX_LEN`] at deserialization.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Entity id of {len} bytes exceeds the {ENTITY_ID_MAX_LEN}-byte wire bound")]
pub struct EntityIdTooLong {
    pub len: usize,
}

/// A claimable entity: its kind plus its non-secret identifier (a bare JID
/// for `UserActor`, a room JID for `RoomActor`, the SM-ID for `SmSession`).
/// Typed, never a bare `String` at call sites (typed-payloads hard rule).
/// `Serialize`/`Deserialize` (ADR-0017 Phase 3 Slice 7) so this type can
/// travel wire-typed on the MUC Demote relay ask. `Deserialize` is
/// hand-written (below), not derived, so it can enforce [`ENTITY_ID_MAX_LEN`]
/// on the `id` field — applying [`crate::pending_delivery::SmSessionId`]'s
/// same boundary defense one type over (FIX 9). [`Self::new`]
/// itself stays unvalidated, for the same reason `SmSessionId::new` does: the
/// wire boundary is where an untrusted length actually arrives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
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

/// Shadow of [`Entity`]'s wire shape used only to obtain a derived
/// [`serde::Deserialize`] for the two fields, so the hand-written impl below
/// does not have to hand-parse `entity_type` itself — only add the
/// [`ENTITY_ID_MAX_LEN`] check `derive` cannot express.
#[derive(serde::Deserialize)]
struct RawEntity {
    entity_type: EntityType,
    id: String,
}

impl<'de> serde::Deserialize<'de> for Entity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawEntity::deserialize(deserializer)?;
        if raw.id.len() > ENTITY_ID_MAX_LEN {
            return Err(serde::de::Error::custom(EntityIdTooLong {
                len: raw.id.len(),
            }));
        }
        Ok(Self {
            entity_type: raw.entity_type,
            id: raw.id,
        })
    }
}

/// `<entity_type_tag>:<id>` — the same injective encoding
/// `waddle-server::clustering::claims::entity_key` uses for the
/// `clustering_claims.entity` primary key. Giving `Entity` its own
/// `Display` (FIX 2) lets error types embed a typed [`Entity`] field
/// directly instead of pre-formatting it into a bare `String` at the
/// construction site — the typed-payloads hard rule's whole point: the
/// structured value flows through, and only `Display`/error-rendering
/// ever turns it into text.
impl std::fmt::Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.entity_type.as_db_str(), self.id)
    }
}

/// A read-only snapshot of an entity's current claim (ADR-0017 Phase 3
/// Slice 6): who owns it and at which epoch. Returned by
/// [`ClaimStore::current_claim`] — typed, never a bare tuple, per the
/// typed-payloads hard rule.
///
/// `owner_lease_fresh` (council-adjudicated fix, Slice 6): whether `owner`'s
/// own node-liveness row is currently fresh (not committed-`expired`, and its
/// `node_epoch` still matches) — the exact same owner-stale predicate
/// [`StalePredicate::OwnerStale`] realizes for `steal_stale`, read here
/// advisory-only (no write attached, same "never itself an authority"
/// caveat as the rest of this snapshot). This is what lets a cross-node
/// resume attempt distinguish, once its held-response window closes,
/// between "the owner is still alive, just unreachable over the swarm right
/// now" (XEP-0198's `resource-constraint`) and "the owner's own lease has
/// since expired" (XEP-0198's `item-not-found` — the session is known
/// gone). [`InProcessClaimStore`] has no node-liveness table at all (module
/// doc: "no `clustering_nodes`-equivalent liveness table here"), so it
/// always reports `true` — the single-node case has no notion of a stale
/// peer to distinguish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSnapshot {
    pub owner: NodeIdentity,
    pub claim_epoch: ClaimEpoch,
    pub owner_lease_fresh: bool,
}

/// A claim's fencing generation. Every successful acquire/steal bumps this;
/// a durable write's fencing check compares the epoch it was granted against
/// the epoch currently on file, so a stale epoch can never authorize a
/// write. `Serialize`/`Deserialize` (ADR-0017 Phase 3 Slice 7) so this type
/// can travel wire-typed on the MUC Demote relay ask.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
    authority: NodeAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NodeAuthority {
    Active,
    TerminallyDisabled,
}

impl NodeIdentity {
    pub fn new(node_id: impl Into<String>, node_epoch: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            node_epoch: node_epoch.into(),
            authority: NodeAuthority::Active,
        }
    }

    /// A fixed local identity for single-node/no-clustering deployments,
    /// where [`InProcessClaimStore`] is the only `ClaimStore` in play and
    /// node identity is not a meaningful concept (there is only one node).
    pub fn local() -> Self {
        Self::new("local", "local")
    }

    pub fn is_active(&self) -> bool {
        self.authority == NodeAuthority::Active
    }

    /// Whether both values identify the same process incarnation, regardless
    /// of this process's current publication authority. Terminal cleanup may
    /// still need to recognize and release rows written by the active form of
    /// an identity after [`SharedNodeIdentity::disable`] has revoked any new
    /// claim or publication authority.
    pub fn same_incarnation(&self, other: &Self) -> bool {
        self.node_id == other.node_id && self.node_epoch == other.node_epoch
    }
}

/// A live, shared view of this process's current [`NodeIdentity`].
///
/// Node identity is not fixed for the life of a process: ADR-0017 Phase 3
/// Slice 2's `self_fence::run_node_lease` mints a fresh `node_id`/
/// `node_epoch` pair every time this node re-registers after a self-fence,
/// reassigning its loop-local identity in place. Any call site that binds
/// an identity into a claim acquire/fence CAS on this node's behalf must
/// observe whatever identity is *currently* in force, not a snapshot
/// captured once at startup — a caller holding a stale, pre-fence identity
/// would silently keep acquiring/fencing claims under a `node_epoch` that
/// stopped being current the moment this node last self-fenced (ADR-0017
/// Phase 3 plan, Slice 4 follow-up plumbing note). This type is that
/// shared, updatable view: `self_fence::run_node_lease` calls [`Self::rotate`]
/// every time it mints a fresh identity, and any other holder of a clone
/// calls [`Self::current`] to read the latest value at the moment it
/// actually needs it (never earlier, never cached across an `.await`).
///
/// Declared here (unconditionally compiled), not in `waddle-server`'s
/// `clustering` module, so it is nameable from both the clustering
/// subsystem that updates it and any unconditionally-compiled consumer
/// (e.g. the Postgres-fenced `SmPersistenceStorage`, ADR-0017 Phase 3
/// Slice 4) that only ever reads it, without forcing the reader to depend
/// on the `clustering` Cargo feature.
#[derive(Clone)]
pub struct SharedNodeIdentity {
    identity: std::sync::Arc<std::sync::RwLock<NodeIdentity>>,
    rotation_gate: std::sync::Arc<tokio::sync::RwLock<()>>,
}

/// Shared authority to use one node identity at a publication or commit
/// boundary. Identity rotation takes the exclusive side of the same gate.
pub struct CurrentNodeIdentityGuard {
    identity: NodeIdentity,
    rotation_gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    _rotation_guard: tokio::sync::OwnedRwLockReadGuard<()>,
}

impl CurrentNodeIdentityGuard {
    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }
}

impl SharedNodeIdentity {
    pub fn new(initial: NodeIdentity) -> Self {
        Self {
            identity: std::sync::Arc::new(std::sync::RwLock::new(initial)),
            rotation_gate: std::sync::Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    /// The identity this node currently believes it holds, cloned out
    /// rather than returned by reference so callers never hold the lock
    /// across an `.await`.
    ///
    /// A poisoned lock (a panicking holder) still yields a usable
    /// identity: `NodeIdentity` has no invariant `rotate` can partially
    /// break (both fields are plain, independently-assigned `String`s),
    /// so recovering via the poison error's inner guard is safe and keeps
    /// one panicking holder from cascading into every future
    /// claim-fencing call this process ever makes.
    pub fn current(&self) -> NodeIdentity {
        self.identity
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Inspect the current incarnation while preventing `rotate` from changing
    /// it. The closure must remain synchronous and short; this guard exists
    /// for atomic in-memory lifecycle transitions, never backend I/O.
    pub fn with_current<R>(&self, inspect: impl FnOnce(&NodeIdentity) -> R) -> R {
        let identity = self
            .identity
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inspect(&identity)
    }

    /// Acquire authority to use `expected`. Once returned, rotation cannot
    /// complete until the guard is dropped. Writer preference also prevents
    /// a new stale guard from passing a rotation that has already started.
    pub async fn guard_if_current(
        &self,
        expected: &NodeIdentity,
    ) -> Option<CurrentNodeIdentityGuard> {
        let rotation_guard = self.rotation_gate.clone().read_owned().await;
        let identity = self.current();
        (identity.is_active() && identity == *expected).then_some(CurrentNodeIdentityGuard {
            identity,
            rotation_gate: self.rotation_gate.clone(),
            _rotation_guard: rotation_guard,
        })
    }

    /// Run a short synchronous demotion transition only after every current
    /// publication guard has drained. This uses the exclusive side of the
    /// same gate as identity rotation, so local claim removal cannot race a
    /// caller publishing state under [`CurrentNodeIdentityGuard`].
    pub async fn with_publications_blocked<R>(&self, demote: impl FnOnce() -> R) -> R {
        let _publication_guard = self.rotation_gate.write().await;
        demote()
    }

    /// Whether `guard` holds the read side of this exact identity source's
    /// rotation gate, not merely an equal node-id/incarnation value.
    pub fn owns_guard(&self, guard: &CurrentNodeIdentityGuard) -> bool {
        std::sync::Arc::ptr_eq(&self.rotation_gate, &guard.rotation_gate)
    }

    /// Replace the current identity only after every in-flight guarded
    /// publication or transaction has completed.
    pub async fn rotate(&self, identity: NodeIdentity) {
        let _rotation_guard = self.rotation_gate.write().await;
        *self
            .identity
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = identity;
    }

    /// Permanently revoke publication authority for this clustering
    /// lifetime after every in-flight guarded publication has drained.
    /// The last identity remains inspectable for diagnostics, but compares
    /// unequal to its former active value and cannot pass a claim acquire.
    pub async fn disable(&self) {
        let _rotation_guard = self.rotation_gate.write().await;
        self.identity
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authority = NodeAuthority::TerminallyDisabled;
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

    /// A `steal_stale` / `steal_for_resume` CAS affected zero
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

    /// `acquire`/`ensure_claimed`/`steal_stale` refused to grant a NEW
    /// claim because the calling node has marked itself draining
    /// (ADR-0017 Phase 3 Slice 10: `NodeLeaseStore::mark_draining` — "stop
    /// acquiring new claims, keep serving already-owned ones"). Distinct
    /// from [`ClaimError::AlreadyClaimed`]: the entity may well be
    /// unclaimed — this node simply refuses to be the one to claim it while
    /// on its way out. A caller already holding this exact claim is
    /// unaffected (`ensure_claimed`'s self-reacquire fallback still
    /// succeeds while draining); only a genuinely NEW acquisition is
    /// refused.
    #[error("this node is draining and refuses to acquire a new claim")]
    Draining,

    /// The process terminally revoked its shared node identity after a
    /// fatal ownership ambiguity. No new or self-reacquired claim may be
    /// published under that identity.
    #[error("this node identity is terminally disabled")]
    AuthorityDisabled,
}

/// Result of an ownership- and epoch-exact release attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactReleaseOutcome {
    Released,
    NotOwned,
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
/// durable MUC writes — all later slices) issues its own
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

    /// Idempotent, self-reacquiring claim ensure (ADR-0017 Phase 3, FIX 1 —
    /// council-adjudicated). Attempts a fresh [`acquire`](Self::acquire);
    /// when that loses the CAS race with [`ClaimError::AlreadyClaimed`],
    /// reads the entity's existing claim row: if it is already held by `me`
    /// (the same `node_id` **and** `node_epoch`), this is a *self*-reacquire
    /// — return the row's current [`ClaimEpoch`] instead of erroring, since
    /// re-observing a claim this exact node/epoch already holds is
    /// idempotent, not a conflict. If a genuinely different node/epoch holds
    /// it, this returns [`ClaimError::AlreadyClaimed`] exactly as `acquire`
    /// would.
    ///
    /// This is what the Postgres-fenced `SmPersistenceStorage`'s
    /// first-fenced-write-per-stream path (ADR-0017 Phase 3 Slice 4) calls
    /// instead of a bare `acquire`, layered under a caller-side per-key
    /// single-flight (see `sm_persistence_fenced::claim_epoch_for`) so that:
    /// (a) two concurrent first writes for the same not-yet-claimed
    /// stream_id never spuriously conflict with each other (both observe
    /// `me`'s own successful acquire, one via the CAS, the other via this
    /// self-reacquire read), and (b) a later slice's `<enable/>`-time
    /// acquire — which creates the claims row under this node's own
    /// identity before this path's first fenced write ever runs — is
    /// observed here as a self-reacquire rather than an error (deviation
    /// 26), closing the Slice 5/6 self-lock the bare-`acquire` design would
    /// otherwise hit.
    ///
    /// The row read on conflict is deliberately **unlocked**: it only ever
    /// decides which [`ClaimEpoch`] value to hand back to the caller for a
    /// *later*, separate fencing check to bind — it is never itself the
    /// authority over whether any write may proceed. The authoritative gate
    /// over every actual write remains the per-write `SELECT ... FOR SHARE`
    /// fence inside the write's own transaction (Slice 4's `assert_fenced`).
    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError>;

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

    /// Read-only lookup of `entity`'s current claim, if any (ADR-0017 Phase 3
    /// Slice 6 addition — deviation, see the phase plan). Unlocked, exactly
    /// like [`ensure_claimed`](Self::ensure_claimed)'s own conflict-path read:
    /// it never authorizes a write by itself, it only tells a caller who to
    /// ask/steal from. The cross-node XEP-0198 resume path uses this to
    /// decide which of the three resume branches applies (claim absent or
    /// self-owned → today's local path; owned by another node → the
    /// detached-vs-live branch, itself decided by whether a persisted
    /// snapshot already exists) and to learn the observed [`ClaimEpoch`] to
    /// bind into a subsequent [`steal_for_resume`](Self::steal_for_resume)
    /// call.
    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError>;

    /// Observe a claim only after any detached write already mutating that
    /// claim row has committed or rolled back. Terminal recovery uses this
    /// as an ordering barrier after cancellation may have dropped a steal
    /// future while its backend statement remained in flight.
    ///
    /// In-process stores execute claim mutations synchronously and therefore
    /// need no stronger operation than [`Self::current_claim`]. Stores that
    /// can outlive a dropped future must override this method with a row-lock
    /// or equivalent serialization barrier.
    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.current_claim(entity).await
    }

    /// Advisory, own-transaction check: does `me` still hold `entity` under
    /// epoch `mine` right now? See the trait-level doc for why this is
    /// never the write-path fencing mechanism.
    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError>;

    /// Best-effort, idempotent release of a held claim. A missing, stolen,
    /// foreign-incarnation, or stale-epoch row is already terminal cleanup
    /// and therefore succeeds.
    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError>;

    /// Release only the exact owner incarnation and epoch, reporting whether
    /// a row was actually deleted. Recovery workflows use this when a zero-row
    /// no-op must not be mistaken for confirmed ownership cleanup.
    async fn release_exact(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        Err(ClaimError::Backend(
            "exact release outcome is not implemented by this claim store".to_string(),
        ))
    }

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

/// Rollout-aware claim-acquisition placement (ADR-0017 Phase 3 Slice 10,
/// Q5's mechanism): how long to wait before attempting to steal a claim
/// from a dead owner. Unconditionally compiled (like [`ClaimStore`] itself)
/// so `RoomRegistry`'s re-election path (`steal_from_dead_owner`) can call
/// it without depending on `waddle-server`'s `clustering`-feature-gated
/// `NodeLeaseStore`, which is where the real `pod_template_hash`/
/// `current_generation` comparison lives. `None` (the default on every
/// construction site until wired) means "no backoff" — correct for every
/// single-node deployment and every existing test, which never wire this
/// field at all.
///
/// **Never affects correctness.** This is purely a placement heuristic —
/// it decides who *tries first*, never who *wins*: the claims CAS remains
/// the sole authority over who actually holds any given entity. A missing
/// or unwired implementation costs nothing worse than every node trying to
/// steal a dead owner's claim at the same instant, exactly today's
/// pre-Slice-10 behavior.
#[async_trait]
pub trait RolloutBackoff: Send + Sync {
    /// How long to wait before this node's next claim-steal attempt.
    async fn acquire_delay(&self) -> Duration;
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

    /// ADR-0017 Phase 3 Slice 7 FIX 9 (council-adjudicated): the wire
    /// -deserialize bound on `Entity::id`, mirroring
    /// `SmSessionId`'s identical boundary tests one type over.
    #[test]
    fn entity_deserialize_accepts_the_boundary_length() {
        let id = "a".repeat(ENTITY_ID_MAX_LEN);
        let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
        let entity: Entity =
            serde_json::from_str(&json).expect("exactly at the cap must deserialize");
        assert_eq!(entity.id.len(), ENTITY_ID_MAX_LEN);
    }

    #[test]
    fn entity_deserialize_rejects_one_byte_over_the_cap() {
        let id = "a".repeat(ENTITY_ID_MAX_LEN + 1);
        let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
        let error = serde_json::from_str::<Entity>(&json)
            .expect_err("one byte over the cap must be rejected");
        assert!(
            error.to_string().contains("exceeds"),
            "unexpected error message: {error}"
        );
    }

    #[test]
    fn entity_deserialize_rejects_a_malicious_multi_kb_id() {
        let id = "x".repeat(64 * 1024);
        let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
        assert!(serde_json::from_str::<Entity>(&json).is_err());
    }

    #[test]
    fn node_identity_local_is_stable() {
        assert_eq!(NodeIdentity::local(), NodeIdentity::local());
    }

    #[test]
    fn claim_epoch_orders_by_value() {
        assert!(ClaimEpoch(0) < ClaimEpoch(1));
    }

    #[tokio::test]
    async fn shared_node_identity_rotation_is_visible_through_every_clone() {
        let shared = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-0"));
        let clone = shared.clone();
        assert_eq!(clone.current(), NodeIdentity::new("node-a", "epoch-0"));

        shared.rotate(NodeIdentity::new("node-a", "epoch-1")).await;

        assert_eq!(clone.current(), NodeIdentity::new("node-a", "epoch-1"));
        assert_eq!(shared.current(), NodeIdentity::new("node-a", "epoch-1"));
    }

    #[tokio::test]
    async fn terminally_disabled_identity_rejects_guards_and_claim_acquisition() {
        let active = NodeIdentity::new("node-a", "epoch-0");
        let shared = SharedNodeIdentity::new(active.clone());
        shared.disable().await;

        let disabled = shared.current();
        assert!(!disabled.is_active());
        assert_ne!(disabled, active);
        assert!(disabled.same_incarnation(&active));
        assert!(!disabled.same_incarnation(&NodeIdentity::new("node-a", "epoch-1")));
        assert!(shared.guard_if_current(&disabled).await.is_none());

        let store = InProcessClaimStore::new();
        let entity = Entity::new(EntityType::SmSession, "disabled-owner");
        assert!(matches!(
            store.acquire(&entity, &disabled).await,
            Err(ClaimError::AuthorityDisabled)
        ));
    }

    #[tokio::test]
    async fn guarded_identity_boundary_delays_rotation_until_use_completes() {
        let old = NodeIdentity::new("node-a", "epoch-0");
        let new = NodeIdentity::new("node-a", "epoch-1");
        let shared = SharedNodeIdentity::new(old.clone());
        let guard = shared
            .guard_if_current(&old)
            .await
            .expect("old identity starts current");
        let rotating = shared.clone();
        let rotation = tokio::spawn(async move {
            rotating.rotate(new).await;
        });

        tokio::task::yield_now().await;
        assert!(
            !rotation.is_finished(),
            "rotation must wait at the exact post-check/use boundary"
        );
        assert_eq!(guard.identity(), &old);
        drop(guard);

        rotation.await.expect("rotation task");
        assert_eq!(shared.current(), NodeIdentity::new("node-a", "epoch-1"));
        assert!(shared.guard_if_current(&old).await.is_none());
    }
}
