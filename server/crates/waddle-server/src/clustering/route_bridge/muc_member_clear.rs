use super::*;
use crate::clustering::relay::{RelayMucMemberClear, RelayMucMemberClearReply};

impl OrderedRelayDeliveryBridge {
    /// Origin side of a forwarded room member clear.
    pub(crate) fn muc_member_clear_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(
                crate::admin::bot_affiliations::MUC_MEMBER_CLEAR_MAILBOX_TIMEOUT,
                crate::admin::bot_affiliations::MUC_MEMBER_CLEAR_REPLY_TIMEOUT,
            )
    }

    /// Owner side: check or apply through the local room actor. It never
    /// forwards again.
    pub(crate) async fn muc_member_clear_local(
        &self,
        message: RelayMucMemberClear,
    ) -> RelayMucMemberClearReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::admin::bot_affiliations::relayed_member_clear(&state, message).await
            }
            // A node still wiring its services owns no live room actor yet.
            None => RelayMucMemberClearReply::NotOwner,
        }
    }
}
