//! Extension room sends run on the node that owns the room actor (#1893).
//!
//! The origin forwards the host call; the owner runs the unchanged local send
//! path. A forwarded call enters at [`relayed_room_send`], which never
//! forwards again.
use std::sync::Arc;

use waddle_extensions::{host_tools as ext_host, DisplayText, StanzaId};
use waddle_xmpp::ownership::NodeIdentity;

use crate::clustering::relay::{
    RelayAskError, RelayExtensionRoomSend, RelayExtensionRoomSendReply, RelaySendEffect,
};
use crate::server::routes::{interpret::effects::PlanFailure, websocket::WebSocketState};

use super::{
    conversions::host_tool_error, interpret, ExtensionHostAdapter, ExtensionHostAdapterError,
    ExtensionInvocation, HostMessageTarget, HostSendMessage,
};

#[cfg(test)]
pub(crate) type TestRoomOwnerRelay = Arc<
    dyn Fn(
            NodeIdentity,
            RelayExtensionRoomSend,
        ) -> futures::future::BoxFuture<
            'static,
            Result<RelayExtensionRoomSendReply, RelayAskError>,
        > + Send
        + Sync,
>;

#[cfg(test)]
tokio::task_local! {
    /// Replaces only the transport hop to the owner.
    pub(crate) static TEST_ROOM_OWNER_RELAY: TestRoomOwnerRelay;
}

impl ExtensionHostAdapter {
    pub(super) async fn send_room_routed(
        &self,
        invocation: &ExtensionInvocation,
        send: RelayExtensionRoomSend,
    ) -> Result<StanzaId, ext_host::HostToolError> {
        let mut reresolved = false;
        loop {
            let owner = interpret::extension_room_owner(&self.state, &send.room)
                .await
                .map_err(|failure| host_tool_error(ExtensionHostAdapterError::Plan(failure)))?;
            let reply = match owner {
                None => local_reply(self.send_message(invocation, host_request(&send)).await),
                Some(owner) => self.ask_room_owner(&owner, &send).await?,
            };
            match reply {
                RelayExtensionRoomSendReply::Sent(id) => return Ok(id),
                RelayExtensionRoomSendReply::HostError(error) => return Err(error),
                // The claim moved after it was read: resolve it once more.
                RelayExtensionRoomSendReply::NotOwner if !reresolved => reresolved = true,
                RelayExtensionRoomSendReply::NotOwner => {
                    return Err(temporary_failure("extension room ownership is moving"));
                }
            }
        }
    }

    async fn ask_room_owner(
        &self,
        owner: &NodeIdentity,
        send: &RelayExtensionRoomSend,
    ) -> Result<RelayExtensionRoomSendReply, ext_host::HostToolError> {
        let result = match self.relay_room_send(owner, send.clone()).await {
            // The same offered id resolves a committed first attempt through
            // the owner's ingress origin alias instead of sending twice.
            Err(RelayAskError::Send {
                effect: RelaySendEffect::MaybeCommitted,
                ..
            }) => self.relay_room_send(owner, send.clone()).await,
            result => result,
        };
        result.map_err(|error| {
            tracing::warn!(
                room = %send.room,
                owner = %owner.node_id,
                %error,
                "extension room send could not reach the room owner"
            );
            temporary_failure("extension room owner is unavailable")
        })
    }

    async fn relay_room_send(
        &self,
        owner: &NodeIdentity,
        send: RelayExtensionRoomSend,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        #[cfg(test)]
        if let Ok(relay) = TEST_ROOM_OWNER_RELAY.try_with(Arc::clone) {
            return relay(owner.clone(), send).await;
        }
        let Some(bridge) = self
            .state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return Err(RelayAskError::NotFound {
                node_id: crate::clustering::NodeId::new(owner.node_id.clone()),
            });
        };
        bridge.extension_room_send_remote(owner, send).await
    }
}

/// The owner's entry for a forwarded send.
pub(crate) async fn relayed_room_send(
    state: Arc<WebSocketState>,
    send: RelayExtensionRoomSend,
) -> RelayExtensionRoomSendReply {
    let adapter = ExtensionHostAdapter::new(state);
    match adapter.invocation_for_context(&send.context).await {
        Ok(invocation) => local_reply(adapter.send_message(&invocation, host_request(&send)).await),
        Err(error) => RelayExtensionRoomSendReply::HostError(error),
    }
}

fn local_reply(result: Result<StanzaId, ExtensionHostAdapterError>) -> RelayExtensionRoomSendReply {
    match result {
        Ok(id) => RelayExtensionRoomSendReply::Sent(id),
        Err(ExtensionHostAdapterError::Plan(PlanFailure::RoomOwnedRemotely)) => {
            RelayExtensionRoomSendReply::NotOwner
        }
        Err(error) => RelayExtensionRoomSendReply::HostError(host_tool_error(error)),
    }
}

fn host_request(send: &RelayExtensionRoomSend) -> HostSendMessage {
    HostSendMessage {
        target: HostMessageTarget::Room(send.room.clone()),
        stanza_id: send.offered_id.clone(),
        body: send.body.as_str().to_owned(),
        thread_id: send.thread_id.clone(),
        reply_to: send.reply_to.clone(),
        markup: send.markup.clone(),
        extensions: send.extensions.clone(),
    }
}

pub(crate) fn temporary_failure(message: &'static str) -> ext_host::HostToolError {
    ext_host::HostToolError {
        code: ext_host::HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new(message).expect("static text"),
    }
}
