//! Single-supervisor retry handoff for staged room-effect arming.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::runtime::Handle;
use waddle_xmpp::muc::{RoomEffectOrdinal, RoomEffectReservation, RoomLifecycleId, RoomRevision};

use super::{RoomEffectKey, RoomEffectOutboxError, RoomEffectOutboxStore};
use crate::server::routes::websocket::WebSocketState;

const STAGED_AVAILABLE_AT_MS: i64 = i64::MAX;

type OnArmed = Arc<dyn Fn(RoomLifecycleId) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReservationKey {
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    ordinals: Vec<RoomEffectOrdinal>,
}

impl From<&RoomEffectReservation> for ReservationKey {
    fn from(value: &RoomEffectReservation) -> Self {
        Self {
            lifecycle: value.lifecycle,
            revision: value.revision,
            ordinals: value.ordinals.clone(),
        }
    }
}

impl From<ReservationKey> for RoomEffectReservation {
    fn from(value: ReservationKey) -> Self {
        Self {
            lifecycle: value.lifecycle,
            revision: value.revision,
            ordinals: value.ordinals,
        }
    }
}

#[derive(Default)]
struct RetryState {
    running: bool,
    pending: HashSet<ReservationKey>,
}

enum ArmOutcome {
    Armed,
    AlreadyReady,
    StillStaged,
    Gone,
}

/// Coalesces staged reservation arming behind one retry task. Rows are only
/// ever armed here; deletion remains the responsibility of in-transaction
/// supersession paths.
#[derive(Clone)]
pub struct RoomEffectArmSupervisor {
    store: Arc<RoomEffectOutboxStore>,
    runtime: Handle,
    on_armed: OnArmed,
    drain_state: Arc<Mutex<Option<Weak<WebSocketState>>>>,
    state: Arc<Mutex<RetryState>>,
}

impl RoomEffectArmSupervisor {
    pub fn new(store: Arc<RoomEffectOutboxStore>, runtime: Handle) -> Self {
        Self::with_on_armed(store, runtime, |_| {})
    }

    pub fn with_on_armed(
        store: Arc<RoomEffectOutboxStore>,
        runtime: Handle,
        on_armed: impl Fn(RoomLifecycleId) + Send + Sync + 'static,
    ) -> Self {
        Self {
            store,
            runtime,
            on_armed: Arc::new(on_armed),
            drain_state: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(RetryState::default())),
        }
    }

    /// Attach the finished WebSocket state once its cyclic service graph has
    /// been constructed.  A weak reference keeps the supervisor from
    /// extending server lifetime.
    pub fn attach_drain_state(&self, state: &Arc<WebSocketState>) {
        *self
            .drain_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(state));
    }

    pub fn arm(&self, reservation: RoomEffectReservation) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending.insert(ReservationKey::from(&reservation));
            if state.running {
                return;
            }
            state.running = true;
        }

        let store = Arc::clone(&self.store);
        let on_armed = Arc::clone(&self.on_armed);
        let drain_state = Arc::clone(&self.drain_state);
        let runtime = self.runtime.clone();
        let state = Arc::clone(&self.state);
        self.runtime.spawn(async move {
            loop {
                let reservations = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state
                        .pending
                        .drain()
                        .map(RoomEffectReservation::from)
                        .collect::<Vec<_>>()
                };
                for reservation in reservations {
                    let lifecycle = reservation.lifecycle;
                    match arm_with_retry(&store, &reservation).await {
                        ArmOutcome::Armed | ArmOutcome::AlreadyReady => {
                            on_armed(lifecycle);
                            nudge_drain(&runtime, &drain_state);
                        }
                        ArmOutcome::StillStaged => {
                            state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .pending
                                .insert(ReservationKey::from(&reservation));
                        }
                        ArmOutcome::Gone => {}
                    }
                }
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.pending.is_empty() {
                    state.running = false;
                    break;
                }
            }
        });
    }

    /// Start an exact drain without making the command handler wait for a
    /// recipient writer. Admin commands have no response-batch ordering
    /// requirement, but their handler-window rows still need an immediate
    /// post-commit delivery attempt rather than waiting for the janitor.
    pub fn spawn_reservation_drain(&self, reservation: RoomEffectReservation) {
        let drain_state = Arc::clone(&self.drain_state);
        self.runtime.spawn(async move {
            let state = drain_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(Weak::upgrade);
            let Some(state) = state else {
                return;
            };
            let drain = crate::room_effect_outbox::drain::drain_local_reservation_after_commit(
                state.as_ref(),
                &reservation,
            )
            .await;
            if let Err(error) = drain {
                tracing::warn!(
                    lifecycle = %reservation.lifecycle,
                    revision = reservation.revision.as_i64(),
                    "admin command room-effect drain failed after committed mutation: {error}"
                );
            }
        });
    }

    #[cfg(test)]
    pub(super) fn state_snapshot(&self) -> (bool, usize) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.running, state.pending.len())
    }
}

