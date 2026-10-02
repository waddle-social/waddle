use super::*;
use crate::clustering::relay::{RelayExtensionRoomSend, RelayExtensionRoomSendReply};

impl OrderedRelayDeliveryBridge {
    /// Origin side of a forwarded extension room send (#1893). The ask and
    /// its re-ask share this handle, so the re-ask skips the relay lookup.
    pub(crate) fn extension_room_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(
                crate::server::extension_host_adapter::EXTENSION_ROOM_MAILBOX_TIMEOUT,
                crate::server::extension_host_adapter::EXTENSION_ROOM_REPLY_TIMEOUT,
            )
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
