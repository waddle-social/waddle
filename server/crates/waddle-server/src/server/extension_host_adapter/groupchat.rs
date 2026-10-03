//! Trusted bot sends commit under the plugin's configured grant.
use std::{sync::Arc, time::Duration};

use jid::{BareJid, FullJid};
use tokio::{sync::OwnedMutexGuard, task::JoinHandle};
use waddle_extensions::{host_tools::InvocationKind, PluginId, StanzaId};
use waddle_xmpp::ingress::{NormalizedTarget, TransportGeneration};

use crate::ingress::{
    nested::NestedContinuation, ExtensionPrincipal, IngressPrincipal, IngressStreamIdentity,
    IngressSubmission,
};
use crate::server::routes::websocket::WebSocketState;

use super::{interpret, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation};

/// How long a bot stays in a room after its last send. A send within the
/// window reuses the occupancy, so occupants see one join, the bot's
/// messages, then one leave, instead of a join and leave around each post.
pub(super) const BOT_LINGER: Duration = Duration::from_secs(60);

/// The longest a leave waits for the bot's join to reach occupants on other
/// nodes. A healthy peer takes milliseconds; a hung route must not keep the
/// bot in the room forever.
const JOIN_STRAGGLER_WAIT: Duration = Duration::from_secs(10);

/// What a bot's sends into one room hand on to one another: the occupancy
/// still to leave and the work that leave waits for.
#[derive(Default)]
pub(super) struct HeldBot {
    occupancy: interpret::BotOccupancy,
    /// The authority-owned work (commit, delivery, settlement) of the sends
    /// that used the occupancy, registered as it starts. The next send
    /// commits, and the bot leaves, only after it, so occupants get the
    /// bot's messages in order and before its unavailable.
    work: Vec<JoinHandle<()>>,
    /// Bumped by each send that ends holding the occupancy. A scheduled
    /// leave that finds it moved on leaves the bot to the newer send's leave.
    generation: u64,
}

impl HeldBot {
    /// Waits for the earlier sends' work, which its settlement budget bounds.
    /// Cancelled, it keeps the work for the next wait; a handle that wait
    /// already consumed is dropped first, as polling it again panics.
    async fn delivered(&mut self) {
        self.work.retain(|work| !work.is_finished());
        futures::future::join_all(self.work.iter_mut()).await;
        self.work.clear();
    }
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
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Lets the linger window pass, then waits until the last send of
    /// `plugin` into `room` has left.
    #[cfg(test)]
    pub(super) async fn settled(&self, plugin: &PluginId, room: &BareJid) {
        linger_passes().await;
        let key = (plugin.clone(), room.clone());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.entries.get(&key).is_none_or(|lock| {
                lock.try_lock()
                    .is_ok_and(|held| held.occupancy.held.is_none())
            }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the bot's leave finishes");
    }
}

/// Moves the clock past the linger window of every send so far.
#[cfg(test)]
pub(super) async fn linger_passes() {
    tokio::time::pause();
    tokio::time::advance(BOT_LINGER).await;
    tokio::time::resume();
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
        // Keep the bot's join, sends and leave ordered for this bot/room: the
        // actor's admission generation must not change under a second join.
        // Once locked, the guard schedules the bot's leave however this send
        // ends, cancelled included. A send cancelled while queued used
        // nothing; the leave the last send scheduled still runs.
        let mut cleanup = BotRoomCleanup {
            held: self
                .state
                .deps
                .protocol
                .extension_bot_rooms
                .lock(&invocation.plugin_id, &room)
                .await,
            state: Arc::clone(&self.state),
            plugin: invocation.plugin_id.clone(),
            room: room.clone(),
            sender: sender.clone(),
        };
        let held = &mut *cleanup.held;
        let result = async {
            held.delivered().await;
            // A forwarded send whose origin has stopped waiting must not commit:
            // the origin would report a failure for a message that was posted.
            // Time spent on authorization, this lock and the earlier sends'
            // delivery counts, so a send that already expired never joins.
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
            // The work is held before the decision is awaited: a send
            // cancelled meanwhile may still post, and the leave waits for it.
            let mut outcome = operation
                .commit_tracked(submission, continuation, &mut held.work)
                .await;
            let archive_ids = super::settlement::finish_nested(&mut outcome).await?;
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

/// Schedules the leave of the bot occupancy a dispatch used once the
/// dispatch ends, replied or cancelled. The leave runs [`BOT_LINGER`] later
/// unless a newer send used the occupancy by then; it never delays the reply.
struct BotRoomCleanup {
    /// The bot/room lock, released when the dispatch ends.
    held: OwnedMutexGuard<HeldBot>,
    state: Arc<WebSocketState>,
    plugin: PluginId,
    room: BareJid,
    sender: FullJid,
}

impl Drop for BotRoomCleanup {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.held.occupancy.held.is_none() {
            return;
        }
        self.held.generation = self.held.generation.wrapping_add(1);
        let generation = self.held.generation;
        // ponytail: one sleeping task per send, superseded ones wake to no-op;
        // keep one timer per bot/room if bots post in large bursts.
        let deadline = tokio::time::Instant::now() + BOT_LINGER;
        let state = Arc::clone(&self.state);
        let (plugin, room, sender) = (self.plugin.clone(), self.room.clone(), self.sender.clone());
        let cleanup = async move {
            tokio::time::sleep_until(deadline).await;
            let held = state
                .deps
                .protocol
                .extension_bot_rooms
                .lock(&plugin, &room)
                .await;
            // A newer send used the occupancy; its own leave follows.
            if held.generation == generation {
                leave(held, &state, &room, &sender).await;
            }
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

/// Leaves the held occupancy once occupants have everything sent before it:
/// the bot's messages, and on other nodes its join, or they keep a ghost bot.
async fn leave(
    mut held: OwnedMutexGuard<HeldBot>,
    state: &WebSocketState,
    room: &BareJid,
    sender: &FullJid,
) {
    held.delivered().await;
    let occupancy = &mut held.occupancy;
    // A cancelled send may have consumed some of these; polling one again panics.
    occupancy
        .join_stragglers
        .retain(|route| !route.is_finished());
    if tokio::time::timeout(
        JOIN_STRAGGLER_WAIT,
        futures::future::join_all(occupancy.join_stragglers.iter_mut()),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            room = %room,
            "Extension bot join presence is still routing to another node; leaving anyway"
        );
    }
    occupancy
        .join_stragglers
        .retain(|route| !route.is_finished());
    if let Some((nick, session)) = occupancy.held.take() {
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
