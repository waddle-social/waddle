//! Durable MUC room ownership state (ADR-0017 Phase 3 Slice 7, element 7).
//!
//! Room configuration, the affiliation list, and the current subject are —
//! before this slice — purely in-memory (`MucRoom`/`AffiliationList`'s own
//! doc comments say so explicitly). A takeover that rematerialized a
//! default `MucRoom` would silently drop ban lists, member lists,
//! passwords, config, and subject on every ownership move (element 7's
//! locked spec). This module defines the dyn-compatible seam a freshly
//! spawned/claimed [`super::room_actor::RoomActor`] uses to **restore
//! configuration, affiliations, and subject from Postgres before accepting
//! any join** (the element-7 locked text, quoted verbatim), and to persist
//! the same three pieces on every change thereafter.
//!
//! Dyn-compatible (boxed futures, not RPITIT) for the same reason as
//! [`super::affiliation::DurableMembershipSource`]: the room registry holds
//! it as `Arc<dyn MucDurableStore>` and forwards it to each freshly spawned
//! `RoomActor`. `None` in deployments without clustering enabled (or
//! without a Postgres backend) — such deployments keep today's purely
//! in-memory room state, byte-identical to pre-Slice-7 behavior (ADR-0017
//! element 1's "Postgres-only for multi-replica" — clustering, and
//! therefore this durability layer, is gated on the Postgres backend).
//!
//! **Scope note (deviation, recorded in the phase plan)**: occupant-roster
//! durability (real JID/nick/occupant-id) is NOT part of this trait. Its
//! sole consumer — a surviving node synthesizing XEP-0045 unavailable
//! presence for occupants it did not itself observe joining — needs
//! cross-node presence fan-out (ADR-0017 element 11), which this phase's
//! Non-goals exclude. Landing occupant-roster schema now with no reader
//! would be schema-only churn; deferred to the slice that wires that
//! cross-node consumer.

use std::future::Future;
use std::pin::Pin;

use jid::BareJid;

use super::affiliation::AffiliationEntry;
use super::{RoomConfig, SubjectState};
use crate::ownership::{ClaimEpoch, Entity, NodeIdentity};
use crate::XmppError;

/// Boxed future returned by every [`MucDurableStore`] method, mirroring
/// [`super::affiliation::DurableMembershipFuture`]'s exact shape and
/// rationale.
pub type MucDurableFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, XmppError>> + Send + 'a>>;

/// Typed fencing context for a room's currently-recorded Postgres claim
/// (ADR-0017 Phase 3 Slice 7 FIX 1, council-adjudicated): the same
/// `(Entity, ClaimEpoch, NodeIdentity)` tuple [`MucDurableStore::check_fenced_fanout`]
/// resolves internally via [`MucDurableStore::record_claim_fence`]'s cache,
/// exposed here so a caller that needs to run its OWN fenced write against a
/// DIFFERENT store (MAM's `store_message_fenced`, which cannot share a SQL
/// transaction with this store's own `assert_fenced`) can bind the identical
/// typed values into its own `SELECT ... FOR SHARE` check, rather than
/// re-deriving them from a second, independent source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomClaimFenceContext {
    pub entity: Entity,
    pub epoch: ClaimEpoch,
    pub owner: NodeIdentity,
}

impl RoomClaimFenceContext {
    pub fn new(entity: Entity, owner: NodeIdentity, epoch: ClaimEpoch) -> Self {
        Self {
            entity,
            epoch,
            owner,
        }
    }

    pub fn owner(&self) -> NodeIdentity {
        self.owner.clone()
    }
}

/// Full durable snapshot of a room's long-lived state: configuration,
/// affiliation list, and subject — element 7's locked restore-before-join
/// text names exactly these three as what the new owner restores before
/// accepting any join.
#[derive(Debug, Clone)]
pub struct DurableRoomState {
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
    pub subject: Option<SubjectState>,
    pub affiliations: Vec<AffiliationEntry>,
}

