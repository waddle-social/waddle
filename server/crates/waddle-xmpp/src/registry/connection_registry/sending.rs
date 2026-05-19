use super::*;

impl ConnectionRegistry {
    /// Send a stanza to a connected user as a [`DeliveryKind::DirectFrame`]
    /// — the destination's main loop writes it straight to the wire
    /// without running the recipient pass.
    ///
    /// This is the right call for server-generated frames (carbons,
    /// IQ replies, SM acks, …). For peer-routed stanzas that should
    /// run through the recipient pipeline, use [`Self::send_peer_to`].
    ///
    /// This waits for outbound channel capacity instead of dropping stanzas when
    /// a connection is temporarily backpressured. Closed channels are treated as
    /// stale connections and removed from the registry.
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        self.send_to_with_kind(jid, stanza, DeliveryKind::DirectFrame)
            .await
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

    /// Send a stanza to a connected user as a [`DeliveryKind::PeerStanza`]
    /// — the destination's main loop feeds
    /// [`crate::protocol::InboundEvent::StanzaFromPeer`] into its
    /// state machine so the recipient-pass pipeline (XEP-0191
    /// incoming block, XEP-0359 recipient stamp, XEP-0313 archive,
    /// XEP-0280 received-carbons, inbox projection) runs before any
    /// wire write.
    ///
    /// Used by the [`crate::protocol::OutboundEvent::RouteToConnection`]
    /// interpreter arm starting in #229 PR12.
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_peer_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        self.send_to_with_kind(jid, stanza, DeliveryKind::PeerStanza)
            .await
    }

    /// Inner helper shared by [`Self::send_to`] and [`Self::send_peer_to`]
    /// so the queueing / replacement / channel-closed paths stay in
    /// one place. The only thing the public callers vary is the
    /// [`DeliveryKind`] tag on the queued [`OutboundStanza`], which
    /// is built via the same [`OutboundStanza::new`] / [`OutboundStanza::peer_stanza`]
    /// constructors every other call site uses — no manual struct
    /// literal here so the kind→constructor mapping stays in one
    /// place.
    async fn send_to_with_kind(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        kind: DeliveryKind,
    ) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                debug!("Recipient not connected");
                return SendResult::NotConnected;
            }
        };

        let make_outbound = |s: Stanza| match kind {
            DeliveryKind::DirectFrame => OutboundStanza::new(s),
            DeliveryKind::PeerStanza => OutboundStanza::peer_stanza(s),
        };

        match sender.send(make_outbound(stanza.clone())).await {
            Ok(()) => {
                debug!(?kind, "Stanza queued for delivery");
                SendResult::Sent
            }
            Err(_) => {
                debug!("Outbound channel closed, connection may have dropped");
                self.remove_if_sender_closed_owner(jid, &sender);
                if let Some(entry) = self.connections.get(jid) {
                    let current = entry.value().sender.clone();
                    drop(entry);
                    if !current.same_channel(&sender) {
                        return match current.send(make_outbound(stanza)).await {
                            Ok(()) => {
                                debug!(?kind, "Stanza queued for replacement connection");
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

    /// Non-blocking send as [`DeliveryKind::DirectFrame`]. Returns a
    /// typed `BroadcastOutcome` describing delivery, absence, or
    /// which silent-drop path was taken.
    ///
    /// Intended for fan-out paths (XEP-0163 PEP fanout, MUC presence
    /// broadcasts, roster pushes, …) where a slow or zombied
    /// consumer must never stall the producer task.
    ///
    /// For peer-routed groupchat reflection that needs the recipient
    /// pass (XEP-0359 recipient stamp, XEP-0313 archive, XEP-0280
    /// received-carbons, inbox projection), use
    /// [`Self::try_send_peer_to`] instead — same non-blocking
    /// guarantees, but the destination's main loop runs its state
    /// machine before writing to the wire.
    pub fn try_send_to(&self, jid: &FullJid, stanza: Stanza) -> BroadcastOutcome {
        self.try_send_to_with_kind(jid, stanza, DeliveryKind::DirectFrame)
    }

    /// Non-blocking [`DeliveryKind::PeerStanza`] send. Locked Q-fix
    /// #699: the MUC reflector emits one `RouteToConnection` per
    /// occupant and must NOT block on any single connection's mpsc
    /// — one zombie WebSocket peer with a hung consumer task was
    /// enough to stall every groupchat dispatch through the same
    /// interpreter loop (and therefore archive + inbox writes for
    /// every other user of the room) until the pod restarted.
    ///
    /// Same `BroadcastOutcome` contract as [`Self::try_send_to`];
    /// the destination's main loop feeds the stanza through its
    /// state machine as `InboundEvent::StanzaFromPeer` so the
    /// recipient pass still runs when the channel has capacity.
    pub fn try_send_peer_to(&self, jid: &FullJid, stanza: Stanza) -> BroadcastOutcome {
        self.try_send_to_with_kind(jid, stanza, DeliveryKind::PeerStanza)
    }

    /// Shared non-blocking send path. The only thing the public
    /// callers vary is the [`DeliveryKind`] tag on the queued
    /// [`OutboundStanza`], which is built via the same
    /// [`OutboundStanza::new`] / [`OutboundStanza::peer_stanza`]
    /// constructors as the blocking [`Self::send_to_with_kind`] — no
    /// manual struct literal here so the kind→constructor mapping
    /// stays in one place.
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
    fn try_send_to_with_kind(
        &self,
        jid: &FullJid,
        stanza: Stanza,
        kind: DeliveryKind,
    ) -> BroadcastOutcome {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().sender.clone(),
            None => {
                prometheus::increment_broadcast_not_connected();
                return BroadcastOutcome::NotConnected;
            }
        };

        let outbound = match kind {
            DeliveryKind::DirectFrame => OutboundStanza::new(stanza),
            DeliveryKind::PeerStanza => OutboundStanza::peer_stanza(stanza),
        };

        match sender.try_send(outbound) {
            Ok(()) => {
                prometheus::increment_broadcast_delivered();
                BroadcastOutcome::Delivered
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                prometheus::increment_broadcast_dropped_full();
                // Keep per-recipient detail at debug only — the
                // aggregated broadcast log at the call site already
                // reports a per-send `dropped_full` total, and
                // `waddle_broadcast_dropped_full_total` is always on.
                // A `warn!` here would turn into a log storm under
                // sustained fan-out backpressure (125+/s) and drown
                // out every other signal on the pod.
                debug!(
                    jid = %jid,
                    ?kind,
                    "Outbound channel full; broadcast stanza dropped"
                );
                BroadcastOutcome::DroppedFull
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                prometheus::increment_broadcast_dropped_closed();
                self.remove_if_sender_closed(jid);
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
            self.presence_states.remove(jid);
            debug!(jid = %jid, "Evicted stale owned closed connection entry");
        }
    }
}
