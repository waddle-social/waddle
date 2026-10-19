use super::*;
use crate::clustering::relay::{RelayMucAdminQuery, RelayMucAdminQueryReply};

impl OrderedRelayDeliveryBridge {
    /// Origin side of a forwarded muc#admin list GET (#1298).
    pub(crate) fn muc_admin_query_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(
                crate::server::routes::websocket::MUC_ADMIN_QUERY_MAILBOX_TIMEOUT,
                crate::server::routes::websocket::MUC_ADMIN_QUERY_REPLY_TIMEOUT,
            )
    }

    /// Owner side: answer from the local room actor. It never forwards again.
    pub(crate) async fn muc_admin_query_local(
        &self,
        message: RelayMucAdminQuery,
    ) -> RelayMucAdminQueryReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::server::routes::websocket::relayed_muc_admin_query(&state, message).await
            }
            // A node still wiring its services owns no live room actor yet.
            None => RelayMucAdminQueryReply::NotOwner,
        }
    }
}
