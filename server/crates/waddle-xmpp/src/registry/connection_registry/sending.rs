use super::endpoint::{
    BindingOutboundEndpoint, EndpointEnqueueOutcome, EndpointEnqueueRejection, EndpointPermit,
};
use super::{
    debug, instrument, BroadcastOutcome, ConnectionEntry, ConnectionRegistry, DashMap, FullJid,
    LocalConnectionBindingId, OutboundStanza, OutboundWriteAcceptance, PresenceState, SendResult,
    Stanza,
};
use crate::ownership::BindingRef;
use crate::pending_delivery::PendingRowId;
use crate::pending_delivery::SmSessionId;

/// Capacity reserved for one pending-delivery frame on an exact connection.
/// Dropping this value returns the capacity without publishing a frame.
#[derive(Debug)]
pub struct PendingFlushReservation {
    jid: FullJid,
    owner: LocalConnectionBindingId,
    binding_ref: Option<BindingRef>,
    endpoint: BindingOutboundEndpoint,
    permit: EndpointPermit,
}

/// Why a pending-delivery producer could not reserve outbound capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFlushReserveError {
    Full,
    Closed,
    NotConnected,
    NotAdmitted,
}

/// Result of the synchronous commit of reserved pending-delivery capacity.
///
/// A consumed channel permit does not prove that the socket received, wrote,
/// or took durable custody of the frame. The receiver can also close while
/// the permit is being committed, so that result must not authorize a retry
/// predicated on the frame definitely not being enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFlushCommitOutcome {
    AcceptedOrUnknown,
    DefinitelyNotEnqueued(PendingFlushCommitRejection),
}

/// Reasons the final commit refused the frame before consuming its permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFlushCommitRejection {
    StaleOrIneligible,
    BindingMismatch,
    Provisional,
    Sealed,
    ChannelClosed,
}

/// Outcome of one write-acceptance enqueue attempt against a captured binding.
///
/// A refusal returns the original frame and its acceptance callback. Queue
/// acceptance is not evidence that the socket wrote or recovered the frame.
#[derive(Debug)]
#[must_use]
pub enum WriteAcceptanceEnqueueOutcome {
    AcceptedOrUnknown,
    NotEnqueued {
        reason: WriteAcceptanceEnqueueRejection,
        outbound: Box<OutboundStanza>,
    },
}

/// A pre-enqueue refusal of a write-acceptance attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAcceptanceEnqueueRejection {
    /// No entry was present when the attempt began. This is only a snapshot.
    NotConnected,
    /// The captured entry was removed or replaced before its permit committed.
    BindingMismatch,
    Provisional,
    Sealed,
    Full,
    ChannelClosed,
}

impl WriteAcceptanceEnqueueOutcome {
    fn not_enqueued(reason: WriteAcceptanceEnqueueRejection, outbound: OutboundStanza) -> Self {
        Self::NotEnqueued {
            reason,
            outbound: Box::new(outbound),
        }
    }
}

impl From<EndpointEnqueueRejection> for WriteAcceptanceEnqueueRejection {
    fn from(reason: EndpointEnqueueRejection) -> Self {
        match reason {
            EndpointEnqueueRejection::Provisional => Self::Provisional,
            EndpointEnqueueRejection::Sealed => Self::Sealed,
            EndpointEnqueueRejection::Full => Self::Full,
            EndpointEnqueueRejection::ChannelClosed => Self::ChannelClosed,
            EndpointEnqueueRejection::BindingMismatch => Self::BindingMismatch,
        }
    }
}

