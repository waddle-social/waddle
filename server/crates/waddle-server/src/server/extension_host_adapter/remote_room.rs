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
    RelaySendEffect, RelaySendFailure,
};
use crate::server::routes::{interpret::effects::PlanFailure, websocket::WebSocketState};

use super::{
    conversions::host_tool_error, interpret, ExtensionHostAdapter, ExtensionHostAdapterError,
    ExtensionInvocation, HostMessageTarget, HostSendMessage,
};

/// Receiver-side windows for one owner ask; kameo enforces them on the owner.
pub(crate) const EXTENSION_ROOM_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const EXTENSION_ROOM_REPLY_TIMEOUT: Duration = Duration::from_secs(4);
/// The origin's own bound on one ask, relay lookup included.
const ASK_LIMIT: Duration = EXTENSION_ROOM_MAILBOX_TIMEOUT
    .saturating_add(EXTENSION_ROOM_REPLY_TIMEOUT)
    .saturating_add(Duration::from_millis(500));
/// The origin's whole wait for an ask and its one re-ask. Extension commands
/// run inline under the 15 s stanza-handler backstop.
const ORIGIN_BUDGET: Duration = ASK_LIMIT.saturating_mul(2);
const OWNER_COMMIT_ALLOWANCE: Duration = Duration::from_millis(500);
/// Of the origin's remaining wait, the owner keeps this much for mailbox
/// admission, the commit and the settlement wait.
pub(super) const OWNER_REPLY_RESERVE: Duration = EXTENSION_ROOM_MAILBOX_TIMEOUT
    .saturating_add(super::settlement::SETTLEMENT_RESPONSE_DEADLINE)
    .saturating_add(OWNER_COMMIT_ALLOWANCE);
/// Within one ask, the owner begins its commit early enough for the reply to
/// fit the ask's reply window.
const OWNER_COMMIT_BUDGET: Duration = EXTENSION_ROOM_REPLY_TIMEOUT
    .saturating_sub(super::settlement::SETTLEMENT_RESPONSE_DEADLINE)
    .saturating_sub(OWNER_COMMIT_ALLOWANCE);
/// Any re-ask arrives within the origin's budget of the first attempt.
const RELAYED_OUTCOME_RETENTION: Duration = ORIGIN_BUDGET;

type RelayedFlight = Shared<BoxFuture<'static, RelayExtensionRoomSendReply>>;

/// Forwarded sends on this owner, by offered id. A re-ask shares the first
/// attempt's outcome instead of planning again under a fresh deadline, so it
/// can never report a failure for a send the first attempt committed.
#[derive(Default)]
pub struct RelayedRoomSends(
    dashmap::DashMap<(PluginId, BareJid, StanzaId), (RelayedFlight, tokio::time::Instant)>,
);

/// Replaces only the transport hop; `bool` says the relay ref was just
/// looked up.
#[cfg(test)]
pub(crate) type TestRoomOwnerRelay = Arc<
    dyn Fn(
            NodeIdentity,
            RelayExtensionRoomSend,
            bool,
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
        // One bound for every ask of this host call, re-resolution included.
        let deadline = tokio::time::Instant::now() + ORIGIN_BUDGET;
        let mut reresolved = false;
        loop {
            let owner = interpret::extension_room_owner(&self.state, &send.room)
                .await
                .map_err(|failure| host_tool_error(ExtensionHostAdapterError::Plan(failure)))?;
            let reply = match owner {
                // A re-resolved local send still answers within this host
                // call's wait; it may not commit after the caller gave up.
                None => local_reply(
                    self.send_message(
                        invocation,
                        host_request(&send, reresolved.then(|| deadline - OWNER_REPLY_RESERVE)),
                    )
                    .await,
                ),
                Some(owner) => self.ask_room_owner(&owner, &send, deadline).await?,
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
        deadline: tokio::time::Instant,
    ) -> Result<RelayExtensionRoomSendReply, ext_host::HostToolError> {
        let mut handle = None;
        let mut result = self.ask_once(&mut handle, owner, send, deadline).await;
        if let Err(RelayAskError::Send {
            failure, effect, ..
        }) = &result
        {
            let stale = *failure == RelaySendFailure::StaleRef;
            if stale || *effect == RelaySendEffect::MaybeCommitted {
                // A dead relay ref resolves again. The same offered id makes
                // the owner answer with the first attempt's outcome, if any.
                if stale {
                    handle = None;
                }
                result = self.ask_once(&mut handle, owner, send, deadline).await;
            }
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

    /// kameo bounds the mailbox and reply only on the owner, so the origin
    /// bounds its own wait. Expiry may follow delivery: it is maybe-committed.
    async fn ask_once(
        &self,
        handle: &mut Option<RelayHandle>,
        owner: &NodeIdentity,
        send: &RelayExtensionRoomSend,
        deadline: tokio::time::Instant,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        let limit = deadline.min(tokio::time::Instant::now() + ASK_LIMIT);
        tokio::time::timeout_at(
            limit,
            self.relay_room_send(handle, owner, send.clone(), deadline),
        )
        .await
        .unwrap_or_else(|_| {
            Err(RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                effect: RelaySendEffect::MaybeCommitted,
                message: "the room owner did not answer in time".to_owned(),
            })
        })
    }

    async fn relay_room_send(
        &self,
        handle: &mut Option<RelayHandle>,
        owner: &NodeIdentity,
        send: RelayExtensionRoomSend,
        deadline: tokio::time::Instant,
    ) -> Result<RelayExtensionRoomSendReply, RelayAskError> {
        #[cfg(test)]
        let fresh = handle.is_none();
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
        #[cfg(test)]
        if let Ok(relay) = TEST_ROOM_OWNER_RELAY.try_with(Arc::clone) {
            let mut send = send;
            send.origin_budget = deadline.saturating_duration_since(tokio::time::Instant::now());
            return relay(owner.clone(), send, fresh).await;
        }
        handle.extension_room_send(send, deadline).await
    }
}

/// The owner's entry for a forwarded send.
pub(crate) async fn relayed_room_send(
    state: Arc<WebSocketState>,
    send: RelayExtensionRoomSend,
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
            let started = tokio::time::Instant::now();
            // Begin the commit only while the reply can reach a waiting origin.
            let budget =
                OWNER_COMMIT_BUDGET.min(send.origin_budget.saturating_sub(OWNER_REPLY_RESERVE));
            let attempt = owner_send(Arc::clone(&state), send, started + budget);
            (attempt.boxed().shared(), started)
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
