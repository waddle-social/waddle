//! `UserActor` delivery surface (ADR-0017 Phase 1).
//!
//! Reproduces the load-bearing `ConnectionRegistry` routing behavior inside
//! the per-user actor so the DashMap delivery path can eventually be retired.
//! Every message here is a non-blocking `try_send` onto a resource's bounded
//! outbound channel and returns a typed [`BroadcastOutcome`] — the actor
//! never awaits socket capacity, so one wedged consumer can never stall the
//! user's other traffic or (once the MUC reflector routes through here) a
//! whole room's dispatch (locked Q-fix #699).
//!
//! Invariants mirrored from `ConnectionRegistry`, each with a test in the
//! parent module's `tests` submodule:
//!
//! 1. non-blocking `try_send` fan-out — a full channel drops + counts,
//!    never blocks;
//! 2. race-safe replacement: a re-registered resource's stale closed entry
//!    is evicted and delivery follows the live sender (structurally race-free
//!    here because register and send both serialize through the mailbox);
//! 3. the `DirectFrame` vs `PeerStanza`
//!    recipient-pass split is preserved on the queued [`OutboundStanza`];
//! 4. the Q7b pending-flush SM-row binding rides
//!    [`OutboundStanza::for_pending_flush`];
//! 5. `DroppedFull`/`DroppedClosed` accounting via the same Prometheus
//!    counters (`try_send_to_with_kind` in `ConnectionRegistry` bumps them;
//!    the [`BroadcastOutcome`] returned here maps 1:1 so callers and tests
//!    can assert them);
//! 6. no drop-rate regression under a join burst — a property of the
//!    per-connection actor's mailbox capacity, covered by the
//!    `join_burst_does_not_drop_at_capacity_256` /
//!    `join_burst_drops_at_default_capacity_64` tests in the parent `tests`
//!    submodule;
//! 7. RFC 6121 §8.5.2.1 bare-JID resource selection, excluding
//!    negative-priority resources.

use jid::FullJid;
use kameo::message::Context;
use tokio::sync::mpsc::error::TrySendError;

use super::UserActor;
use crate::registry::connection_registry::{BroadcastOutcome, ConnectionEntry, OutboundStanza};
use crate::Stanza;

impl UserActor {
    /// Non-blocking `try_send` of an already-built [`OutboundStanza`] to one
    /// resource, evicting the entry if its channel has closed. Shared by
    /// every delivery handler so the drop/evict/accounting logic lives in one
    /// place (mirrors `ConnectionRegistry::try_send_to_with_kind`).
    fn try_deliver(&mut self, jid: &FullJid, outbound: OutboundStanza) -> BroadcastOutcome {
        let Some(entry) = self.connections.get(jid) else {
            crate::telemetry::reliability::increment_broadcast_not_connected();
            return BroadcastOutcome::NotConnected;
        };
        let delivered_kind = crate::telemetry::messages::delivered_message_kind(&outbound.stanza);
        match entry.sender.try_send(outbound) {
            Ok(()) => {
                crate::telemetry::reliability::increment_broadcast_delivered();
                if let Some(kind) = delivered_kind {
                    crate::telemetry::messages::record_delivered_message(kind);
                }
                BroadcastOutcome::Delivered
            }
            Err(TrySendError::Full(_)) => {
                crate::telemetry::reliability::increment_broadcast_dropped_full();
                BroadcastOutcome::DroppedFull
            }
            Err(TrySendError::Closed(_)) => {
                crate::telemetry::reliability::increment_broadcast_dropped_closed();
                // The actor serializes register against send, so unlike the
                // DashMap path there is no concurrent-replacement window: if
                // the stored entry's channel is closed it is genuinely dead
                // and safe to evict. Route through `remove_resource` so the
                // per-resource cleanup set stays single-sourced and cannot
                // drift from the eviction path if a new field is added.
                self.remove_resource(jid);
                BroadcastOutcome::DroppedClosed
            }
        }
    }
}