impl ConnectionEntry {
    /// Called with the registry entry guard held. No await may separate the
    /// exact-binding check and permit commit, or occur under the admission
    /// lock. This path does not yet seal any production binding.
    fn commit_pending_flush(
        &self,
        reservation: PendingFlushReservation,
        outbound: OutboundStanza,
    ) -> PendingFlushCommitOutcome {
        use PendingFlushCommitOutcome::{AcceptedOrUnknown, DefinitelyNotEnqueued};
        use PendingFlushCommitRejection::{BindingMismatch, ChannelClosed, Provisional, Sealed};

        if self.binding_id() != reservation.owner
            || self.binding_ref() != reservation.binding_ref.as_ref()
            || !self.endpoint.same_binding(&reservation.endpoint)
            || !self
                .endpoint
                .matches_owner(reservation.owner, reservation.binding_ref.as_ref())
        {
            return DefinitelyNotEnqueued(BindingMismatch);
        }
        match reservation
            .endpoint
            .commit_reserved(reservation.permit, outbound)
        {
            EndpointEnqueueOutcome::AcceptedOrUnknown => AcceptedOrUnknown,
            EndpointEnqueueOutcome::ProvenNotEnqueued { reason, .. } => match reason {
                EndpointEnqueueRejection::BindingMismatch => DefinitelyNotEnqueued(BindingMismatch),
                EndpointEnqueueRejection::Provisional => DefinitelyNotEnqueued(Provisional),
                EndpointEnqueueRejection::Sealed => DefinitelyNotEnqueued(Sealed),
                EndpointEnqueueRejection::ChannelClosed => DefinitelyNotEnqueued(ChannelClosed),
                EndpointEnqueueRejection::Full => {
                    unreachable!("an owned permit cannot become full")
                }
            },
        }
    }
}

impl ConnectionRegistry {
    /// Wait for pending-delivery capacity before marking the row offered.
    /// No durable offer exists while this operation awaits backpressure.
    pub async fn reserve_pending_flush(
        &self,
        jid: &FullJid,
        owner: Option<&LocalConnectionBindingId>,
    ) -> Result<PendingFlushReservation, PendingFlushReserveError> {
        let entry = self
            .connections
            .get(jid)
            .filter(|entry| owner.is_none_or(|owner| entry.binding_id() == *owner))
            .ok_or(PendingFlushReserveError::NotConnected)?;
        let endpoint = entry.endpoint.clone();
        let captured_owner = entry.binding_id();
        let binding_ref = entry.binding_ref().cloned();
        drop(entry);
        match endpoint.reserve_owned().await {
            Ok(permit) => Ok(PendingFlushReservation {
                jid: jid.clone(),
                owner: captured_owner,
                binding_ref,
                endpoint,
                permit,
            }),
            Err(rejection) => {
                if rejection == EndpointEnqueueRejection::ChannelClosed {
                    self.remove_if_sender_closed_owner(jid, &endpoint);
                }
                Err(map_reserve_rejection(rejection))
            }
        }
    }

    /// Reserve capacity without waiting before marking a pending row offered.
    /// Even without a supplied owner, the reservation captures the current
    /// connection identity and cannot later be committed to its replacement.
    pub fn try_reserve_pending_flush(
        &self,
        jid: &FullJid,
        owner: Option<&LocalConnectionBindingId>,
    ) -> Result<PendingFlushReservation, PendingFlushReserveError> {
        let entry = self
            .connections
            .get(jid)
            .filter(|entry| owner.is_none_or(|owner| entry.binding_id() == *owner))
            .ok_or(PendingFlushReserveError::NotConnected)?;
        let endpoint = entry.endpoint.clone();
        let captured_owner = entry.binding_id();
        let binding_ref = entry.binding_ref().cloned();
        drop(entry);
        match endpoint.try_reserve_owned() {
            Ok(permit) => Ok(PendingFlushReservation {
                jid: jid.clone(),
                owner: captured_owner,
                binding_ref,
                endpoint,
                permit,
            }),
            Err(rejection) => {
                if rejection == EndpointEnqueueRejection::ChannelClosed {
                    self.remove_if_sender_closed_owner(jid, &endpoint);
                }
                Err(map_reserve_rejection(rejection))
            }
        }
    }

