use super::*;

impl OrderedRelayDeliveryBridge {
    pub(in super::super) async fn lock_for_channel(
        &self,
        channel: &OrderedRelayChannel,
    ) -> Option<Arc<Mutex<()>>> {
        let mut locks = self.channel_locks.lock().await;
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            tracing::warn!(
                limit = MAX_ORDERED_RELAY_CHANNEL_LOCKS,
                "ordered relay: channel lock map is full"
            );
            return None;
        }
        Some(
            locks
                .entry(channel.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
        )
    }

    pub(in super::super) async fn remove_channel_lock_if_unused(
        &self,
        channel: &OrderedRelayChannel,
        lock: &Arc<Mutex<()>>,
    ) {
        let mut locks = self.channel_locks.lock().await;
        if locks
            .get(channel)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(channel);
        }
    }

    pub(in super::super) fn sign_envelope(
        &self,
        envelope: &mut RemoteStanzaEnvelope,
    ) -> Result<(), ()> {
        let Some(signer) = self.origin_signer.get() else {
            tracing::warn!("ordered relay: origin signer is not wired; dropping envelope");
            return Err(());
        };
        let signing_bytes = envelope.signing_bytes().map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: failed to serialize envelope signing bytes"
            );
        })?;
        let signature = signer.keypair.sign(&signing_bytes).map_err(|error| {
            tracing::warn!(%error, "ordered relay: failed to sign envelope");
        })?;
        envelope.origin_proof = Some(OrderedRelayOriginProof {
            public_key: signer.public_key.clone(),
            signature,
        });
        Ok(())
    }

    pub(in super::super) async fn divert_channel(
        &self,
        channel: OrderedRelayChannel,
        reason: OrderedRelayDiversionReason,
    ) {
        self.sender_state
            .lock()
            .await
            .divert(OrderedRelayDiversion { channel, reason });
    }

    pub(in super::super) async fn forget_channel(&self, channel: &OrderedRelayChannel) {
        self.sender_state.lock().await.forget_channel(channel);
    }

    pub(in super::super) async fn apply_nack_channel_action(
        &self,
        envelope: &RemoteStanzaEnvelope,
        action: NackChannelAction,
    ) {
        match action {
            NackChannelAction::Divert(reason) => {
                self.divert_channel(envelope.channel.clone(), reason).await;
            }
            NackChannelAction::Forget => self.forget_channel(&envelope.channel).await,
            NackChannelAction::Keep => {}
            NackChannelAction::Rollback => {
                self.sender_state
                    .lock()
                    .await
                    .rollback_unseen_envelope(envelope);
            }
        }
    }
}
