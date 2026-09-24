use super::*;

/// Capacity reserved for one pending-delivery frame on an exact connection.
/// Dropping this value returns the capacity without publishing a frame.
#[derive(Debug)]
pub struct PendingFlushReservation {
    jid: FullJid,
    owner: Arc<AtomicBool>,
    sender: mpsc::Sender<OutboundStanza>,
    permit: mpsc::OwnedPermit<OutboundStanza>,
}

/// Why a pending-delivery producer could not reserve outbound capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFlushReserveError {
    Full,
    Closed,
    NotConnected,
}

impl ConnectionRegistry {
    /// Wait for pending-delivery capacity before marking the row offered.
    /// No durable offer exists while this operation awaits backpressure.
    pub async fn reserve_pending_flush(
        &self,
        jid: &FullJid,
        owner: Option<&Arc<AtomicBool>>,
    ) -> Result<PendingFlushReservation, PendingFlushReserveError> {
        let entry = self
            .connections
            .get(jid)
            .filter(|entry| owner.is_none_or(|owner| Arc::ptr_eq(&entry.carbons_enabled, owner)))
            .ok_or(PendingFlushReserveError::NotConnected)?;
        let sender = entry.sender.clone();
        let captured_owner = Arc::clone(&entry.carbons_enabled);
        drop(entry);
        match sender.clone().reserve_owned().await {
            Ok(permit) => Ok(PendingFlushReservation {
                jid: jid.clone(),
                owner: captured_owner,
                sender,
                permit,
            }),
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                Err(PendingFlushReserveError::Closed)
            }
        }
    }

    /// Reserve capacity without waiting before marking a pending row offered.
    /// Even without a supplied owner, the reservation captures the current
    /// connection identity and cannot later be committed to its replacement.
    pub fn try_reserve_pending_flush(
        &self,
        jid: &FullJid,
        owner: Option<&Arc<AtomicBool>>,
    ) -> Result<PendingFlushReservation, PendingFlushReserveError> {
        let entry = self
            .connections
            .get(jid)
            .filter(|entry| owner.is_none_or(|owner| Arc::ptr_eq(&entry.carbons_enabled, owner)))
            .ok_or(PendingFlushReserveError::NotConnected)?;
        let sender = entry.sender.clone();
        let captured_owner = Arc::clone(&entry.carbons_enabled);
        drop(entry);
        match sender.clone().try_reserve_owned() {
            Ok(permit) => Ok(PendingFlushReservation {
                jid: jid.clone(),
                owner: captured_owner,
                sender,
                permit,
            }),
            Err(mpsc::error::TrySendError::Full(_)) => Err(PendingFlushReserveError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                Err(PendingFlushReserveError::Closed)
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
    ) -> SendResult {
        let PendingFlushReservation {
            jid,
            owner,
            sender,
            permit,
        } = reservation;
        let Some(entry) = self.connections.get_mut(&jid).filter(|entry| {
            Arc::ptr_eq(&entry.carbons_enabled, &owner)
                && entry.sender.same_channel(&sender)
                && entry.is_locally_hosted()
                && entry.is_presence_available()
                && entry.presence_priority() >= 0
        }) else {
            return SendResult::NotConnected;
        };
        if sender.is_closed() {
            drop(entry);
            self.remove_if_sender_closed_owner(&jid, &sender);
            return SendResult::ChannelClosed;
        }
        permit.send(outbound);
        drop(entry);
        SendResult::Sent
    }

    /// Enqueue a server-generated direct frame with a completion notifier that
    /// the destination connection resolves only after accepting the frame into
    /// its write/recovery-owning path.
    pub async fn send_to_with_write_acceptance(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        acceptance: OutboundWriteAcceptance,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => return SendResult::NotConnected,
        };
        let outbound = OutboundStanza::with_write_acceptance(stanza.clone(), acceptance.clone());
        match sender.send(outbound).await {
            Ok(()) => SendResult::Sent,
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                if let Some(entry) = self.connections.get(jid) {
                    let current = entry.value().sender.clone();
                    drop(entry);
                    if !current.same_channel(&sender) {
                        return match current
                            .send(OutboundStanza::with_write_acceptance(stanza, acceptance))
                            .await
                        {
                            Ok(()) => SendResult::Sent,
                            Err(_) => {
                                self.remove_if_sender_closed_owner(jid, &current);
                                SendResult::ChannelClosed
                            }
                        };
                    }
                }
                SendResult::ChannelClosed
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
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                debug!("Recipient not connected");
                return SendResult::NotConnected;
            }
        };

        match sender.send(OutboundStanza::new(stanza.clone())).await {
            Ok(()) => {
                debug!("Stanza queued for delivery");
                SendResult::Sent
            }
            Err(_) => {
                debug!("Outbound channel closed, connection may have dropped");
                self.remove_if_sender_closed_owner(jid, &sender);
                if let Some(entry) = self.connections.get(jid) {
                    let current = entry.value().sender.clone();
                    drop(entry);
                    if !current.same_channel(&sender) {
                        return match current.send(OutboundStanza::new(stanza)).await {
                            Ok(()) => {
                                debug!("Stanza queued for replacement connection");
                                SendResult::Sent
                            }
                            Err(_) => {
                                self.remove_if_sender_closed_owner(jid, &current);
                                SendResult::ChannelClosed
                            }
                        };
                    }
                }
                SendResult::ChannelClosed
            }
        }
    }

    /// Owner-gated [`Self::send_to`]: deliver a `DirectFrame` only while the
    /// resource's current registry entry still belongs to `owner` (the carbons
    /// ownership token). Unlike [`Self::send_to`], it does NOT retry on a
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
        owner: &Arc<AtomicBool>,
        stanza: Stanza,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) if Arc::ptr_eq(&entry.value().carbons_enabled, owner) => {
                entry.value().sender.clone()
            }
            _ => {
                debug!("Recipient not owned by this session; not delivering");
                return SendResult::NotConnected;
            }
        };
        match sender.send(OutboundStanza::new(stanza)).await {
            Ok(()) => SendResult::Sent,
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                SendResult::ChannelClosed
            }
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
        row_id: crate::pending_delivery::PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                debug!("Recipient not connected for pending flush");
                return SendResult::NotConnected;
            }
        };
        let outbound = OutboundStanza::for_pending_flush(stanza, row_id, original_receipt_at);
        match sender.send(outbound).await {
            Ok(()) => SendResult::Sent,
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                SendResult::ChannelClosed
            }
        }
    }

    /// Owner-gated variant of [`Self::send_pending_flush`]. Delivers only if
    /// the resource's current registry entry still belongs to `owner` (the
    /// carbons ownership token, mirroring [`Self::entry_if_owner`] /
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
        owner: &Arc<AtomicBool>,
        stanza: Stanza,
        row_id: crate::pending_delivery::PendingRowId,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) if Arc::ptr_eq(&entry.value().carbons_enabled, owner) => {
                entry.value().sender.clone()
            }
            _ => {
                debug!("Recipient not owned by this session for pending flush");
                return SendResult::NotConnected;
            }
        };
        let outbound = OutboundStanza::for_pending_flush(stanza, row_id, original_receipt_at);
        match sender.send(outbound).await {
            Ok(()) => SendResult::Sent,
            Err(_) => {
                self.remove_if_sender_closed_owner(jid, &sender);
                SendResult::ChannelClosed
            }
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
        self.try_send_outbound_if_owner(jid, &entry.carbons_enabled, outbound)
    }

    fn try_send_to_matching(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        matches: impl FnOnce(&ConnectionEntry) -> bool,
    ) -> BroadcastOutcome {
        let sender = match self.connections.get(jid) {
            Some(entry) if matches(entry.value()) => entry.value().sender.clone(),
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
        match sender.try_send(OutboundStanza::new(stanza)) {
            Ok(()) => {
                crate::telemetry::reliability::increment_broadcast_delivered();
                if let Some(kind) = delivered_kind {
                    crate::telemetry::messages::record_delivered_message(kind);
                }
                BroadcastOutcome::Delivered
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
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
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                crate::telemetry::reliability::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed(jid);
                BroadcastOutcome::DroppedClosed
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
        owner: &Arc<AtomicBool>,
        outbound: OutboundStanza,
    ) -> BroadcastOutcome {
        let entry = match self.connections.get(jid) {
            Some(entry) if Arc::ptr_eq(&entry.value().carbons_enabled, owner) => {
                entry.value().clone()
            }
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
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                crate::telemetry::reliability::increment_broadcast_dropped_full();
                BroadcastOutcome::DroppedFull
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                crate::telemetry::reliability::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed_owner(jid, &entry.sender);
                BroadcastOutcome::DroppedClosed
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
        let removed = self
            .connections
            .remove_if(jid, |_, entry| entry.sender.is_closed());
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
            self.presence_states.remove(jid);
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
        sender: &mpsc::Sender<OutboundStanza>,
    ) {
        let removed = self.connections.remove_if(jid, |_, entry| {
            entry.sender.is_closed() && entry.sender.same_channel(sender)
        });
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
            self.presence_states.remove(jid);
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