/// Durable backing store for MUC room ownership (ADR-0017 Phase 3 Slice 7).
///
/// Every write is epoch-fenced against this node's currently-recorded claim
/// on the room's `RoomActor` entity (see [`Self::record_claim_fence`]) —
/// "epoch-fenced like all claimed-entity writes" per element 7.
///
/// **Fail-open contract, revised (ADR-0017 Phase 3 Slice 7 FIX 2,
/// council-adjudicated).** This trait's `save_*` methods themselves are
/// still fire-and-forget from a Rust-signature perspective (`Result<(),
/// XmppError>`, not surfaced up through every intermediate layer) — a
/// fenced write that fails still just returns `Err` to its immediate
/// caller rather than panicking or blocking. What changed is what
/// [`super::room_actor::RoomActor`] itself does with that `Err`, for every
/// mutation handler that changes durable-relevant state (`UpdateConfig`,
/// `RollbackConfigIfRevision`, `UpdateGroupDmConfigByMember`, `SetSubject`,
/// `ChangeAffiliation`, `ApplyAdminItems`, `ApplyAffiliationChange`,
/// `EnforceMembersOnlyAffiliations`, `ReconcileChannelBackedRoom`):
///
/// 1. **Before mutating**: the handler runs
///    [`super::room_actor::RoomActor::gate_mutation`] — a `SELECT ... FOR
///    SHARE`-fenced [`Self::check_fenced_fanout`] pre-check — and refuses
///    to mutate at all on a definitive ownership-loss result. This is new:
///    previously every mutation applied in-memory unconditionally,
///    regardless of whether this node's claim was still current.
/// 2. **After mutating**: a `save_*` failure that is NOT ownership loss
///    (a transient backend outage) is no longer silently logged and
///    swallowed — it now surfaces as a typed error
///    (`RoomMutationError::PersistFailed`/the per-message error enum's
///    equivalent variant) to the ask's caller, which is expected to
///    trigger `RoomLocalClaims::demote` (on ownership loss) and/or report
///    the failure onward rather than treating bare `Ok` as durable
///    convergence.
///
/// Single-node/non-clustering deployments are unaffected: no
/// `MucDurableStore` is configured there at all, so `gate_mutation`'s
/// `None`-store branch always returns `Ok(())` and `save_*` is never
/// called — today's purely in-memory behavior, byte-identical.
pub trait MucDurableStore: Send + Sync {
    /// Load `room_jid`'s durable state, if a row exists. `None` for a room
    /// that has never been durably written (e.g. a brand-new persistent
    /// room's very first spawn on any node, or a non-persistent instant
    /// room, which is never durably written at all).
    fn load_room_state<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>>;

