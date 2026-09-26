//! A responsive actor loop with bounded workers and round-robin room admission.
use super::{subscription, ObservationRuntimeError};
use crate::{ingress_uow::RoomObservationRepository, server::routes::websocket::WebSocketState};
use jid::BareJid;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    sync::{mpsc, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use waddle_extensions::{ConfiguredRoomObserver, RoomObservationSubscription};
use waddle_xmpp::muc::room_registry_actor::GetOrRestoreDurableRoom;

pub(super) async fn run(
    state: Weak<WebSocketState>,
    observer: ConfiguredRoomObserver,
    mut receiver: mpsc::Receiver<BareJid>,
    process_limit: Arc<Semaphore>,
    cancellation: CancellationToken,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scheduler = super::scheduler::Scheduler::new(observer.max_concurrent);
    let mut active = HashMap::new();
    let mut workers = JoinSet::new();
    let mut recovery_cursor = None;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            room = receiver.recv() => match room {
                Some(room) => scheduler.wake(room),
                None => break,
            },
            Some(completed) = workers.join_next_with_id(), if !workers.is_empty() => {
                let (id, more_work) = match completed {
                    Ok((id, Ok(more_work))) => (id, more_work),
                    Ok((id, Err(error))) => {
                        tracing::warn!(plugin = %observer.plugin, %error, "room observation worker deferred");
                        (id, false)
                    }
                    Err(error) => {
                        tracing::warn!(plugin = %observer.plugin, cancelled = error.is_cancelled(), "room observation task ended unexpectedly; lease will recover");
                        (error.id(), false)
                    }
                };
                if let Some(room) = active.remove(&id) {
                    scheduler.completed(room, more_work);
                }
            },
            _ = tick.tick() => {
                let Some(state) = state.upgrade() else { break; };
                match due_rooms(&state, &observer, recovery_cursor.as_ref()).await {
                    Ok(rooms) => {
                        recovery_cursor = rooms.last().cloned();
                        for room in rooms { scheduler.wake(room); }
                    },
                    Err(error) => tracing::warn!(plugin = %observer.plugin, %error, "room observation recovery deferred"),
                }
            }
        }
        while workers.len() < observer.max_concurrent as usize {
            let Ok(permit) = process_limit.clone().try_acquire_owned() else {
                break;
            };
            let Some(room) = scheduler.next() else {
                break;
            };
            let Some(state) = state.upgrade() else {
                break;
            };
            let subscription = subscription(&observer, room.clone());
            let handle = workers.spawn(async move {
                let _permit = permit;
                process_room(&state, &subscription).await
            });
            active.insert(handle.id(), room);
        }
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}

async fn due_rooms(
    state: &WebSocketState,
    observer: &ConfiguredRoomObserver,
    after: Option<&BareJid>,
) -> Result<Vec<BareJid>, ObservationRuntimeError> {
    let mut tx = state
        .deps
        .protocol
        .ingress
        .observation_transaction()
        .await?;
    let rooms =
        RoomObservationRepository::due_rooms(&mut tx, observer, after, crate::time::now_ms(), 512)
            .await?;
    tx.commit().await?;
    Ok(rooms)
}

async fn process_room(
    state: &Arc<WebSocketState>,
    subscription: &RoomObservationSubscription,
) -> Result<bool, ObservationRuntimeError> {
    // Each installation replica services its local room owners. A saved
    // publication stays discoverable when ownership moves between nodes.
    if !matches!(
        state
            .deps
            .protocol
            .room_registry
            .ask(GetOrRestoreDurableRoom {
                room_jid: subscription.room.clone()
            })
            .reply_timeout(Duration::from_secs(5))
            .await,
        Ok(Some(_))
    ) {
        return Ok(false);
    }
    super::publication::publish_pending(state, subscription).await?;
    let authority = &state.deps.protocol.ingress;
    let mut tx = authority.observation_transaction().await?;
    let work =
        RoomObservationRepository::claim(&mut tx, subscription, crate::time::now_ms()).await?;
    tx.commit().await?;
    let Some(work) = work else {
        return Ok(false);
    };
    super::telemetry::started(&work.source.observed_at, work.attempt);
    let started = std::time::Instant::now();
    let outcome = state
        .deps
        .protocol
        .extension_manager
        .observe_room_message(subscription, work.source.clone(), work.body.clone())
        .await;
    let duration = started.elapsed();
    let mut tx = authority.observation_transaction().await?;
    let saved =
        RoomObservationRepository::finish(&mut tx, &work, &outcome, crate::time::now_ms()).await?;
    tx.commit().await?;
    super::telemetry::finished(
        &subscription.plugin,
        work.attempt,
        &outcome,
        duration,
        saved,
    );
    if saved {
        super::publication::publish_pending(state, subscription).await?;
    }
    Ok(saved)
}