    /// Publish an already-authorized pending frame without another await.
    /// The exclusive registry guard serializes eligibility checks and enqueue
    /// against same-JID replacement and presence updates. The caller supplies
    /// the pending row tag for SM delivery, or a direct frame without SM.
    pub fn send_reserved_pending_flush(
        &self,
        reservation: PendingFlushReservation,
        outbound: OutboundStanza,
    ) -> PendingFlushCommitOutcome {
        let jid = reservation.jid.clone();
        let endpoint = reservation.endpoint.clone();
        let Some(entry) = self.connections.get_mut(&jid).filter(|entry| {
            entry.is_locally_hosted()
                && entry.is_presence_available()
                && entry.presence_priority() >= 0
        }) else {
            return PendingFlushCommitOutcome::DefinitelyNotEnqueued(
                PendingFlushCommitRejection::StaleOrIneligible,
            );
        };
        let outcome = entry.commit_pending_flush(reservation, outbound);
        drop(entry);
        if outcome
            == PendingFlushCommitOutcome::DefinitelyNotEnqueued(
                PendingFlushCommitRejection::ChannelClosed,
            )
        {
            self.remove_if_sender_closed_owner(&jid, &endpoint);
        }
        outcome
    }

    /// Enqueue a server-generated direct frame with a completion notifier that
    /// the destination connection resolves only after accepting the frame into
    /// its write/recovery-owning path. A single attempt is bound to the entry
    /// captured before waiting for capacity; it never retargets a replacement.
    /// Cancelling this future drops its envelope, so callers must retain an
    /// independent source if cancellation should be retryable.
    pub async fn send_to_with_write_acceptance(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        acceptance: OutboundWriteAcceptance,
    ) -> WriteAcceptanceEnqueueOutcome {
        let outbound = OutboundStanza::with_write_acceptance(stanza, acceptance);
        let endpoint = match self.connections.get(jid) {
            Some(entry) => entry.value().endpoint.clone(),
            None => {
                return WriteAcceptanceEnqueueOutcome::not_enqueued(
                    WriteAcceptanceEnqueueRejection::NotConnected,
                    outbound,
                )
            }
        };
        // Never hold a registry guard while waiting for capacity. Once a
        // permit is reserved, revalidate the binding and commit synchronously
        // under the registry guard so replacement cannot interleave.
        let permit = match endpoint.reserve_owned().await {
            Ok(permit) => permit,
            Err(reason) => {
                if reason == EndpointEnqueueRejection::ChannelClosed {
                    self.remove_if_sender_closed_owner(jid, &endpoint);
                }
                return WriteAcceptanceEnqueueOutcome::not_enqueued(reason.into(), outbound);
            }
        };
        let Some(entry) = self.connections.get(jid) else {
            return WriteAcceptanceEnqueueOutcome::not_enqueued(
                WriteAcceptanceEnqueueRejection::BindingMismatch,
                outbound,
            );
        };
        if !entry.endpoint.same_binding(&endpoint) {
            return WriteAcceptanceEnqueueOutcome::not_enqueued(
                WriteAcceptanceEnqueueRejection::BindingMismatch,
                outbound,
            );
        }
        let result = endpoint.commit_reserved(permit, outbound);
        drop(entry);
        match result {
            EndpointEnqueueOutcome::AcceptedOrUnknown => {
                WriteAcceptanceEnqueueOutcome::AcceptedOrUnknown
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { reason, outbound } => {
                if reason == EndpointEnqueueRejection::ChannelClosed {
                    self.remove_if_sender_closed_owner(jid, &endpoint);
                }
                WriteAcceptanceEnqueueOutcome::NotEnqueued {
                    reason: reason.into(),
                    outbound,
                }
            }
        }
    }

    /// Send a stanza to a connected user as a [`DeliveryKind::DirectFrame`]
    /// — the destination's main loop writes it straight to the wire
    /// without running the recipient pass.
    ///
    /// This is the right call for server-generated frames (carbons,
    /// IQ replies, SM acks, …). Peer-routed stanzas that must run through
    /// the recipient pipeline go through the authoritative `UserActor`'s
    /// `TrySendPeer` (ADR-0017 Slice 2), not a DashMap send.
    ///
    /// This waits for outbound channel capacity instead of dropping stanzas when
    /// a connection is temporarily backpressured. Closed channels are treated as
    /// stale connections and removed from the registry; if a concurrent
    /// `register` installed a fresh sender on the same JID between our lookup and
    /// a failed send, the stanza is retried on the replacement rather than lost.
    #[instrument(
        skip(self, stanza, jid),
        fields(to = %jid, message_id = tracing::field::Empty)
    )]
    pub async fn send_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        crate::telemetry::messages::record_span_message_id(stanza_message_id(&stanza));
        let endpoint = match self.connections.get(jid) {
            Some(entry) => entry.value().endpoint.clone(),
            None => {
                debug!("Recipient not connected");
                return SendResult::NotConnected;
            }
        };

        match endpoint.send(OutboundStanza::new(stanza)).await {
            EndpointEnqueueOutcome::AcceptedOrUnknown => {
                debug!("Stanza queued for delivery");
                SendResult::Sent
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::ChannelClosed,
                outbound,
            } => {
                debug!("Outbound channel closed, connection may have dropped");
                self.remove_if_sender_closed_owner(jid, &endpoint);
                if let Some(entry) = self.connections.get(jid) {
                    let current = entry.value().endpoint.clone();
                    drop(entry);
                    if !current.same_binding(&endpoint) {
                        return match current.send(*outbound).await {
                            EndpointEnqueueOutcome::AcceptedOrUnknown => {
                                debug!("Stanza queued for replacement connection");
                                SendResult::Sent
                            }
                            EndpointEnqueueOutcome::ProvenNotEnqueued {
                                reason: EndpointEnqueueRejection::ChannelClosed,
                                ..
                            } => {
                                self.remove_if_sender_closed_owner(jid, &current);
                                SendResult::ChannelClosed
                            }
                            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => {
                                SendResult::NotConnected
                            }
                        };
                    }
                }
                SendResult::ChannelClosed
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => SendResult::NotConnected,
        }
    }

    /// Owner-gated [`Self::send_to`]: deliver a `DirectFrame` only while the
    /// resource's current registry entry still belongs to `owner` (its local
    /// binding identity). Unlike [`Self::send_to`], it does NOT retry on a
    /// replacement sender — on an owner mismatch it returns `NotConnected`
    /// without delivering.
    ///
    /// Used for the off-task RFC 6121 §3.1.3 pending-subscribe delivery (issue
    /// #1220): those stanzas are dequeued non-destructively, so if this session
    /// was superseded the replacement's own once-per-session flush will deliver
    /// them — rerouting them to the replacement here (as `send_to` would) would
    /// double-deliver (Qodo review on PR #1234).
    #[instrument(skip(self, stanza, jid), fields(to = %jid))]
    pub async fn send_to_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
        stanza: Stanza,
    ) -> SendResult {
        let endpoint = match self.connections.get(jid) {
            Some(entry) if entry.value().binding_id() == *owner => entry.value().endpoint.clone(),
            _ => {
                debug!("Recipient not owned by this session; not delivering");
                return SendResult::NotConnected;
            }
        };
        match endpoint.send(OutboundStanza::new(stanza)).await {
            EndpointEnqueueOutcome::AcceptedOrUnknown => SendResult::Sent,
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::ChannelClosed,
                ..
            } => {
                self.remove_if_sender_closed_owner(jid, &endpoint);
                SendResult::ChannelClosed
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => SendResult::NotConnected,
        }
    }

    /// Send a [`pending_delivery`](crate::pending_delivery) flush stanza
    /// to a recovering session. Identical to [`Self::send_to`] except
    /// the queued [`OutboundStanza`] carries the source row id so the
    /// destination's main loop can bind the stanza's assigned XEP-0198
    /// outbound counter back to the row (locked Q7b SM-ack lifecycle).
    #[instrument(skip(self, stanza, jid), fields(to = %jid, row = %row_id))]
    pub async fn send_pending_flush(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        row_id: PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> SendResult {
        let endpoint = match self.connections.get(jid) {
            Some(entry) => entry.value().endpoint.clone(),
            None => {
                debug!("Recipient not connected for pending flush");
                return SendResult::NotConnected;
            }
        };
        let outbound = OutboundStanza::for_pending_flush(stanza, row_id, original_receipt_at);
        match endpoint.send(outbound).await {
            EndpointEnqueueOutcome::AcceptedOrUnknown => SendResult::Sent,
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::ChannelClosed,
                ..
            } => {
                self.remove_if_sender_closed_owner(jid, &endpoint);
                SendResult::ChannelClosed
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => SendResult::NotConnected,
        }
    }

    /// Owner-gated variant of [`Self::send_pending_flush`]. Delivers only if
    /// the resource's current registry entry still belongs to `owner` (the
    /// local binding identity, mirroring [`Self::entry_if_owner`] /
    /// [`Self::try_send_outbound_if_owner`]); otherwise returns
    /// `NotConnected` without sending.
    ///
    /// The XEP-0160 offline flush (issue #1220) runs on a spawned task and
    /// pushes SM-claimed rows tagged with the ORIGINAL session's stream id.
    /// If that session were superseded by a same-full-JID replacement
    /// mid-flush, an ungated send would deliver those rows to the
    /// replacement, whose `<a h>` acks key on a DIFFERENT stream id and so
    /// never clear the original session's claim — wedging the rows until the
    /// claim-expiry janitor releases them, with a duplicate-delivery risk.
    /// Gating the send binds the flush to the session it was planned for; on
    /// a mismatch the caller releases the row for the replacement's own flush.
    #[instrument(skip(self, stanza, jid), fields(to = %jid, row = %row_id))]
    pub async fn send_pending_flush_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
        stanza: Stanza,
        row_id: PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> SendResult {
        let endpoint = match self.connections.get(jid) {
            Some(entry) if entry.value().binding_id() == *owner => entry.value().endpoint.clone(),
            _ => {
                debug!("Recipient not owned by this session for pending flush");
                return SendResult::NotConnected;
            }
        };
        let outbound = OutboundStanza::for_pending_flush(stanza, row_id, original_receipt_at);
        match endpoint.send(outbound).await {
            EndpointEnqueueOutcome::AcceptedOrUnknown => SendResult::Sent,
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::ChannelClosed,
                ..
            } => {
                self.remove_if_sender_closed_owner(jid, &endpoint);
                SendResult::ChannelClosed
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => SendResult::NotConnected,
        }
    }

    /// Non-blocking send as [`DeliveryKind::DirectFrame`]. Returns a
    /// typed `BroadcastOutcome` describing delivery, absence, or
    /// which silent-drop path was taken.
    ///
    /// Intended for fan-out paths (XEP-0163 PEP fanout, MUC presence
    /// broadcasts, roster pushes, …) where a slow or zombied
    /// consumer must never stall the producer task.
    ///
    /// Peer-routed groupchat reflection that needs the recipient pass now routes
    /// through the authoritative `UserActor`'s `TrySendPeer` (ADR-0017 Slice 2),
    /// not a DashMap send.
    ///
    /// On `Closed` the stale entry is evicted, but only if the
    /// current registry entry's sender is still closed — a
    /// concurrent `register` for the same FullJid may have installed
    /// a fresh, live sender between our `get` and `try_send`, and we
    /// must not wipe the newcomer. On `Full` the stanza is dropped
    /// without touching the registry (the consumer may just be
    /// catching up).
    ///
    /// Every outcome bumps a Prometheus counter so production drop
    /// rates are visible even when callers discard the return value.
    pub fn try_send_to(&self, jid: &FullJid, stanza: Stanza) -> BroadcastOutcome {
        self.try_send_to_matching(jid, stanza, |_| true)
    }

    /// Recovery may enqueue only on a real local socket, retaining its frozen
    /// obligation so concurrent attempts share live acceptance deduplication.
    pub fn try_send_outbound_to_locally_hosted(
        &self,
        jid: &FullJid,
        outbound: OutboundStanza,
    ) -> BroadcastOutcome {
        let Some(entry) = self
            .connections
            .get(jid)
            .filter(|entry| entry.is_locally_hosted())
            .map(|entry| entry.clone())
        else {
            return BroadcastOutcome::NotConnected;
        };
        self.try_send_outbound_if_owner(jid, &entry.binding_id(), outbound)
    }

    fn try_send_to_matching(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        matches: impl FnOnce(&ConnectionEntry) -> bool,
    ) -> BroadcastOutcome {
        let endpoint = match self.connections.get(jid) {
            Some(entry) if matches(entry.value()) => entry.value().endpoint.clone(),
            None => {
                crate::telemetry::reliability::increment_broadcast_not_connected();
                return BroadcastOutcome::NotConnected;
            }
            Some(_) => {
                crate::telemetry::reliability::increment_broadcast_not_connected();
                return BroadcastOutcome::NotConnected;
            }
        };

        let delivered_kind = crate::telemetry::messages::delivered_message_kind(&stanza);
        match endpoint.try_send(OutboundStanza::new(stanza)) {
            EndpointEnqueueOutcome::AcceptedOrUnknown => {
                crate::telemetry::reliability::increment_broadcast_delivered();
                if let Some(kind) = delivered_kind {
                    crate::telemetry::messages::record_delivered_message(kind);
                }
                BroadcastOutcome::Delivered
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::Full,
                ..
            } => {
                crate::telemetry::reliability::increment_broadcast_dropped_full();
                // Keep per-recipient detail at debug only — the
                // aggregated broadcast log at the call site already
                // reports a per-send `dropped_full` total, and
                // `xmpp.broadcast.dropped_full (alias waddle_broadcast_dropped_full_total)` is always on.
                // A `warn!` here would turn into a log storm under
                // sustained fan-out backpressure (125+/s) and drown
                // out every other signal on the pod.
                debug!(
                    jid = %jid,
                    "Outbound channel full; broadcast stanza dropped"
                );
                BroadcastOutcome::DroppedFull
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued {
                reason: EndpointEnqueueRejection::ChannelClosed,
                ..
            } => {
                crate::telemetry::reliability::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed(jid);
                BroadcastOutcome::DroppedClosed
            }
            EndpointEnqueueOutcome::ProvenNotEnqueued { .. } => {
                crate::telemetry::reliability::increment_broadcast_not_connected();
                BroadcastOutcome::NotConnected
            }
        }
    }

    /// Non-blocking send of an already-tagged outbound frame, gated on the
    /// resource still belonging to the provided connection owner.
    ///
    /// Clustered remote-resource delivery uses this on the socket node: the
    /// authoritative `UserActor` may live on another node, but the real
    /// WebSocket channel remains in this registry. The owner check mirrors
    /// [`Self::entry_if_owner`] so a delayed relay frame from an older same
    /// full-JID connection cannot be written to a replacement session.
    pub fn try_send_outbound_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
        outbound: OutboundStanza,
    ) -> BroadcastOutcome {
        let entry = match self.connections.get(jid) {
            Some(entry) if entry.value().binding_id() == *owner => entry.value().clone(),
            _ => {
                crate::telemetry::reliability::increment_broadcast_not_connected();
                return BroadcastOutcome::NotConnected;
            }
        };

        // Deliberately NOT counted in `waddle.messages.delivered`: the only
        // production caller is the clustered route bridge on the socket
        // node, and cross-node deliveries are counted exactly once on the
        // UserActor-owner node — pump-relayed frames at relay-channel
        // entry (`try_deliver`/`try_send_to`), direct remote-resource
        // frames on the socket node's Delivered acknowledgment in
        // `deliver_registered_remote_resource_with_registration`.
        // Counting here would double every cross-node delivery.
        match entry.try_send_archive_ordered(outbound) {
            Ok(()) => {
                crate::telemetry::reliability::increment_broadcast_delivered();
                BroadcastOutcome::Delivered
            }
            Err(EndpointEnqueueRejection::Full) => {
                crate::telemetry::reliability::increment_broadcast_dropped_full();
                BroadcastOutcome::DroppedFull
            }
            Err(EndpointEnqueueRejection::ChannelClosed) => {
                crate::telemetry::reliability::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed_owner(jid, &entry.endpoint);
                BroadcastOutcome::DroppedClosed
            }
            Err(_) => {
                crate::telemetry::reliability::increment_broadcast_not_connected();
                BroadcastOutcome::NotConnected
            }
        }
    }

    /// Capture an owned socket witness for a room actor's synchronous replay
    /// boundary. The shared map is rechecked when invoked; a replacement
    /// connection can never inherit this callback's frozen frame.
    pub fn capture_owned_enqueue_if_owner(
        &self,
        jid: FullJid,
        owner: LocalConnectionBindingId,
        outbound: OutboundStanza,
    ) -> impl FnOnce() -> BroadcastOutcome + Send + 'static {
        let connections = self.connections.clone();
        let presence_states = self.presence_states.clone();
        let sm_stream_owners = self.sm_stream_owners.clone();
        move || {
            let entry = match connections.get(&jid) {
                Some(entry) if entry.binding_id() == owner => entry.value().clone(),
                _ => {
                    crate::telemetry::reliability::increment_broadcast_not_connected();
                    return BroadcastOutcome::NotConnected;
                }
            };
            match entry.try_send_archive_ordered(outbound) {
                Ok(()) => {
                    crate::telemetry::reliability::increment_broadcast_delivered();
                    BroadcastOutcome::Delivered
                }
                Err(EndpointEnqueueRejection::Full) => {
                    crate::telemetry::reliability::increment_broadcast_dropped_full();
                    BroadcastOutcome::DroppedFull
                }
                Err(EndpointEnqueueRejection::ChannelClosed) => {
                    crate::telemetry::reliability::increment_broadcast_dropped_closed();
                    Self::remove_closed_owner_entry(
                        &connections,
                        &presence_states,
                        &sm_stream_owners,
                        &jid,
                        &entry.endpoint,
                    );
                    BroadcastOutcome::DroppedClosed
                }
                Err(_) => {
                    crate::telemetry::reliability::increment_broadcast_not_connected();
                    BroadcastOutcome::NotConnected
                }
            }
        }
    }

    /// Race-safe eviction of a stale entry whose outbound channel is closed.
    ///
    /// Used on the non-blocking broadcast path to clean up zombies without
    /// risking the deletion of a live registration that happened to take
    /// over the slot between the caller's `get` and its `try_send`. If the
    /// currently-registered sender is still closed, the entry is removed
    /// and the connected-users metric and presence state are updated;
    /// otherwise this is a no-op.
    pub(super) fn remove_if_sender_closed(&self, jid: &FullJid) {
        let removed = self.connections.remove_if(jid, |_, entry| {
            if !entry.endpoint.is_closed() {
                return false;
            }
            Self::remove_sm_stream_owner(&self.sm_stream_owners, jid, entry);
            Self::remove_retired_presence(&self.presence_states, jid);
            true
        });
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
            debug!(jid = %jid, "Evicted stale closed connection entry");
        }
    }

    /// Race-safe eviction for an awaited send failure.
    ///
    /// The async send path clones the sender before awaiting channel capacity.
    /// If another session replaces the same FullJid while the await is in
    /// progress, a failed send on the old channel must not unregister the new
    /// session. Match both closed state and channel identity.
    pub(super) fn remove_if_sender_closed_owner(
        &self,
        jid: &FullJid,
        endpoint: &BindingOutboundEndpoint,
    ) {
        Self::remove_closed_owner_entry(
            &self.connections,
            &self.presence_states,
            &self.sm_stream_owners,
            jid,
            endpoint,
        );
    }

    fn remove_closed_owner_entry(
        connections: &DashMap<FullJid, ConnectionEntry>,
        presence_states: &DashMap<FullJid, PresenceState>,
        sm_stream_owners: &DashMap<SmSessionId, FullJid>,
        jid: &FullJid,
        endpoint: &BindingOutboundEndpoint,
    ) {
        let removed = connections.remove_if(jid, |_, entry| {
            if !entry.endpoint.is_closed() || !entry.endpoint.same_binding(endpoint) {
                return false;
            }
            Self::remove_sm_stream_owner(sm_stream_owners, jid, entry);
            Self::remove_retired_presence(presence_states, jid);
            true
        });
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
            debug!(jid = %jid, "Evicted stale owned closed connection entry");
        }
    }
}