fn nudge_drain(runtime: &Handle, drain_state: &Arc<Mutex<Option<Weak<WebSocketState>>>>) {
    let state = drain_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(Weak::upgrade);
    let Some(state) = state else {
        return;
    };
    runtime.spawn(async move {
        if let Err(error) =
            crate::room_effect_outbox::drain::drain_due_effects(&state, crate::time::now_ms(), 64)
                .await
        {
            tracing::warn!(%error, "room effect arm nudge drain failed");
        }
    });
}

async fn arm_with_retry(
    store: &RoomEffectOutboxStore,
    reservation: &RoomEffectReservation,
) -> ArmOutcome {
    let mut retry_delay = initial_retry_delay();
    loop {
        match try_arm_reservation(store, reservation).await {
            // StillStaged should be unreachable (the arm UPDATE matches any
            // inert, non-superseded row), but if it ever occurs it must not
            // spin inside this call and starve the supervisor's other pending
            // reservations — hand it back to the outer pending set instead.
            Ok(outcome) => return outcome,
            Err(error) => {
                tracing::warn!(
                    %error,
                    lifecycle = %reservation.lifecycle,
                    revision = reservation.revision.as_i64(),
                    ordinals = ?reservation
                        .ordinals
                        .iter()
                        .map(|ordinal| ordinal.as_i64())
                        .collect::<Vec<_>>(),
                    retry_delay_ms = retry_delay.as_millis(),
                    "room effect arm retry failed"
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay
                    .saturating_mul(2)
                    .min(Duration::from_secs(10 * 60));
            }
        }
    }
}

fn initial_retry_delay() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(1)
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(5)
    }
}

async fn try_arm_reservation(
    store: &RoomEffectOutboxStore,
    reservation: &RoomEffectReservation,
) -> Result<ArmOutcome, RoomEffectOutboxError> {
    let now_ms = crate::time::now_ms();
    if store.arm_reservation(reservation, now_ms).await? > 0 {
        return Ok(ArmOutcome::Armed);
    }

    let mut rows = Vec::with_capacity(reservation.ordinals.len());
    for ordinal in &reservation.ordinals {
        rows.push(
            store
                .find(&RoomEffectKey {
                    lifecycle: reservation.lifecycle,
                    revision: reservation.revision,
                    ordinal: *ordinal,
                })
                .await?,
        );
    }

    Ok(classify_post_arm_rows(rows.iter().flatten()))
}

