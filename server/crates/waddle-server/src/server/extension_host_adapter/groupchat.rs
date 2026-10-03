//! Trusted bot sends commit under the plugin's configured grant.
use std::sync::Arc;

use jid::{BareJid, FullJid};
use tokio::sync::OwnedMutexGuard;
use waddle_extensions::{host_tools::InvocationKind, PluginId, StanzaId};
use waddle_xmpp::ingress::{NormalizedTarget, TransportGeneration};

use crate::ingress::{
    nested::{NestedContinuation, NestedOutcome, SettlementOutcome},
    ExtensionPrincipal, IngressPrincipal, IngressStreamIdentity, IngressSubmission,
};
use crate::server::routes::websocket::WebSocketState;

use super::{interpret, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation};

/// What a bot's sends into one room hand on to one another: the occupancy
/// still to leave and the work that leave waits for.
#[derive(Default)]
pub(super) struct HeldBot {
    occupancy: interpret::BotOccupancy,
    /// Committed sends still settling: their messages reach occupants before
    /// the bot's unavailable.
    settlements: Vec<tokio::task::JoinHandle<SettlementOutcome>>,
}

/// Serializes the synthetic actor lifecycle across independently created adapters.
#[derive(Default)]
pub struct BotRoomLocks {
    entries: dashmap::DashMap<(PluginId, BareJid), Arc<tokio::sync::Mutex<HeldBot>>>,
    #[cfg(feature = "clustering")]
    pub(super) relayed: super::remote_room::RelayedRoomSends,
}

impl BotRoomLocks {
    async fn lock(&self, plugin: &PluginId, room: &BareJid) -> OwnedMutexGuard<HeldBot> {
        let lock = self
            .entries
            .entry((plugin.clone(), room.clone()))
            .or_insert_with(Default::default)
            .clone();
        lock.lock_owned().await
    }

    /// Waits until the last send of `plugin` into `room` has left. It polls:
    /// queueing on the lock would be a successor the bot is handed to.
    #[cfg(test)]
    pub(super) async fn settled(&self, plugin: &PluginId, room: &BareJid) {
        let key = (plugin.clone(), room.clone());
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !self.entries.get(&key).is_none_or(|lock| {
                lock.try_lock()
                    .is_ok_and(|held| held.occupancy.held.is_none())
            }) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the bot's leave finishes");
    }
}

impl ExtensionHostAdapter {
    pub(super) async fn dispatch_groupchat(
        &self,
        invocation: &ExtensionInvocation,
        room: BareJid,
        response: interpret::ExtensionRoomMessage,
        commit_deadline: Option<tokio::time::Instant>,
    ) -> Result<StanzaId, ExtensionHostAdapterError> {
        let offered_id = response.stanza_id.clone().ok_or_else(|| {
            ExtensionHostAdapterError::Protocol("host message has no offered stanza id".to_owned())
        })?;
        let authority = &self.state.deps.protocol.ingress;
        let provider = invocation.kind == InvocationKind::ProviderWebhook;
        let grant = if provider {
            authority
                .active_extension_room_grant(&invocation.plugin_id, &room)
                .await
        } else {
            authority
                .active_extension_send_grant(&invocation.plugin_id)
                .await
        }
        .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?
        .ok_or(ExtensionHostAdapterError::NotAuthorized)?;
        let operation = authority
            .try_begin_nested()
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        let sender = self.plugin_actor_jid(&invocation.plugin_id)?;
        let requester = (!provider).then(|| invocation.actor_jid.to_bare());
        // Created first, so the bot leaves however this send ends, cancelled
        // included.
        let mut cleanup = BotRoomCleanup {
            state: Arc::clone(&self.state),
            plugin: invocation.plugin_id.clone(),
            room: room.clone(),
            sender: sender.clone(),
            held: None,
            outcome: None,
        };
        // Keep the bot's join, send and leave ordered for this bot/room: the
        // actor's admission generation must not change under a second join,
        // and the next send must not find this send's occupancy unless it
        // was handed it.
        let held = cleanup.held.insert(
            self.state
                .deps
                .protocol
                .extension_bot_rooms
                .lock(&invocation.plugin_id, &room)
                .await,
        );
        let settlement = &mut cleanup.outcome;
        let result = async {
            // A forwarded send whose origin has stopped waiting must not commit:
            // the origin would report a failure for a message that was posted.
            // Time spent on authorization and this lock counts, so a send that
            // already expired never joins the bot.
            let expired =
                || commit_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline);
            if expired() {
                return Err(ExtensionHostAdapterError::DeadlineExceeded);
            }
            let deps = self.interpret_deps(invocation.session.as_ref());
            let planned = interpret::plan_extension_bot_groupchat(
                &deps,
                room.clone(),
                sender.clone(),
                response,
                &self.state,
                commit_deadline,
                &mut held.occupancy,
            )
            .await
            .map_err(|error| match error {
                interpret::ExtensionBotDispatchError::InvalidEnvelope
                | interpret::ExtensionBotDispatchError::BotOutcast
                | interpret::ExtensionBotDispatchError::GroupDm => {
                    ExtensionHostAdapterError::NotAuthorized
                }
                interpret::ExtensionBotDispatchError::Plan(failure) => {
                    ExtensionHostAdapterError::Plan(failure)
                }
                interpret::ExtensionBotDispatchError::Digest(error) => {
                    ExtensionHostAdapterError::Unsupported(error.to_string())
                }
                interpret::ExtensionBotDispatchError::RoomNotRegistered => {
                    ExtensionHostAdapterError::RoomNotFound(room.clone())
                }
                other => ExtensionHostAdapterError::Protocol(other.to_string()),
            })?;
            if let Some(failure) = planned.plan.failure {
                return Err(ExtensionHostAdapterError::Plan(failure));
            }
            if expired() {
                return Err(ExtensionHostAdapterError::DeadlineExceeded);
            }
            let submission = IngressSubmission {
                identity: IngressStreamIdentity::Extension {
                    plugin: invocation.plugin_id.clone(),
                    requester: requester.clone(),
                },
                principal: IngressPrincipal::Extension(ExtensionPrincipal {
                    grant,
                    requester,
                    sender: sender.to_bare(),
                }),
                sender: sender.clone(),
                target: NormalizedTarget::Bare(room.clone()),
                digest_input: planned.digest_input,
                plan: planned.plan,
                connection_generation: TransportGeneration::Host,
            };
            let continuation = NestedContinuation::new(
                Arc::clone(&self.state),
                invocation.session.clone(),
                submission.sender.clone(),
            );
            let outcome = settlement.insert(
                operation
                    .commit_and_continue(submission, continuation)
                    .await,
            );
            let archive_ids = super::settlement::finish_nested(outcome).await?;
            Ok(archive_ids
                .into_iter()
                .find(|(archive, _)| archive == &room)
                .and_then(|(_, id)| StanzaId::new(id.id).ok()))
        }
        .await;
        drop(cleanup);
        // A committed denial may have only an error frame and no room archive.
        // If its settlement misses the response deadline, acceptance still uses
        // the offered ID. Successful room sends retain their canonical reply ID.
        result.map(|id| id.unwrap_or(offered_id))
    }
}