/// RFC 6121 §8.5.2.1 destination-resource selection for bare-JID 1:1 routing.
///
/// Returns every connected resource whose advertised priority equals the
/// maximum among the user's **available, non-negative-priority** resources.
/// Per §8.5.2.1.1 a resource with negative priority is never a bare-JID
/// destination (invariant 7), and ties at the top priority all receive the
/// stanza (§8.5.2.1.1 governs message routing; §8.5.2.1.2 is presence). An
/// empty result means the caller should fall back to offline-storage
/// semantics.
pub struct SelectRoutableResources;

impl kameo::message::Message<SelectRoutableResources> for UserActor {
    type Reply = Vec<FullJid>;

    async fn handle(
        &mut self,
        _msg: SelectRoutableResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let candidates: Vec<(&FullJid, i8)> = self
            .connections
            .iter()
            .filter(|(_, entry)| entry.is_presence_available())
            .map(|(jid, entry)| (jid, entry.presence_priority()))
            .filter(|(_, priority)| *priority >= 0)
            .collect();
        let Some(max_priority) = candidates.iter().map(|(_, p)| *p).max() else {
            return Vec::new();
        };
        candidates
            .into_iter()
            .filter(|(_, p)| *p == max_priority)
            .map(|(jid, _)| jid.clone())
            .collect()
    }
}

/// Deliver a server-generated frame to one resource as a
/// `DirectFrame` — written straight to the wire without a
/// recipient pass (carbons, IQ replies, SM acks, …).
pub struct TrySendDirect {
    pub jid: FullJid,
    pub stanza: Stanza,
}

impl kameo::message::Message<TrySendDirect> for UserActor {
    type Reply = BroadcastOutcome;

    async fn handle(
        &mut self,
        msg: TrySendDirect,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.try_deliver(&msg.jid, OutboundStanza::new(msg.stanza))
    }
}

/// Deliver a peer-routed stanza to one resource as a
/// `PeerStanza` — the destination's main loop feeds it
/// through its state machine so the recipient pass (XEP-0191 incoming block,
/// XEP-0359 recipient stamp, XEP-0313 archive, XEP-0280 received-carbons,
/// inbox projection) runs before the wire write.
pub struct TrySendPeer {
    pub jid: FullJid,
    pub stanza: Stanza,
}

impl kameo::message::Message<TrySendPeer> for UserActor {
    type Reply = BroadcastOutcome;

    async fn handle(
        &mut self,
        msg: TrySendPeer,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.try_deliver(&msg.jid, OutboundStanza::peer_stanza(msg.stanza))
    }
}

/// Replay a queued `pending_delivery` row to a recovering session (locked Q7b
/// SM-ack lifecycle). The [`OutboundStanza::for_pending_flush`] envelope
/// carries the source row id and original receipt time so the destination's
/// main loop can bind the assigned XEP-0198 outbound counter back to the row.
pub struct TrySendPendingFlush {
    pub jid: FullJid,
    pub stanza: Stanza,
    pub row_id: crate::pending_delivery::PendingRowId,
    pub original_receipt_at: chrono::DateTime<chrono::Utc>,
}

impl kameo::message::Message<TrySendPendingFlush> for UserActor {
    type Reply = BroadcastOutcome;

    async fn handle(
        &mut self,
        msg: TrySendPendingFlush,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.try_deliver(
            &msg.jid,
            OutboundStanza::for_pending_flush(msg.stanza, msg.row_id, msg.original_receipt_at),
        )
    }
}

/// Read-only accessor: the [`ConnectionEntry`] for a resource, if connected.
/// Lets the connection-actor slice thread the sender/atomics through without
/// re-deriving them. Returns a clone (the entry is cheap `Arc`-backed).
pub struct GetConnectionEntry {
    pub jid: FullJid,
}

impl kameo::message::Message<GetConnectionEntry> for UserActor {
    type Reply = Option<ConnectionEntry>;

    async fn handle(
        &mut self,
        msg: GetConnectionEntry,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections.get(&msg.jid).cloned()
    }
}
