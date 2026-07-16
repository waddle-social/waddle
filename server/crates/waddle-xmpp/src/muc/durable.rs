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
/// (ADR-0017 Phase 3 Slice 7): the immutable `(Entity, ClaimEpoch,
/// NodeIdentity)` tuple retained by an actor incarnation. Actor-owned durable
/// load, save, and delete operations bind this exact fence instead of
/// resolving a possibly newer cache entry.
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
/// Every load, write, and delete is epoch-fenced against the exact claim
/// retained by that `RoomActor` incarnation — "epoch-fenced like all
/// claimed-entity writes" per element 7. The cache populated by
/// [`Self::record_claim_fence`] is not an authority for actor-owned writes.
///
/// **Fail-closed ownership contract (ADR-0017 Phase 3 Slice 7 FIX 2).**
/// This trait's `save_*` methods return an awaited `Result<(), XmppError>`.
/// A fenced write that fails returns `Err` to its immediate caller; it is not
/// fire-and-forget. What changed is what
/// [`super::room_actor::RoomActor`] itself does with that `Err`, for every
/// mutation handler that changes durable-relevant state (`UpdateConfig`,
/// `RollbackConfigIfRevision`, `UpdateGroupDmConfigByMember`, `SetSubject`,
/// `ChangeAffiliation`, `ApplyAdminItems`, `ApplyAffiliationChange`,
/// `EnforceMembersOnlyAffiliations`, `ReconcileChannelBackedRoom`):
///
/// 1. **Before mutating**: the handler runs
///    [`super::room_actor::RoomActor::gate_mutation`] — a `SELECT ... FOR
///    SHARE`-fenced [`Self::check_exact_claim_fence`] pre-check using the
///    actor incarnation's retained fence. It refuses to mutate both when
///    ownership was lost and when ownership cannot be proven.
/// 2. **After mutating**: config and ordinary affiliation writes classify a
///    `save_*` failure that is NOT ownership loss (a transient backend
///    outage) as a typed error rather than silently logging and swallowing
///    it. Subject persistence intentionally occurs before applying the
///    `SubjectState`, so its failure is classified before mutation.
///    The typed result surfaces as
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
    /// Load one authoritative room snapshot while proving that the actor
    /// incarnation's exact claim fence is still current in the same database
    /// transaction. Implementations must not split the ownership check and
    /// snapshot read across transactions: a claim steal between those reads
    /// would let a deposed actor install state owned by its successor.
    ///
    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>>;

    /// Durably upsert the room's configuration (plus the `waddle_id`/
    /// `channel_id` it travels with).
    fn save_config_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        waddle_id: &'a str,
        channel_id: &'a str,
        config: &'a RoomConfig,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()>;

    /// Update (or, when `subject` is `None`, clear) an existing room's current
    /// subject. This must not create a parent row: #1352 owns atomically
    /// creating the complete room plus initial Owner. Until then, a missing
    /// parent returns [`crate::XmppError::DurableRoomStateMissing`] so callers
    /// fail before acknowledging or applying a non-durable subject.
    fn save_subject_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        subject: Option<&'a SubjectState>,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()>;

    /// Durably upsert one affiliation-list entry. `Affiliation::None`
    /// removes the row, mirroring `AffiliationList::set`'s in-memory
    /// contract.
    fn save_affiliation_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        entry: &'a AffiliationEntry,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()>;

    /// XEP-0045 §10.9 (#1261): destroy removes the room "even if it was
    /// defined as persistent". Delete every durable row for `room_jid`
    /// — config, subject, and the full affiliation list — so a
    /// destroyed room can never resurrect from storage (with its old
    /// config, subject, or ban list) on the next join. Called by the
    /// room registry's explicit-destroy path only; dormancy eviction
    /// keeps the rows because an evicted-but-live room MUST restore.
    fn delete_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()>;

    /// Publish the claim fence alongside the matching ready room-registry
    /// entry — never immediately after acquire/steal while restore is still
    /// in flight. This cache is only for consumers that cannot carry an
    /// actor snapshot. Actor-owned load/save/delete operations receive their
    /// exact fence explicitly and must not consult this mutable room-JID cache
    /// for authorization. The cache remains the legacy pre-fanout groupchat
    /// dispatch/MAM backstop until #1283 replaces it. Default no-op:
    /// single-node/non-clustering deployments never configure a
    /// `MucDurableStore` at all, so this is never called there.
    fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        let _ = (room_jid, fence);
    }

    /// Best-effort cache hygiene on claim release (dormancy eviction /
    /// explicit destroy): forget the recorded epoch so a stale cache
    /// entry cannot linger past this node's ownership. Default no-op.
    fn forget_claim_fence(&self, room_jid: &BareJid, expected: &RoomClaimFenceContext) {
        let _ = (room_jid, expected);
    }

    /// Cache-based ownership check for the legacy pre-fanout groupchat
    /// dispatch/MAM path. New room actor paths must use
    /// [`Self::check_exact_claim_fence`] with the fence retained by their actor
    /// incarnation. `Ok(true)` iff this process can still serve the published
    /// claim; `Ok(false)` means no published fence is currently serviceable,
    /// including an absent cache entry, local identity rotation, or a
    /// definitive missing exact database tuple.
    /// Default `Ok(true)` (never demotes):
    /// single-node/non-clustering deployments never configure a
    /// `MucDurableStore` at all, so this default only matters for tests
    /// exercising the trait directly.
    fn check_fenced_fanout<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
        let _ = room_jid;
        Box::pin(async { Ok(true) })
    }

    /// Check the actor incarnation's retained claim rather than whichever
    /// claim is currently cached for the same room JID. Implementations must
    /// validate that `fence.entity` names `room_jid` and fail closed on
    /// backend errors. `Ok(false)` is a definitive non-serving result, not
    /// necessarily proof that another node has already committed a steal.
    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool>;

    /// The typed `(Entity, ClaimEpoch, node_id)` context currently published
    /// for a ready live room entry. This is observability/legacy support;
    /// actor-derived work must use the immutable fence carried by its own
    /// [`super::room_actor::RoomChainSnapshot`]. `None` while no actor is
    /// published (or after `forget_claim_fence`). Default
    /// `None`: single-node/non-clustering deployments never configure a
    /// `MucDurableStore` at all, so this default only matters for tests
    /// exercising the trait directly.
    fn current_claim_fence(&self, room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
        let _ = room_jid;
        None
    }

    /// Two-part demotion protocol, part (a) (element 7): an awaited
    /// notification to the node this room's claim was just stolen from,
    /// naming the entity and the new epoch. The recipient tombstones the
    /// entity and NotOwner-NACKs subsequent traffic. It is not relied on as
    /// the sole correctness mechanism: the cache-backed legacy pre-fanout
    /// check runs independently of notification delivery. Default no-op
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
