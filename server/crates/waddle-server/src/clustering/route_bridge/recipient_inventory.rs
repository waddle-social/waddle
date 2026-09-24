use super::*;
use crate::server::routes::interpret::{local_recipient_inventory, RecipientInventory};

impl OrderedRelayDeliveryBridge {
    /// A failed or moved owner cannot be interpreted as an offline recipient.
    pub(crate) async fn recipient_inventory_remote(
        &self,
        target: &jid::BareJid,
    ) -> Result<Option<RecipientInventory>, ()> {
        let services = self.services.get().ok_or(())?;
        let entity = Entity::new(EntityType::UserActor, target.to_string());
        let Some(claim) = services
            .claim_store
            .current_claim(&entity)
            .await
            .map_err(|_| ())?
        else {
            return Ok(None);
        };
        if !claim.owner_lease_fresh {
            return Err(());
        }
        if claim.owner == services.node_identity.current() {
            return Ok(None);
        }
        let mut handle = RelayHandle::new(
            NodeId::new(claim.owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let reply = handle
            .recipient_inventory(target.clone(), claim.claim_epoch)
            .await
            .map_err(|_| ())?;
        match reply {
            super::super::relay::RelayRecipientInventoryReply::Inventory(inventory) => {
                Ok(Some(inventory))
            }
            super::super::relay::RelayRecipientInventoryReply::Unavailable => Err(()),
        }
    }

    pub(crate) async fn recipient_inventory_local(
        &self,
        target: &jid::BareJid,
        epoch: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<RecipientInventory, ()> {
        let services = self.services.get().ok_or(())?;
        let entity = Entity::new(EntityType::UserActor, target.to_string());
        let owns = |claim: &ClaimSnapshot| {
            claim.owner_lease_fresh
                && claim.owner == services.node_identity.current()
                && claim.claim_epoch == epoch
        };
        if !services
            .claim_store
            .current_claim(&entity)
            .await
            .map_err(|_| ())?
            .as_ref()
            .is_some_and(owns)
        {
            return Err(());
        }
        let state = services.web_socket_state.upgrade().ok_or(())?;
        let deps = build_interpret_deps(state.as_ref(), None);
        let inventory = local_recipient_inventory(&deps, target).await?;
        if !services
            .claim_store
            .current_claim(&entity)
            .await
            .map_err(|_| ())?
            .as_ref()
            .is_some_and(owns)
        {
            return Err(());
        }
        Ok(inventory)
    }
}