fn stanza_message_id(stanza: &Stanza) -> &str {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map_or("", |id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => "",
    }
}

fn map_reserve_rejection(rejection: EndpointEnqueueRejection) -> PendingFlushReserveError {
    match rejection {
        EndpointEnqueueRejection::Full => PendingFlushReserveError::Full,
        EndpointEnqueueRejection::ChannelClosed => PendingFlushReserveError::Closed,
        EndpointEnqueueRejection::Provisional | EndpointEnqueueRejection::Sealed => {
            PendingFlushReserveError::NotAdmitted
        }
        EndpointEnqueueRejection::BindingMismatch => {
            unreachable!("capacity reservation does not commit a permit")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionEntry, OutboundStanza, OutboundWriteAcceptance, PendingFlushCommitOutcome,
        PendingFlushCommitRejection, PendingFlushReservation, Stanza,
    };
    use tokio::sync::mpsc;

    #[test]
    fn pending_flush_maps_a_foreign_permit_to_binding_mismatch() {
        let (sender, mut receiver) = mpsc::channel(1);
        let entry = ConnectionEntry::new(sender.clone());
        let (foreign_sender, mut foreign_receiver) = mpsc::channel(1);
        let foreign = ConnectionEntry::new(foreign_sender.clone());
        let permit = foreign
            .endpoint
            .try_reserve_owned()
            .expect("reserve foreign queue");
        let reservation = PendingFlushReservation {
            jid: "bob@example.test/phone".parse().expect("full JID"),
            owner: entry.binding_id(),
            binding_ref: entry.binding_ref().cloned(),
            endpoint: entry.endpoint.clone(),
            permit,
        };
        let (acceptance, mut accepted) = OutboundWriteAcceptance::new();
        let outcome = entry.commit_pending_flush(
            reservation,
            OutboundStanza::with_write_acceptance(
                Stanza::Message(xmpp_parsers::message::Message::new(None)),
                acceptance,
            ),
        );

        assert_eq!(
            outcome,
            PendingFlushCommitOutcome::DefinitelyNotEnqueued(
                PendingFlushCommitRejection::BindingMismatch
            )
        );
        assert_eq!(sender.capacity(), 1);
        assert_eq!(foreign_sender.capacity(), 1);
        assert!(receiver.try_recv().is_err());
        assert!(foreign_receiver.try_recv().is_err());
        assert!(matches!(
            accepted.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }
}