    /// Load one authoritative room snapshot while proving that this store's
    /// recorded claim fence is still current in the same database
    /// transaction. Implementations must not split the ownership check and
    /// snapshot read across transactions: a claim steal between those reads
    /// would let a deposed actor install state owned by its successor.
    ///
    /// The default fails closed. Clustered stores must opt in explicitly;
    /// in-memory test stores may delegate to [`Self::load_room_state`] when
    /// their ownership model is controlled synchronously by the test.
    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        let _ = room_jid;
        Box::pin(async {
            Err(XmppError::internal(
                "durable store does not implement fenced room-state loading",
            ))
        })
    }

    /// Durably upsert the room's configuration (plus the `waddle_id`/
    /// `channel_id` it travels with).
    fn save_config<'a>(
        &'a self,
        room_jid: &'a BareJid,
        waddle_id: &'a str,
        channel_id: &'a str,
        config: &'a RoomConfig,
    ) -> MucDurableFuture<'a, ()>;

    /// Durably upsert (or, when `subject` is `None`, clear) the room's
    /// current subject.
    fn save_subject<'a>(
        &'a self,
        room_jid: &'a BareJid,
        subject: Option<&'a SubjectState>,
    ) -> MucDurableFuture<'a, ()>;

    /// Durably upsert one affiliation-list entry. `Affiliation::None`
    /// removes the row, mirroring `AffiliationList::set`'s in-memory
    /// contract.
    fn save_affiliation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        entry: &'a AffiliationEntry,
    ) -> MucDurableFuture<'a, ()>;

    /// XEP-0045 §10.9 (#1261): destroy removes the room "even if it was
    /// defined as persistent". Delete every durable row for `room_jid`
    /// — config, subject, and the full affiliation list — so a
    /// destroyed room can never resurrect from storage (with its old
    /// config, subject, or ban list) on the next join. Called by the
    /// room registry's explicit-destroy path only; dormancy eviction
    /// keeps the rows because an evicted-but-live room MUST restore.
    /// Default no-op mirrors the other optional hooks: single-node
    /// deployments never configure a `MucDurableStore` at all.
    fn delete_room_state<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, ()> {
        let _ = room_jid;
        Box::pin(async { Ok(()) })
    }

    /// Record the claim epoch this node most recently won for `room_jid`
    /// (called by the room registry immediately after a successful
    /// `ClaimStore::ensure_claimed`/steal — never by the store itself),
    /// so later `save_*` calls know which epoch to bind into their
    /// fencing SQL. Default no-op: single-node/non-clustering deployments
    /// never configure a `MucDurableStore` at all, so this is never
    /// called there.
    fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        let _ = (room_jid, fence);
    }

    /// Best-effort cache hygiene on claim release (dormancy eviction /
    /// explicit destroy): forget the recorded epoch so a stale cache
    /// entry cannot linger past this node's ownership. Default no-op.
    fn forget_claim_fence(&self, room_jid: &BareJid, expected: &RoomClaimFenceContext) {
        let _ = (room_jid, expected);
    }

    /// The two-part demotion protocol's guaranteed backstop (element 7): a
    /// fenced `SELECT ... FOR SHARE` against this room's claim (at the
    /// fence [`Self::record_claim_fence`] last recorded), run before every
    /// local fan-out. `Ok(true)` iff this node still holds the claim;
    /// `Ok(false)` means a steal has committed and the caller must demote
    /// locally and not deliver. Default `Ok(true)` (never demotes):
    /// single-node/non-clustering deployments never configure a
    /// `MucDurableStore` at all, so this default only matters for tests
    /// exercising the trait directly.
    fn check_fenced_fanout<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
        let _ = room_jid;
        Box::pin(async { Ok(true) })
    }

    /// The typed `(Entity, ClaimEpoch, node_id)` context this room is
    /// currently cached under (ADR-0017 Phase 3 Slice 7 FIX 1) — the exact
    /// same values [`Self::check_fenced_fanout`] resolves internally, handed
    /// out here so a caller needing its OWN fenced write (MAM's
    /// `store_message_fenced`, which uses its own Postgres connection and
    /// cannot share a transaction with this store) can bind them into an
    /// equivalent `SELECT ... FOR SHARE` check without re-deriving them from
    /// a second, independent mechanism. `None` when no epoch is cached for
    /// this room (this node has never claimed it, or `forget_claim_fence`
    /// ran) — callers treat that exactly like a fencing failure. Default
    /// `None`: single-node/non-clustering deployments never configure a
    /// `MucDurableStore` at all, so this default only matters for tests
    /// exercising the trait directly.
    fn current_claim_fence(&self, room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
        let _ = room_jid;
        None
    }

    /// Two-part demotion protocol, part (a) (element 7): best-effort,
    /// fire-and-forget notification to the node this room's claim was
    /// just stolen from, naming the entity and the new epoch. The
    /// recipient tombstones the entity and NotOwner-NACKs subsequent
    /// traffic. Never awaited for correctness — the guaranteed backstop
    /// is the fenced pre-fan-out check the room-dispatch path runs
    /// independently of this notification's delivery. Default no-op
    /// (single-node/non-clustering deployments have no relay to notify
    /// over).
    fn notify_previous_owner_demoted<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _previous_owner_node_id: &'a str,
        _previous_owner_node_epoch: &'a str,
        _new_epoch: ClaimEpoch,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
