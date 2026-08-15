use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use waddle_xmpp::muc::{RoomLifecycleId, RoomLifecycleState, RoomRevision};
use waddle_xmpp::registry::OutboundStanza;

use super::*;
use crate::room_effect_outbox::drain::{
    complete_after_write, drain_due_effects, drain_reservation_inline,
};
use crate::room_effect_outbox::{RoomEffectArmSupervisor, RoomEffectEnqueue, RoomEffectKey};
use crate::server::routes::websocket::get_or_create_room_actor;
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, register_test_connection,
};
use crate::server::session_janitors::spawn_room_effect_outbox_janitor;

const STAGED_AVAILABLE_AT_MS: i64 = i64::MAX;

fn integration_room_jid(name: &str) -> BareJid {
    BareJid::from_str(&format!("{name}@muc.example.com")).expect("room JID")
}

fn parse_lifecycle(value: &str) -> RoomLifecycleId {
    uuid::Uuid::parse_str(value)
        .map(RoomLifecycleId::from_uuid)
        .expect("lifecycle UUID")
}

async fn enqueue_config_reservation_with(
    store: &RoomEffectOutboxStore,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    room_jid: BareJid,
    recipients: Vec<FullJid>,
    producing_node: RoomEffectProducingNode,
    now_ms: i64,
) -> waddle_xmpp::muc::RoomEffectReservation {
    enqueue_config_reservation_with_origin(
        store,
        ConfigReservationSpec {
            lifecycle,
            revision,
            room_jid,
            recipients,
            origin: origin(),
            producing_node,
            now_ms,
        },
    )
    .await
}

struct ConfigReservationSpec {
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    room_jid: BareJid,
    recipients: Vec<FullJid>,
    origin: RoomEffectOriginInstanceId,
    producing_node: RoomEffectProducingNode,
    now_ms: i64,
}

async fn enqueue_config_reservation_with_origin(
    store: &RoomEffectOutboxStore,
    spec: ConfigReservationSpec,
) -> waddle_xmpp::muc::RoomEffectReservation {
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: spec.lifecycle,
                revision: spec.revision,
                effects: &config_effects_for(spec.room_jid, spec.recipients),
                origin: &spec.origin,
                producing_node: &spec.producing_node,
                now_ms: spec.now_ms,
            },
        )
        .await
        .expect("enqueue");
    tx.commit().await.expect("commit");
    reservation
}

async fn ensure_local_room_and_lifecycle(
    store: &RoomEffectOutboxStore,
    state: &crate::server::routes::websocket::WebSocketState,
    room_jid: &BareJid,
) -> (RoomLifecycleId, RoomRevision) {
    get_or_create_room_actor(
        state,
        room_jid,
        waddle_xmpp::muc::RoomConfig::default(),
        "space".to_owned(),
        "chat".to_owned(),
    )
    .await
    .expect("room actor");

    let connection = store.database().guard().await.expect("connection");
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
            (),
        )
        .await
        .expect("create lifecycle table");
    let mut rows = connection
        .query(
            "SELECT lifecycle_id, revision FROM clustering_muc_room_lifecycles WHERE room_jid = ? AND state <> 'tombstoned' LIMIT 1",
            crate::db_params![room_jid.to_string()],
        )
        .await
        .expect("query live lifecycle");
    if let Some(row) = rows.next().await.expect("lifecycle row") {
        let lifecycle: String = row.get(0).expect("lifecycle text");
        let revision = RoomRevision::from_stored(row.get::<i64>(1).expect("revision"))
            .expect("stored revision");
        return (parse_lifecycle(&lifecycle), revision);
    }
    drop(rows);

    let lifecycle = lifecycle();
    let revision = initial_revision();
    connection
        .execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![
                lifecycle.to_string(),
                room_jid.to_string(),
                revision.as_i64(),
                RoomLifecycleState::Active.as_db_str(),
            ],
        )
        .await
        .expect("insert lifecycle");
    (lifecycle, revision)
}