fn classify_post_arm_rows<'a, I>(rows: I) -> ArmOutcome
where
    I: IntoIterator<Item = &'a super::RoomEffectRow>,
{
    let mut has_ready = false;
    for row in rows {
        if row.superseded {
            continue;
        }
        if row.available_at_ms == STAGED_AVAILABLE_AT_MS {
            return ArmOutcome::StillStaged;
        }
        has_ready = true;
    }

    if has_ready {
        ArmOutcome::AlreadyReady
    } else {
        ArmOutcome::Gone
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jid::{BareJid, FullJid};
    use waddle_xmpp::muc::{
        MucConfigStatusCode, RoomEffectReservation, RoomLifecycleId, RoomMutationEffects,
        RoomRevision,
    };
    use waddle_xmpp::ownership::NodeIdentity;

    use super::*;
    use crate::db::Database;
    use crate::room_effect_outbox::{
        RoomEffectEnqueue, RoomEffectOriginInstanceId, RoomEffectProducingNode,
    };

    async fn store_with_db(name: &str) -> (Database, Arc<RoomEffectOutboxStore>) {
        let database = Database::in_memory(name).await.expect("database");
        let store = Arc::new(
            RoomEffectOutboxStore::new(database.clone())
                .await
                .expect("store"),
        );
        (database, store)
    }

    fn room_jid() -> BareJid {
        BareJid::from_str("room@conference.example.test").expect("room JID")
    }

    fn lifecycle() -> RoomLifecycleId {
        RoomLifecycleId::generate()
    }

    fn initial_revision() -> RoomRevision {
        RoomRevision::initial()
    }

    fn origin() -> RoomEffectOriginInstanceId {
        RoomEffectOriginInstanceId::new("origin-instance".to_owned()).expect("origin instance")
    }

    fn producing_node() -> RoomEffectProducingNode {
        RoomEffectProducingNode::from_node_identity(NodeIdentity::new("node-a", "epoch-a"))
    }

    fn full_jid(value: &str) -> FullJid {
        FullJid::from_str(value).expect("full JID")
    }

    fn config_effects() -> RoomMutationEffects {
        RoomMutationEffects::config(
            room_jid(),
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![full_jid("alice@example.test/device")],
        )
    }

    async fn enqueue_config_reservation(
        store: &RoomEffectOutboxStore,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
        now_ms: i64,
    ) -> RoomEffectReservation {
        let mut tx = store.database().begin().await.expect("transaction");
        let reservation = store
            .enqueue_in_tx(
                &mut tx,
                RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects: &config_effects(),
                    origin: &origin(),
                    producing_node: &producing_node(),
                    now_ms,
                },
            )
            .await
            .expect("enqueue");
        tx.commit().await.expect("commit");
        reservation
    }

    async fn rename_effect_table(store: &RoomEffectOutboxStore, from: &str, to: &str) {
        let connection = store.database().guard().await.expect("connection");
        connection
            .execute(
                &format!("ALTER TABLE {from} RENAME TO {to}"),
                crate::db_params!(),
            )
            .await
            .expect("rename table");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn arm_supervisor_coalesces_duplicate_reservations() {
        let (_db, store) = store_with_db("room-effect-arm-supervisor").await;
        let lifecycle = lifecycle();
        let reservation =
            enqueue_config_reservation(store.as_ref(), lifecycle, initial_revision(), 100).await;
        let nudges = Arc::new(AtomicUsize::new(0));
        let supervisor = RoomEffectArmSupervisor::with_on_armed(
            Arc::clone(&store),
            tokio::runtime::Handle::current(),
            {
                let nudges = Arc::clone(&nudges);
                move |_| {
                    nudges.fetch_add(1, Ordering::SeqCst);
                }
            },
        );

        supervisor.arm(reservation.clone());
        supervisor.arm(reservation);
        assert_eq!(supervisor.state_snapshot(), (true, 1));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = store
                    .list_for_lifecycle(lifecycle)
                    .await
                    .expect("list rows");
                let armed = rows
                    .iter()
                    .all(|row| row.available_at_ms != STAGED_AVAILABLE_AT_MS);
                if armed
                    && nudges.load(Ordering::SeqCst) == 1
                    && supervisor.state_snapshot() == (false, 0)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervisor must arm and quiesce");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn arm_supervisor_retries_until_store_recovers() {
        let (_db, store) = store_with_db("room-effect-arm-retry").await;
        let lifecycle = lifecycle();
        let reservation =
            enqueue_config_reservation(store.as_ref(), lifecycle, initial_revision(), 100).await;
        let nudges = Arc::new(AtomicUsize::new(0));
        let supervisor = RoomEffectArmSupervisor::with_on_armed(
            Arc::clone(&store),
            tokio::runtime::Handle::current(),
            {
                let nudges = Arc::clone(&nudges);
                move |_| {
                    nudges.fetch_add(1, Ordering::SeqCst);
                }
            },
        );

        rename_effect_table(
            store.as_ref(),
            "clustering_muc_room_effects",
            "clustering_muc_room_effects_hidden",
        )
        .await;

        supervisor.arm(reservation);
        tokio::task::yield_now().await;
        assert_eq!(nudges.load(Ordering::SeqCst), 0);

        rename_effect_table(
            store.as_ref(),
            "clustering_muc_room_effects_hidden",
            "clustering_muc_room_effects",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let rows = store
                    .list_for_lifecycle(lifecycle)
                    .await
                    .expect("list rows");
                let armed = rows
                    .iter()
                    .all(|row| row.available_at_ms != STAGED_AVAILABLE_AT_MS);
                if armed
                    && nudges.load(Ordering::SeqCst) == 1
                    && supervisor.state_snapshot() == (false, 0)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervisor must retry after store recovery");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn still_staged_rows_do_not_count_as_armed() {
        let (_db, store) = store_with_db("room-effect-arm-still-staged").await;
        let lifecycle = lifecycle();
        let reservation =
            enqueue_config_reservation(store.as_ref(), lifecycle, initial_revision(), 100).await;
        let mut rows = Vec::new();
        for ordinal in &reservation.ordinals {
            rows.push(
                store
                    .find(&RoomEffectKey {
                        lifecycle,
                        revision: reservation.revision,
                        ordinal: *ordinal,
                    })
                    .await
                    .expect("find row"),
            );
        }

        assert!(matches!(
            classify_post_arm_rows(rows.iter().flatten()),
            ArmOutcome::StillStaged
        ));
    }
}
