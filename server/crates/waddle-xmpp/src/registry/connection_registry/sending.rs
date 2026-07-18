use super::*;

impl ConnectionRegistry {
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
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
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
    #[instrument(skip(self, stanza), fields(to = %jid))]
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
    #[instrument(skip(self, stanza), fields(to = %jid, row = %row_id))]
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
    #[instrument(skip(self, stanza), fields(to = %jid, row = %row_id))]
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
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
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
                // `waddle_broadcast_dropped_full_total` is always on.
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
        let sender = match self.connections.get(jid) {
            Some(entry) if Arc::ptr_eq(&entry.value().carbons_enabled, owner) => {
                entry.value().sender.clone()
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
        match sender.try_send(outbound) {
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
                self.remove_if_sender_closed_owner(jid, &sender);
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
            prometheus::decrement_connected_users();
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
            prometheus::decrement_connected_users();
            crate::metrics::adjust_connections_active(-1);
            self.presence_states.remove(jid);
            debug!(jid = %jid, "Evicted stale owned closed connection entry");
        }
    }
}