async fn insert_active_lifecycle(
    store: &RoomEffectOutboxStore,
    room_jid: &BareJid,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
) {
    let connection = store.database().guard().await.expect("connection");
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
            (),
        )
        .await
        .expect("create lifecycle table");
    connection
        .execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![
                lifecycle.to_string(),
                room_jid.to_string(),
                revision.as_i64(),
                RoomLifecycleState::Active.as_db_str(),
            ],
        )
        .await
        .expect("insert lifecycle");
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
async fn foreign_inert_arming_only_arms_rows_from_dead_node_incarnations() {
    let (_db, store) = store_with_db("room-effect-foreign-inert-arming").await;
    let live_lifecycle = lifecycle();
    let foreign_lifecycle = lifecycle();
    let live_node = RoomEffectProducingNode::from_node_identity(
        waddle_xmpp::ownership::NodeIdentity::new("node-a", "current-epoch"),
    );
    let foreign_node = RoomEffectProducingNode::from_node_identity(
        waddle_xmpp::ownership::NodeIdentity::new("node-a", "previous-epoch"),
    );

    let live_reservation = enqueue_config_reservation_with(
        &store,
        live_lifecycle,
        initial_revision(),
        room_jid(),
        vec![full_jid("alice@example.test/device")],
        live_node.clone(),
        100,
    )
    .await;
    let foreign_reservation = enqueue_config_reservation_with(
        &store,
        foreign_lifecycle,
        initial_revision(),
        room_jid(),
        vec![full_jid("bob@example.test/device")],
        foreign_node,
        100,
    )
    .await;

    let stale = store
        .list_foreign_inert(std::slice::from_ref(&live_node))
        .await
        .expect("list foreign inert");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].key.lifecycle, foreign_lifecycle);

    assert_eq!(
        store
            .arm_foreign_inert(&[live_node], 250)
            .await
            .expect("arm foreign inert"),
        1
    );

    let live_key = RoomEffectKey {
        lifecycle: live_lifecycle,
        revision: live_reservation.revision,
        ordinal: live_reservation.ordinals[0],
    };
    let foreign_key = RoomEffectKey {
        lifecycle: foreign_lifecycle,
        revision: foreign_reservation.revision,
        ordinal: foreign_reservation.ordinals[0],
    };
    assert_eq!(
        store
            .find(&live_key)
            .await
            .expect("live row")
            .expect("live row present")
            .available_at_ms,
        STAGED_AVAILABLE_AT_MS
    );
    assert_eq!(
        store
            .find(&foreign_key)
            .await
            .expect("foreign row")
            .expect("foreign row present")
            .available_at_ms,
        250
    );
}

