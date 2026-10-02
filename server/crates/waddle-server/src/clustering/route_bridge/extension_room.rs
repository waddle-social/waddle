use super::*;
use crate::clustering::relay::{RelayExtensionRoomSend, RelayExtensionRoomSendReply};

impl OrderedRelayDeliveryBridge {
    /// Origin side of a forwarded extension room send (#1893).
    pub(crate) async fn extension_room_send_remote(
        &self,
        owner: &NodeIdentity,
        message: RelayExtensionRoomSend,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout)
            .extension_room_send(message)
            .await
    }

    /// Owner side: run the local host send. It never forwards again.
    pub(crate) async fn extension_room_send_local(
        &self,
        message: RelayExtensionRoomSend,
    ) -> RelayExtensionRoomSendReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::server::extension_host_adapter::relayed_room_send(state, message).await
            }
            None => RelayExtensionRoomSendReply::HostError(
                crate::server::extension_host_adapter::temporary_failure(
                    "extension room owner is starting up",
                ),
            ),
        }
    }
}
