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
//! **Occupancy.** The occupant roster lives in actor memory and is not part
//! of the snapshot this trait restores. The actor reports every seat it
//! grants or removes through [`MucDurableStore::record_room_occupancy`]; a
//! store with a bound [`RoomOccupancyLedger`] writes them to the room's
//! durable occupancy record, from which a node that takes the room's claim
//! seats the same occupants. A store without one keeps no record.

use std::future::Future;
use std::pin::Pin;

use jid::{BareJid, FullJid};

use super::affiliation::AffiliationEntry as StoredAffiliationEntry;
use super::{RoomConfig, SubjectState};
use crate::ownership::{ClaimEpoch, CurrentNodeIdentityGuard, Entity, NodeIdentity};
use crate::types::Affiliation;
use crate::XmppError;

mod effects;
pub mod lifecycle;
pub mod projection;

pub use effects::{
    AdminPresenceKind, DestroyPassword, DestroyReason, DestroyRecipient, MucOccupantNick,
    OccupantPresenceUpdate, OccupantVoiceChange, RoomEffect, RoomEffectKind, RoomEffectReservation,
    RoomEffectStagingClass, RoomMutationEffects,
};
pub use lifecycle::{
    DestroyAttemptId, EphemeralProjectionAuthorization, NotAProjectionCommit, RoomCommitKind,
    RoomCommittedCoordinates, RoomEffectIntent, RoomEffectOrdinal, RoomLifecycleId,
    RoomLifecycleState, RoomMutationCommit, RoomRevision,
};
pub use projection::{OccupancyLeaveCause, RoomPinProjection, RoomProjection, RoomProjectionKind};

mod implementation;
pub(crate) use implementation::authorize_ephemeral_projection;
pub(crate) use implementation::mint_room_mutation_commit;
pub use implementation::AdminMutationId;
pub use implementation::AdminMutationReceipt;
pub use implementation::AffiliationEntry;
pub use implementation::ChannelId;
pub use implementation::DurableRoomState;
pub use implementation::MucDurableFuture;
pub use implementation::MucDurableStore;
pub use implementation::RoomClaimFenceContext;
pub use implementation::RoomCommitDatabaseError;
pub use implementation::RoomCommitError;
pub use implementation::RoomCommitFuture;
pub use implementation::RoomCommitOutcome;
pub use implementation::RoomDurableMutation;
pub use implementation::RoomOccupancyChange;
pub use implementation::RoomOccupancyLedger;
pub use implementation::RoomOccupancyRecord;
pub use implementation::RoomOccupancySeat;
pub use implementation::WaddleId;
