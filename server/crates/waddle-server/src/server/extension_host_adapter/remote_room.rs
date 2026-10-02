//! Extension room sends run on the node that owns the room actor (#1893).
//!
//! The origin forwards the host call; the owner runs the unchanged local send
//! path. A forwarded call enters at [`relayed_room_send`], which never
//! forwards again.
use std::{sync::Arc, time::Duration};

use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use jid::BareJid;
use waddle_extensions::{host_tools as ext_host, DisplayText, PluginId, StanzaId};
use waddle_xmpp::ownership::NodeIdentity;

use crate::clustering::relay::{
    RelayAskError, RelayExtensionRoomSend, RelayExtensionRoomSendReply, RelayHandle,
    RelaySendEffect,
};
use crate::server::routes::{interpret::effects::PlanFailure, websocket::WebSocketState};

use super::{
    conversions::host_tool_error, interpret, ExtensionHostAdapter, ExtensionHostAdapterError,
    ExtensionInvocation, HostMessageTarget, HostSendMessage,
};

/// One owner ask. Extension commands run inline under the 15 s stanza
/// handler backstop, so the relay lookup backoff (2.1 s), the ask and its one
/// re-ask must fit inside it: 2.1 s + 2 × (1 s + 4 s) ≈ 12 s.
pub(crate) const EXTENSION_ROOM_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const EXTENSION_ROOM_REPLY_TIMEOUT: Duration = Duration::from_secs(4);
/// The owner begins its commit only while the commit and the settlement wait
/// still fit before the origin stops waiting for the reply.
const OWNER_COMMIT_ALLOWANCE: Duration = Duration::from_millis(500);
pub(super) const OWNER_COMMIT_BUDGET: Duration = EXTENSION_ROOM_REPLY_TIMEOUT
    .saturating_sub(super::settlement::SETTLEMENT_RESPONSE_DEADLINE)
    .saturating_sub(OWNER_COMMIT_ALLOWANCE);
/// The origin's re-ask arrives within one ask of the first attempt.
const RELAYED_OUTCOME_RETENTION: Duration = EXTENSION_ROOM_MAILBOX_TIMEOUT
    .saturating_add(EXTENSION_ROOM_REPLY_TIMEOUT)
    .saturating_mul(2);

type RelayedFlight = Shared<BoxFuture<'static, RelayExtensionRoomSendReply>>;

/// Forwarded sends on this owner, by offered id. A re-ask shares the first
/// attempt's outcome instead of planning again under a fresh deadline, so it
/// can never report a failure for a send the first attempt committed.
#[derive(Default)]
pub struct RelayedRoomSends(
    dashmap::DashMap<(PluginId, BareJid, StanzaId), (RelayedFlight, tokio::time::Instant)>,
);

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
                None => local_reply(
                    self.send_message(invocation, host_request(&send, None))
                        .await,
                ),
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
        let mut handle = None;
        let mut result = self.relay_room_send(&mut handle, owner, send.clone()).await;
        if matches!(
            result,
            Err(RelayAskError::Send {
                effect: RelaySendEffect::MaybeCommitted,
                ..
            })
        ) {
            // The same offered id on the resolved relay: the owner answers
            // with the first attempt's outcome instead of sending twice.
            result = self.relay_room_send(&mut handle, owner, send.clone()).await;
        }
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
        handle: &mut Option<RelayHandle>,
        owner: &NodeIdentity,
        send: RelayExtensionRoomSend,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        #[cfg(test)]
        if let Ok(relay) = TEST_ROOM_OWNER_RELAY.try_with(Arc::clone) {
            return relay(owner.clone(), send).await;
        }
        let handle = match handle {
            Some(handle) => handle,
            None => {
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
                handle.insert(bridge.extension_room_handle(owner))
            }
        };
        handle.extension_room_send(send).await
    }
}

/// The owner's entry for a forwarded send.
pub(crate) async fn relayed_room_send(
    state: Arc<WebSocketState>,
    send: RelayExtensionRoomSend,
) -> RelayExtensionRoomSendReply {
    relayed_room_send_within(state, send, OWNER_COMMIT_BUDGET).await
}

pub(super) async fn relayed_room_send_within(
    state: Arc<WebSocketState>,
    send: RelayExtensionRoomSend,
    commit_budget: Duration,
) -> RelayExtensionRoomSendReply {
    let flights = &state.deps.protocol.extension_bot_rooms.relayed.0;
    flights.retain(|_, (flight, started)| {
        flight.peek().is_none() || started.elapsed() < RELAYED_OUTCOME_RETENTION
    });
    let key = (
        send.context.plugin_id.clone(),
        send.room.clone(),
        send.offered_id.clone(),
    );
    let flight = flights
        .entry(key)
        .or_insert_with(|| {
            let commit_deadline = tokio::time::Instant::now() + commit_budget;
            let attempt = owner_send(Arc::clone(&state), send, commit_deadline);
            (attempt.boxed().shared(), tokio::time::Instant::now())
        })
        .0
        .clone();
    flight.await
}

async fn owner_send(
    state: Arc<WebSocketState>,
    send: RelayExtensionRoomSend,
    commit_deadline: tokio::time::Instant,
) -> RelayExtensionRoomSendReply {
    let adapter = ExtensionHostAdapter::new(state);
    match adapter.invocation_for_context(&send.context).await {
        Ok(invocation) => local_reply(
            adapter
                .send_message(&invocation, host_request(&send, Some(commit_deadline)))
                .await,
        ),
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

fn host_request(
    send: &RelayExtensionRoomSend,
    commit_deadline: Option<tokio::time::Instant>,
) -> HostSendMessage {
    HostSendMessage {
        target: HostMessageTarget::Room(send.room.clone()),
        stanza_id: send.offered_id.clone(),
        body: send.body.as_str().to_owned(),
        thread_id: send.thread_id.clone(),
        reply_to: send.reply_to.clone(),
        markup: send.markup.clone(),
        extensions: send.extensions.clone(),
        commit_deadline,
    }
}

pub(crate) fn temporary_failure(message: &'static str) -> ext_host::HostToolError {
    ext_host::HostToolError {
        code: ext_host::HostToolErrorCode::TemporaryFailure,
        message: DisplayText::new(message).expect("static text"),
    }
}
