//! Trusted bot sends commit under the plugin's configured grant.
use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_extensions::{host_tools::InvocationKind, StanzaId};
use waddle_xmpp::ingress::{NormalizedTarget, TransportGeneration};

use crate::ingress::{
    nested::{NestedContinuation, NestedOutcome, SettlementOutcome},
    ExtensionPrincipal, IngressPrincipal, IngressStreamIdentity, IngressSubmission,
};
use crate::server::routes::websocket::WebSocketState;

use super::{interpret, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation};

/// Serializes the synthetic actor lifecycle across independently created adapters.
#[derive(Default)]
pub struct BotRoomLocks {
    entries: dashmap::DashMap<(waddle_extensions::PluginId, BareJid), Arc<tokio::sync::Mutex<()>>>,
    #[cfg(feature = "clustering")]
    pub(super) relayed: super::remote_room::RelayedRoomSends,
}

impl BotRoomLocks {
    pub(super) async fn lock(
        &self,
        plugin: &waddle_extensions::PluginId,
        room: &BareJid,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .entries
            .entry((plugin.clone(), room.clone()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        lock.lock_owned().await
    }

    /// Waits until the last send of `plugin` into `room` has left.
    #[cfg(test)]
    pub(super) async fn settled(&self, plugin: &waddle_extensions::PluginId, room: &BareJid) {
        tokio::time::timeout(std::time::Duration::from_secs(10), self.lock(plugin, room))
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
        // Keep the bot's join, send and leave ordered for this bot/room: the
        // actor's admission generation must not change under a second join,
        // and the next send must not find this send's occupancy.
        let room_guard = self
            .state
            .deps
            .protocol
            .extension_bot_rooms
            .lock(&invocation.plugin_id, &room)
            .await;
        let mut occupancy = interpret::BotOccupancy::default();
        let mut settlement = None;
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
                &mut occupancy,
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
        BotRoomCleanup {
            state: Arc::clone(&self.state),
            room,
            sender,
            occupancy,
            settlement: match settlement {
                Some(NestedOutcome::Committed { settlement, .. }) if !settlement.is_finished() => {
                    Some(settlement)
                }
                _ => None,
            },
        }
        .spawn(room_guard);
        // A committed denial may have only an error frame and no room archive.
        // If its settlement misses the response deadline, acceptance still uses
        // the offered ID. Successful room sends retain their canonical reply ID.
        result.map(|id| id.unwrap_or(offered_id))
    }
}

/// What one dispatch leaves to finish after its reply: the bot occupancy it
/// used is left.
struct BotRoomCleanup {
    state: Arc<WebSocketState>,
    room: BareJid,
    sender: FullJid,
    occupancy: interpret::BotOccupancy,
    /// A committed send's settlement still running: its message reaches
    /// occupants before the bot's unavailable.
    settlement: Option<tokio::task::JoinHandle<SettlementOutcome>>,
}

impl BotRoomCleanup {
    /// Runs detached so the leave never delays the reply. The room guard is
    /// held until the bot has left, so the next send joins afresh.
    fn spawn(self, room_guard: tokio::sync::OwnedMutexGuard<()>) {
        let cleanup = async move {
            if let Some(settlement) = self.settlement {
                let _ = settlement.await;
            }
            // Occupants on other nodes must see the join before its
            // unavailable, or they keep a ghost bot.
            for route in self.occupancy.join_stragglers {
                let _ = route.await;
            }
            if let Some((nick, session)) = self.occupancy.held {
                // The normal departure path emits unavailable presence and
                // retains interrupted actor cleanup for the departure janitor.
                let _ = crate::server::routes::websocket::handlers::presence::handle_muc_leave(
                    &self.state,
                    &self.room,
                    &self.sender,
                    nick.as_str(),
                    session,
                    None,
                )
                .await;
            }
            drop(room_guard);
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
        tokio::spawn(cleanup);
    }
}