#[tokio::test(flavor = "current_thread")]
async fn foreign_inert_row_drains_after_janitor_arming() {
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let room_jid = integration_room_jid("foreign-inert-drain");
    let recipient = full_jid("foreign@example.com/device");
    let (lifecycle, revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &room_jid).await;
    let stale_node = RoomEffectProducingNode::from_node_identity(
        waddle_xmpp::ownership::NodeIdentity::new("node-before-restart", "old-epoch"),
    );
    let reservation = enqueue_config_reservation_with(
        store.as_ref(),
        lifecycle,
        revision,
        room_jid,
        vec![recipient.clone()],
        stale_node,
        0,
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &recipient, tx).await;

    assert_eq!(
        store
            .arm_foreign_inert(&[producing_node()], 0)
            .await
            .expect("arm stale epoch row"),
        1
    );
    let state_for_drain = Arc::clone(&state);
    let drain =
        tokio::spawn(async move { drain_due_effects(state_for_drain.as_ref(), 0, 8).await });
    let outbound = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("armed row delivery timeout")
        .expect("armed row delivery");
    outbound
        .write_acceptance
        .as_ref()
        .expect("armed row acceptance")
        .acknowledge();
    assert_eq!(
        drain
            .await
            .expect("drain join")
            .expect("drain result")
            .drained,
        1
    );
    assert_eq!(
        store
            .pending_rows_for_lifecycle(lifecycle)
            .await
            .expect("pending rows"),
        0,
        "the armed foreign row must complete after delivery"
    );
    assert_eq!(reservation.ordinals.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn standalone_janitor_arms_predecessor_instance_rows_without_touching_current_rows() {
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let predecessor_room = integration_room_jid("standalone-predecessor");
    let current_room = integration_room_jid("standalone-current");
    let predecessor_origin =
        RoomEffectOriginInstanceId::new("predecessor-instance".to_owned()).expect("origin");
    let current_origin = crate::room_effect_outbox::room_effect_origin_instance_id();

    let (predecessor_lifecycle, predecessor_revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &predecessor_room).await;
    let (current_lifecycle, current_revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &current_room).await;

    let predecessor = enqueue_config_reservation_with_origin(
        store.as_ref(),
        ConfigReservationSpec {
            lifecycle: predecessor_lifecycle,
            revision: predecessor_revision,
            room_jid: predecessor_room,
            recipients: vec![full_jid("predecessor@example.com/device")],
            origin: predecessor_origin,
            producing_node: producing_node(),
            now_ms: 0,
        },
    )
    .await;
    let current = enqueue_config_reservation_with_origin(
        store.as_ref(),
        ConfigReservationSpec {
            lifecycle: current_lifecycle,
            revision: current_revision,
            room_jid: current_room,
            recipients: vec![full_jid("current@example.com/device")],
            origin: current_origin,
            producing_node: producing_node(),
            now_ms: 0,
        },
    )
    .await;

    let predecessor_key = RoomEffectKey {
        lifecycle: predecessor_lifecycle,
        revision: predecessor_revision,
        ordinal: predecessor.ordinals[0],
    };
    let current_key = RoomEffectKey {
        lifecycle: current_lifecycle,
        revision: current_revision,
        ordinal: current.ordinals[0],
    };

    spawn_room_effect_outbox_janitor(&state);

    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let predecessor_available_at = store
                .find(&predecessor_key)
                .await
                .expect("find predecessor row")
                .expect("predecessor row present")
                .available_at_ms;
            let current_available_at = store
                .find(&current_key)
                .await
                .expect("find current row")
                .expect("current row present")
                .available_at_ms;
            if predecessor_available_at != STAGED_AVAILABLE_AT_MS {
                assert_eq!(
                    current_available_at, STAGED_AVAILABLE_AT_MS,
                    "current-process inert rows must remain staged in standalone recovery"
                );
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("standalone predecessor arming timeout");
}

#[tokio::test(flavor = "current_thread")]
async fn staged_row_survives_origin_crash_until_reaper_arms_and_drains_it() {
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let room_jid = integration_room_jid("stale-producer-reaper-drain");
    let recipient = full_jid("restart@example.com/device");
    let (lifecycle, revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &room_jid).await;
    let stale_node = RoomEffectProducingNode::from_node_identity(
        waddle_xmpp::ownership::NodeIdentity::new("node-before-restart", "old-epoch"),
    );
    let live_node = RoomEffectProducingNode::from_node_identity(
        waddle_xmpp::ownership::NodeIdentity::new("node-before-restart", "new-epoch"),
    );
    let reservation = enqueue_config_reservation_with(
        store.as_ref(),
        lifecycle,
        revision,
        room_jid,
        vec![recipient.clone()],
        stale_node,
        100,
    )
    .await;
    let key = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[0],
    };
    assert_eq!(
        store
            .find(&key)
            .await
            .expect("find staged row")
            .expect("staged row present")
            .available_at_ms,
        STAGED_AVAILABLE_AT_MS,
        "without a supervisor handoff, the origin crash leaves the row inert"
    );

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &recipient, tx).await;

    assert_eq!(
        store
            .arm_foreign_inert(&[live_node], 250)
            .await
            .expect("reaper arms stale row"),
        1
    );
    assert_eq!(
        store
            .find(&key)
            .await
            .expect("find armed row")
            .expect("armed row present")
            .available_at_ms,
        250,
        "the reaper should convert the inert row into a due row"
    );

    let state_for_drain = Arc::clone(&state);
    let drain =
        tokio::spawn(async move { drain_due_effects(state_for_drain.as_ref(), 250, 8).await });
    let outbound = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("reaped row delivery timeout")
        .expect("reaped row delivery");
    outbound
        .write_acceptance
        .as_ref()
        .expect("reaped row acceptance")
        .acknowledge();
    assert_eq!(
        drain
            .await
            .expect("reaped drain join")
            .expect("reaped drain result")
            .drained,
        1
    );
    assert!(
        store
            .find(&key)
            .await
            .expect("find completed row")
            .is_none(),
        "the reaped row must complete after draining"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_retries_transient_store_failure_then_drains_once_store_recovers() {
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let room_jid = integration_room_jid("arm-retry");
    let recipient = full_jid("alice@example.com/device");
    let (lifecycle, revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &room_jid).await;
    let reservation = enqueue_config_reservation_with(
        store.as_ref(),
        lifecycle,
        revision,
        room_jid.clone(),
        vec![recipient.clone()],
        producing_node(),
        100,
    )
    .await;
    let (_owner, mut recipient_rx) = {
        let (tx, rx) = mpsc::channel::<OutboundStanza>(8);
        let owner = register_test_connection(state.as_ref(), &recipient, tx).await;
        (owner, rx)
    };
    let armed = Arc::new(AtomicUsize::new(0));
    let supervisor =
        RoomEffectArmSupervisor::with_on_armed(Arc::clone(&store), Handle::current(), {
            let armed = Arc::clone(&armed);
            move |_| {
                armed.fetch_add(1, Ordering::SeqCst);
            }
        });
    supervisor.attach_drain_state(&state);

    rename_effect_table(
        store.as_ref(),
        "clustering_muc_room_effects",
        "clustering_muc_room_effects_hidden",
    )
    .await;

    // Real time on purpose (paused-clock sqlx acquires PoolTimedOut).
    supervisor.arm(reservation.clone());
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert_eq!(armed.load(Ordering::SeqCst), 0);

    rename_effect_table(
        store.as_ref(),
        "clustering_muc_room_effects_hidden",
        "clustering_muc_room_effects",
    )
    .await;

    let outbound = tokio::time::timeout(Duration::from_secs(30), recipient_rx.recv())
        .await
        .expect("recipient delivery timeout")
        .expect("recipient delivery");
    outbound
        .write_acceptance
        .as_ref()
        .expect("write acceptance")
        .acknowledge();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if store
                .list_for_lifecycle(lifecycle)
                .await
                .expect("list rows")
                .is_empty()
                && armed.load(Ordering::SeqCst) == 1
                && supervisor.state_snapshot() == (false, 0)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("supervisor must arm, drain, and quiesce");
}

#[tokio::test(flavor = "current_thread")]
async fn inline_drain_keeps_lease_until_write_acceptance_then_completes_cleanly() {
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let room_jid = integration_room_jid("inline-complete");
    let initiator = full_jid("alice@example.com/web");
    let peer = full_jid("bob@example.com/phone");
    let (lifecycle, revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &room_jid).await;
    let reservation = enqueue_config_reservation_with(
        store.as_ref(),
        lifecycle,
        revision,
        room_jid,
        vec![initiator.clone(), peer.clone()],
        producing_node(),
        100,
    )
    .await;
    assert_eq!(
        store
            .arm_reservation(&reservation, 200)
            .await
            .expect("arm reservation"),
        1
    );

    let (_peer_owner, mut peer_rx) = {
        let (tx, rx) = mpsc::channel::<OutboundStanza>(8);
        let owner = register_test_connection(state.as_ref(), &peer, tx).await;
        (owner, rx)
    };

    let mut frames = drain_reservation_inline(state.as_ref(), &reservation, Some(&initiator))
        .await
        .expect("inline drain");
    assert_eq!(frames.len(), 1);
    let completion = frames.pop().expect("inline frame").completion;
    let key = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[0],
    };
    assert!(store
        .find(&key)
        .await
        .expect("find leased row")
        .expect("leased row exists")
        .lease_token
        .is_some());

    let completion_task = tokio::spawn({
        let state = Arc::clone(&state);
        let completion = completion.clone();
        async move { complete_after_write(state.as_ref(), &completion).await }
    });

    let outbound = tokio::time::timeout(Duration::from_secs(1), peer_rx.recv())
        .await
        .expect("peer delivery timeout")
        .expect("peer delivery");
    tokio::task::yield_now().await;
    assert!(
        !completion_task.is_finished(),
        "completion must wait for local write acceptance"
    );
    assert_eq!(
        drain_due_effects(state.as_ref(), crate::time::now_ms(), 8)
            .await
            .expect("race janitor drain"),
        crate::room_effect_outbox::drain::RoomEffectDrainSummary::default()
    );
    assert!(
        store.find(&key).await.expect("row after race").is_some(),
        "leased inline row must survive while write acceptance is pending"
    );

    outbound
        .write_acceptance
        .as_ref()
        .expect("write acceptance")
        .acknowledge();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), completion_task)
            .await
            .expect("completion timeout")
            .expect("completion join")
            .expect("completion result"),
        "completion must delete the row after write acceptance"
    );
    assert!(
        store
            .find(&key)
            .await
            .expect("find completed row")
            .is_none(),
        "completed inline row must be deleted"
    );
    assert!(
        drain_reservation_inline(state.as_ref(), &reservation, Some(&initiator))
            .await
            .expect("janitor-won inline race is clean")
            .is_empty(),
        "an already-completed reservation must not deliver a second frame"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn janitor_smoke_telemetry_reports_completed_room_effect_sweep() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let room_jid = integration_room_jid("janitor-smoke");
    let recipient = full_jid("janitor@example.com/device");
    let (lifecycle, revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &room_jid).await;
    let reservation = enqueue_config_reservation_with(
        store.as_ref(),
        lifecycle,
        revision,
        room_jid,
        vec![recipient.clone()],
        producing_node(),
        crate::time::now_ms(),
    )
    .await;
    store
        .arm_reservation(&reservation, crate::time::now_ms())
        .await
        .expect("arm reservation");
    let (_owner, mut recipient_rx) = {
        let (tx, rx) = mpsc::channel::<OutboundStanza>(8);
        let owner = register_test_connection(state.as_ref(), &recipient, tx).await;
        (owner, rx)
    };

    spawn_room_effect_outbox_janitor(&state);

    let delivery = tokio::time::timeout(Duration::from_secs(7), recipient_rx.recv())
        .await
        .expect("janitor delivery timeout")
        .expect("janitor delivery");
    delivery
        .write_acceptance
        .as_ref()
        .expect("janitor write acceptance")
        .acknowledge();

    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if metrics.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "room_effect_outbox"), ("outcome", "completed")],
            ) == Some(1)
                && metrics.histogram_count(
                    "waddle.room_effect_outbox.depth",
                    &[("janitor", "room_effect_outbox")],
                ) == Some(1)
                && store
                    .find(&RoomEffectKey {
                        lifecycle,
                        revision,
                        ordinal: reservation.ordinals[0],
                    })
                    .await
                    .expect("find janitor row")
                    .is_none()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("janitor telemetry timeout");
}

