//! Supervised installation actors. Room mailboxes carry bounded wake hints;
//! committed database work, not notification delivery, determines progress.
mod actor;
mod publication;
mod scheduler;
mod telemetry;

use crate::{
    ingress_uow::{initialize_room_observations, RoomObservationRepository},
    server::routes::websocket::WebSocketState,
};
use jid::BareJid;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::task::TaskTracker;
use waddle_extensions::{ConfiguredRoomObserver, PluginId, RoomObservationSubscription};
use waddle_xmpp::muc::{
    room_actor::{NotifyRoomExtensions, RoomExtensionSubscriber, SubscribeRoomExtension},
    room_registry_actor::GetRoom,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ObservationRuntimeError {
    #[error(transparent)]
    Ingress(#[from] crate::ingress_uow::IngressUowError),
    #[error(transparent)]
    Store(#[from] crate::ingress_uow::ObservationError),
    #[error("room result has invalid host identity")]
    InvalidResult,
    #[error("room result owner is unavailable")]
    RoomUnavailable,
    #[error("room result admission was deferred")]
    AdmissionDeferred,
}

pub(crate) struct RoomObservationActors {
    mailboxes: HashMap<PluginId, (ConfiguredRoomObserver, mpsc::Sender<BareJid>)>,
    tasks: TaskTracker,
}

struct SubscriptionMailbox {
    subscription: RoomObservationSubscription,
    sender: mpsc::Sender<BareJid>,
}
impl RoomExtensionSubscriber for SubscriptionMailbox {
    fn subscription(&self) -> &RoomObservationSubscription {
        &self.subscription
    }
    fn try_notify(&self, room: &BareJid) {
        let _ = self.sender.try_send(room.clone());
    }
}

impl RoomObservationActors {
    pub(crate) async fn start(state: &Arc<WebSocketState>) -> Result<(), ObservationRuntimeError> {
        let configured = state
            .deps
            .protocol
            .extension_manager
            .configured_room_observers();
        // Source freshness is shared across replicas. Even a node with no
        // local observers must invalidate old results on accepted edits or
        // retractions, so it initializes the store and binds an empty service.
        initialize_room_observations(state.deps.app_state.db_pool.global()).await?;
        let authority = &state.deps.protocol.ingress;
        let mut tx = authority.observation_transaction().await?;
        RoomObservationRepository::sync_configured(&mut tx, &configured).await?;
        tx.commit().await?;
        let tasks = TaskTracker::new();
        let process_limit = Arc::new(Semaphore::new(32));
        let cancellation = authority.observation_cancellation();
        let mut mailboxes = HashMap::new();
        for observer in configured {
            let (sender, receiver) = mpsc::channel(128);
            mailboxes.insert(observer.plugin.clone(), (observer.clone(), sender.clone()));
            tasks.spawn(actor::run(
                Arc::downgrade(state),
                observer,
                receiver,
                process_limit.clone(),
                cancellation.clone(),
            ));
        }
        authority.bind_room_observers(Arc::new(Self { mailboxes, tasks }));
        Ok(())
    }

    pub(crate) async fn notify_room(
        &self,
        state: &WebSocketState,
        room: &BareJid,
        plugin: &PluginId,
    ) {
        let Some((observer, sender)) = self.mailboxes.get(plugin) else {
            return;
        };
        if !observer.scope.includes(room) {
            return;
        }
        let subscription = subscription(observer, room.clone());
        // The same path repairs subscriptions on a newly reclaimed actor.
        if let Ok(Some(actor)) = state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .reply_timeout(std::time::Duration::from_millis(100))
            .await
        {
            let subscriber = Arc::new(SubscriptionMailbox {
                subscription,
                sender: sender.clone(),
            });
            if actor
                .ask(SubscribeRoomExtension { subscriber })
                .reply_timeout(std::time::Duration::from_millis(100))
                .await
                .is_ok_and(|accepted| accepted)
            {
                let _ = actor
                    .tell(NotifyRoomExtensions)
                    .mailbox_timeout(std::time::Duration::from_millis(100))
                    .await;
                return;
            }
        }
        // Ownership can change after commit. Durable recovery on the current
        // owner handles a full or absent mailbox without losing the source.
        let _ = sender.try_send(room.clone());
    }

    pub(crate) async fn join(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }
}

fn subscription(observer: &ConfiguredRoomObserver, room: BareJid) -> RoomObservationSubscription {
    RoomObservationSubscription {
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        room,
    }
}