/// Leaves the bot occupancy a dispatch used once the dispatch ends, replied
/// or cancelled: dropping it spawns the leave, so the leave never delays the
/// reply.
struct BotRoomCleanup {
    state: Arc<WebSocketState>,
    plugin: PluginId,
    room: BareJid,
    sender: FullJid,
    /// The bot/room lock, once acquired. It is held until the bot has left,
    /// so the next send joins afresh, or is handed to a queued send.
    held: Option<OwnedMutexGuard<HeldBot>>,
    /// This dispatch's commit, whose settlement the leave waits for.
    outcome: Option<NestedOutcome>,
}

impl Drop for BotRoomCleanup {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut held = self.held.take();
        if let (Some(held), Some(NestedOutcome::Committed { settlement, .. })) =
            (held.as_mut(), self.outcome.take())
        {
            if !settlement.is_finished() {
                held.settlements.push(settlement);
            }
        }
        let state = Arc::clone(&self.state);
        let (plugin, room, sender) = (self.plugin.clone(), self.room.clone(), self.sender.clone());
        let cleanup = async move {
            // Cancelled while queued: queue on, so whatever the lock is handed
            // is still left.
            let held = match held {
                Some(held) => held,
                None => {
                    state
                        .deps
                        .protocol
                        .extension_bot_rooms
                        .lock(&plugin, &room)
                        .await
                }
            };
            leave(held, &state, &room, &sender).await;
        };
        // A test observing remote delivery keeps observing the detached leave.
        #[cfg(all(test, feature = "clustering"))]
        let cleanup = {
            let controlled = interpret::CONTROLLED_REGISTERED_REMOTE_DELIVERY
                .try_with(Clone::clone)
                .ok();
            async move {
                match controlled {
                    Some(controlled) => {
                        interpret::CONTROLLED_REGISTERED_REMOTE_DELIVERY
                            .scope(controlled, cleanup)
                            .await
                    }
                    None => cleanup.await,
                }
            }
        };
        runtime.spawn(cleanup);
    }
}

/// A queued send of this bot into this room: besides the map's and `held`'s
/// own, each waiter holds a reference to the lock.
fn successor_waiting(held: &OwnedMutexGuard<HeldBot>) -> bool {
    Arc::strong_count(OwnedMutexGuard::mutex(held)) > 2
}

/// Leaves the held occupancy, unless a send of the same bot into the same
/// room is queued: that send takes the occupancy over and leaves it after
/// its own message, instead of waiting for this leave and joining again.
async fn leave(
    mut held: OwnedMutexGuard<HeldBot>,
    state: &WebSocketState,
    room: &BareJid,
    sender: &FullJid,
) {
    if successor_waiting(&held) {
        return;
    }
    // Occupants see the message, and occupants on other nodes the join,
    // before the unavailable, or they keep a ghost bot. Bounded, since a
    // send queued meanwhile waits on it; one still running is left to the
    // next leave.
    let HeldBot {
        occupancy,
        settlements,
    } = &mut *held;
    let _ = tokio::time::timeout(
        super::settlement::SETTLEMENT_RESPONSE_DEADLINE,
        futures::future::join(
            futures::future::join_all(settlements.iter_mut()),
            futures::future::join_all(occupancy.join_stragglers.iter_mut()),
        ),
    )
    .await;
    settlements.retain(|settlement| !settlement.is_finished());
    occupancy
        .join_stragglers
        .retain(|route| !route.is_finished());
    if successor_waiting(&held) {
        return;
    }
    if let Some((nick, session)) = held.occupancy.held.take() {
        // The normal departure path emits unavailable presence and retains
        // interrupted actor cleanup for the departure janitor.
        let _ = crate::server::routes::websocket::handlers::presence::handle_muc_leave(
            state,
            room,
            sender,
            nick.as_str(),
            session,
            None,
        )
        .await;
    }
}
