use super::{NodeId, NodeIdentity, OrderedRelayDeliveryBridge, RelayHandle};
use crate::clustering::relay::{RelayRemoteRoomRequest, RelayRemoteRoomRequestReply};
use crate::server::routes::websocket::REMOTE_ROOM_REQUEST_MAILBOX_TIMEOUT;
use crate::server::routes::websocket::REMOTE_ROOM_REQUEST_REPLY_TIMEOUT;

impl OrderedRelayDeliveryBridge {
    /// Origin side of a remote principal's stanza carried to the room
    /// owner (ADR-0020 D10).
    pub(crate) fn remote_room_request_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(
                REMOTE_ROOM_REQUEST_MAILBOX_TIMEOUT,
                REMOTE_ROOM_REQUEST_REPLY_TIMEOUT,
            )
    }

    /// Owner side: queue the stanza for the local room host. It is never
    /// carried again.
    pub(crate) async fn remote_room_request_local(
        &self,
        message: RelayRemoteRoomRequest,
    ) -> RelayRemoteRoomRequestReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::server::routes::websocket::relayed_remote_room_request(&state, message).await
            }
            // A node still wiring its services owns no live room actor yet.
            None => RelayRemoteRoomRequestReply::NotOwner,
        }
    }
}