#[tokio::test(flavor = "current_thread")]
async fn janitor_exports_requeued_and_stale_room_effect_counters() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);

    let drained_room = integration_room_jid("janitor-drained");
    let requeued_room = integration_room_jid("janitor-requeued");
    let stale_room = integration_room_jid("janitor-stale");
    let recipient = full_jid("janitor-metrics@example.com/device");
    let now_ms = crate::time::now_ms();

    let (drained_lifecycle, drained_revision) =
        ensure_local_room_and_lifecycle(store.as_ref(), state.as_ref(), &drained_room).await;
    let requeued_lifecycle = lifecycle();
    insert_active_lifecycle(
        store.as_ref(),
        &requeued_room,
        requeued_lifecycle,
        initial_revision(),
    )
    .await;
    let stale_lifecycle = lifecycle();

    let drained = enqueue_config_reservation_with(
        store.as_ref(),
        drained_lifecycle,
        drained_revision,
        drained_room,
        vec![recipient.clone()],
        producing_node(),
        now_ms,
    )
    .await;
    let requeued = enqueue_config_reservation_with(
        store.as_ref(),
        requeued_lifecycle,
        initial_revision(),
        requeued_room,
        vec![full_jid("remote@example.com/device")],
        producing_node(),
        now_ms,
    )
    .await;
    let stale = enqueue_config_reservation_with(
        store.as_ref(),
        stale_lifecycle,
        initial_revision(),
        stale_room,
        vec![full_jid("stale@example.com/device")],
        producing_node(),
        now_ms,
    )
    .await;
    for reservation in [&drained, &requeued, &stale] {
        store
            .arm_reservation(reservation, now_ms)
            .await
            .expect("arm reservation");
    }

    let (_owner, mut recipient_rx) = {
        let (tx, rx) = mpsc::channel::<OutboundStanza>(8);
        let owner = register_test_connection(state.as_ref(), &recipient, tx).await;
        (owner, rx)
    };

    spawn_room_effect_outbox_janitor(&state);

    let delivery = tokio::time::timeout(Duration::from_secs(7), recipient_rx.recv())
        .await
        .expect("janitor delivery timeout")
        .expect("janitor delivery");
    delivery
        .write_acceptance
        .as_ref()
        .expect("janitor write acceptance")
        .acknowledge();

    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if metrics.counter_sum(
                "waddle.room_effect_outbox.drained",
                &[("janitor", "room_effect_outbox")],
            ) == Some(1)
                && metrics.counter_sum(
                    "waddle.room_effect_outbox.requeued",
                    &[("janitor", "room_effect_outbox")],
                ) == Some(1)
                && metrics.counter_sum(
                    "waddle.room_effect_outbox.stale",
                    &[("janitor", "room_effect_outbox")],
                ) == Some(1)
                && store
                    .find(&RoomEffectKey {
                        lifecycle: drained_lifecycle,
                        revision: drained_revision,
                        ordinal: drained.ordinals[0],
                    })
                    .await
                    .expect("find drained row")
                    .is_none()
                && store
                    .find(&RoomEffectKey {
                        lifecycle: requeued_lifecycle,
                        revision: initial_revision(),
                        ordinal: requeued.ordinals[0],
                    })
                    .await
                    .expect("find requeued row")
                    .is_some()
                && store
                    .find(&RoomEffectKey {
                        lifecycle: stale_lifecycle,
                        revision: initial_revision(),
                        ordinal: stale.ordinals[0],
                    })
                    .await
                    .expect("find stale row")
                    .is_none()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("janitor metrics timeout");
}
