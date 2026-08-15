use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use tokio::sync::mpsc;
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::muc::{
    DestroyRecipient, MucConfigStatusCode, RoomConfig, RoomLifecycleId, RoomLifecycleState,
    RoomMutationEffects, RoomRevision,
};
use waddle_xmpp::registry::OutboundStanza;
use xmpp_parsers::minidom::Element;

use super::*;
use crate::room_effect_outbox::RoomEffectEnqueue;
use crate::room_effect_outbox::drain::{
    complete_after_write, drain_due_effects, drain_reservation_inline,
};
use crate::server::routes::websocket::WebSocketState;
use crate::server::routes::websocket::stanza_to_xml;
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, register_test_connection,
};

fn drain_room_jid() -> BareJid {
    BareJid::from_str("room@muc.example.com").expect("room JID")
}

fn config_effects_for(room_jid: &BareJid, recipients: Vec<FullJid>) -> RoomMutationEffects {
    RoomMutationEffects::config(
        room_jid.clone(),
        vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
        recipients,
    )
}

fn destroy_effects_for(room_jid: &BareJid, sessions: Vec<FullJid>) -> RoomMutationEffects {
    RoomMutationEffects::destroy(
        room_jid.clone(),
        None,
        None,
        None,
        vec![DestroyRecipient {
            nick: nick("alice"),
            sessions,
        }],
    )
}

async fn enqueue_effects(
    state: &WebSocketState,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    effects: &RoomMutationEffects,
    now_ms: i64,
) -> waddle_xmpp::muc::RoomEffectReservation {
    let store = state.deps.protocol.room_effect_outbox.as_ref();
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision,
                effects,
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

async fn enqueue_and_arm(
    store: &RoomEffectOutboxStore,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    effects: RoomMutationEffects,
) -> waddle_xmpp::muc::RoomEffectReservation {
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision,
                effects: &effects,
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");
    tx.commit().await.expect("commit");
    store
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm reservation");
    reservation
}

async fn insert_lifecycle_row(
    state: &WebSocketState,
    room_jid: &BareJid,
    lifecycle: RoomLifecycleId,
    revision: RoomRevision,
    state_name: RoomLifecycleState,
) {
    let connection = state
        .deps
        .protocol
        .room_effect_outbox
        .database()
        .guard()
        .await
        .expect("connection");
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
                state_name.as_db_str(),
            ],
        )
        .await
        .expect("insert lifecycle");
}

async fn create_owned_room_and_lifecycle(state: &WebSocketState) -> RoomLifecycleId {
    let room_jid = drain_room_jid();
    let lifecycle = lifecycle();
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "room-effect-test".to_owned(),
            channel_id: "room-effect-test".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");
    insert_lifecycle_row(
        state,
        &room_jid,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    lifecycle
}

async fn recv_outbound(rx: &mut mpsc::Receiver<OutboundStanza>) -> OutboundStanza {
    tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("outbound receive timeout")
        .expect("outbound stanza")
}

fn muc_status_codes(outbound: &OutboundStanza) -> Vec<String> {
    let xml = stanza_to_xml(&outbound.stanza);
    let message = xml.parse::<Element>().expect("outbound MUC XML");
    message
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .expect("MUC user payload")
        .children()
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code"))
        .map(ToOwned::to_owned)
        .collect()
}

#[tokio::test]
async fn fifo_claiming_keeps_two_ordinals_per_revision_ordered_and_lifecycles_independent() {
    let (_db, store) = store_with_db("room-effect-drain-fifo").await;
    let first_lifecycle = lifecycle();
    let second_lifecycle = lifecycle();
    let first_revision = initial_revision();
    let second_revision = first_revision.next().expect("next revision");

    let first = enqueue_and_arm(&store, first_lifecycle, first_revision, admin_effects()).await;
    let second = enqueue_and_arm(&store, first_lifecycle, second_revision, admin_effects()).await;
    let other = enqueue_and_arm(&store, second_lifecycle, first_revision, config_effects()).await;

    let initial = store
        .claim_due_head(super::super::store::HANDLER_GRACE_MS, 8)
        .await
        .expect("initial claims");
    assert_eq!(initial.len(), 2, "each lifecycle exposes one FIFO head");
    assert!(initial.iter().any(|claim| claim.row.key
        == RoomEffectKey {
            lifecycle: first_lifecycle,
            revision: first_revision,
            ordinal: first.ordinals[0],
        }));
    assert!(initial.iter().any(|claim| claim.row.key
        == RoomEffectKey {
            lifecycle: second_lifecycle,
            revision: first_revision,
            ordinal: other.ordinals[0],
        }));
    let later_key = RoomEffectKey {
        lifecycle: first_lifecycle,
        revision: second_revision,
        ordinal: second.ordinals[0],
    };
    assert!(
        store
            .claim_exact(&later_key, super::super::store::HANDLER_GRACE_MS)
            .await
            .expect("later claim")
            .is_none(),
        "a later due row is not claimable while an earlier row exists"
    );
    for claim in initial {
        assert!(
            store
            .complete(&claim.row.key, &claim.lease_token)
            .await
                .expect("complete head")
        );
    }

    let mut observed = vec![(first_revision, first.ordinals[0])];
    for expected in [
        (first_revision, first.ordinals[1]),
        (second_revision, second.ordinals[0]),
        (second_revision, second.ordinals[1]),
    ] {
        let claim = store
            .claim_due_head(super::super::store::HANDLER_GRACE_MS, 8)
            .await
            .expect("next claim")
            .into_iter()
            .next()
            .expect("next FIFO row");
        assert_eq!(
            (claim.row.key.revision, claim.row.key.ordinal),
            expected,
            "one lifecycle drains in revision, ordinal order"
        );
        observed.push(expected);
        assert!(
            store
            .complete(&claim.row.key, &claim.lease_token)
            .await
                .expect("complete FIFO row")
        );
    }
    assert_eq!(
        observed,
        vec![
            (first_revision, first.ordinals[0]),
            (first_revision, first.ordinals[1]),
            (second_revision, second.ordinals[0]),
            (second_revision, second.ordinals[1]),
        ]
    );
}

#[tokio::test]
async fn inline_drain_respects_fifo_across_revisions() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let first = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![alice.clone()]),
        0,
    )
    .await;
    let second = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision().next().expect("next revision"),
        &config_effects_for(&room_jid, vec![alice.clone()]),
        0,
    )
    .await;

    let frames = drain_reservation_inline(state.as_ref(), &first, Some(&alice))
        .await
        .expect("drain first");
    assert_eq!(frames.len(), 1, "the FIFO head should drain first");
    assert!(
        drain_reservation_inline(state.as_ref(), &second, Some(&alice))
            .await
            .expect("drain second while first leased")
            .is_empty(),
        "later revisions must stay blocked until the head completes"
    );
    assert!(
        complete_after_write(state.as_ref(), &frames[0].completion)
            .await
            .expect("complete first"),
        "the head row should delete on completion"
    );

    let frames = drain_reservation_inline(state.as_ref(), &second, Some(&alice))
        .await
        .expect("drain second after first completes");
    assert_eq!(frames.len(), 1, "the next revision should unblock");
    assert!(
        complete_after_write(state.as_ref(), &frames[0].completion)
            .await
            .expect("complete second"),
        "the second row should delete on completion"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .queue_depth()
            .await
            .expect("queue depth"),
        0
    );
}

#[tokio::test]
async fn due_drain_requeues_nonterminal_rows_when_room_is_not_locally_owned() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let lifecycle = lifecycle();
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![full_jid("alice@example.test/device")]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm reservation");

    let summary = drain_due_effects(state.as_ref(), 0, 8)
        .await
        .expect("drain due");
    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 1);
    assert_eq!(summary.stale, 0);

    let row = state
        .deps
        .protocol
        .room_effect_outbox
        .find(&RoomEffectKey {
            lifecycle,
            revision: initial_revision(),
            ordinal: reservation.ordinals[0],
        })
        .await
        .expect("find row")
        .expect("requeued row");
    assert_eq!(
        row.attempt_count, 0,
        "ownership misses must not burn retries"
    );
    assert_eq!(
        row.available_at_ms, 1_000,
        "ownership misses requeue after the fixed delay"
    );
    assert!(
        row.lease_token.is_none(),
        "ownership miss must release the lease"
    );

    state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "room-effect-test".to_owned(),
            channel_id: "room-effect-test".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("acquire local room ownership");
    let recipient = full_jid("alice@example.test/device");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &recipient, tx).await;
    let state_for_owned_drain = Arc::clone(&state);
    let owned_drain =
        tokio::spawn(
            async move { drain_due_effects(state_for_owned_drain.as_ref(), 1_000, 8).await },
        );
    let outbound = recv_outbound(&mut rx).await;
    outbound
        .write_acceptance
        .as_ref()
        .expect("owned-room write acceptance")
        .acknowledge();
    assert_eq!(
        owned_drain
            .await
            .expect("owned drain join")
            .expect("owned drain result")
            .drained,
        1,
        "the requeued row must drain once the room becomes local"
    );
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&RoomEffectKey {
                lifecycle,
                revision: initial_revision(),
                ordinal: reservation.ordinals[0],
            })
            .await
            .expect("find drained row")
            .is_none(),
        "the locally-owned retry must complete the original row"
    );
}

#[tokio::test]
async fn due_drain_executes_terminal_rows_without_local_room_ownership() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let lifecycle = lifecycle();
    let recipient = full_jid("alice@example.test/device");
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &destroy_effects_for(&room_jid, vec![recipient.clone()]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm destroy reservation");

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &recipient, tx).await;
    let state_for_drain = Arc::clone(&state);
    let drain =
        tokio::spawn(async move { drain_due_effects(state_for_drain.as_ref(), 0, 8).await });
    let outbound = recv_outbound(&mut rx).await;
    outbound
        .write_acceptance
        .as_ref()
        .expect("terminal write acceptance")
        .acknowledge();
    let summary = drain
        .await
        .expect("terminal drain join")
        .expect("terminal drain result");
    assert_eq!(summary.drained, 1);
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.stale, 0);
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&RoomEffectKey {
                lifecycle,
                revision: initial_revision(),
                ordinal: reservation.ordinals[0],
            })
            .await
            .expect("find destroy row")
            .is_none(),
        "terminal rows must drain even when no local actor owns the room"
    );
}

#[tokio::test]
async fn due_drain_discards_rows_for_missing_or_reused_lifecycle() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let stale_lifecycle = lifecycle();
    let current_lifecycle = lifecycle();
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        current_lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    let reservation = enqueue_effects(
        state.as_ref(),
        stale_lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![full_jid("alice@example.test/device")]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm reservation");

    let summary = drain_due_effects(state.as_ref(), 0, 8)
        .await
        .expect("drain due");
    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.stale, 1);
    assert_eq!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .pending_rows_for_lifecycle(stale_lifecycle)
            .await
            .expect("pending rows"),
        0
    );

    let tombstoned_lifecycle = lifecycle();
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        tombstoned_lifecycle,
        initial_revision(),
        RoomLifecycleState::Tombstoned,
    )
    .await;
    let tombstoned = enqueue_effects(
        state.as_ref(),
        tombstoned_lifecycle,
        initial_revision(),
        &destroy_effects_for(&room_jid, vec![full_jid("alice@example.test/device")]),
        0,
    )
    .await;
    let tombstoned_key = RoomEffectKey {
        lifecycle: tombstoned_lifecycle,
        revision: initial_revision(),
        ordinal: tombstoned.ordinals[0],
    };
    let tombstoned_row = state
        .deps
        .protocol
        .room_effect_outbox
        .find(&tombstoned_key)
        .await
        .expect("find tombstoned row")
        .expect("tombstoned row");
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .lifecycle_is_executable(&tombstoned_row)
            .await
            .expect("tombstone fence"),
        "the exact lifecycle tombstone remains executable"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn due_drain_redelivers_after_writer_ack_gap_expires() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &alice, tx).await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![alice.clone()]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm reservation");

    let state_for_first = Arc::clone(&state);
    let first =
        tokio::spawn(async move { drain_due_effects(state_for_first.as_ref(), 0, 8).await });
    let first_delivery = recv_outbound(&mut rx).await;
    assert!(
        first_delivery.write_acceptance.is_some(),
        "local delivery should retain a write-acceptance token"
    );
    let summary = first.await.expect("first drain task").expect("first drain");
    assert_eq!(
        summary.drained, 0,
        "timed-out write acceptance must keep the row leased"
    );
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.stale, 0);

    let state_for_second = Arc::clone(&state);
    let second = tokio::spawn(async move {
        drain_due_effects(state_for_second.as_ref(), CLAIM_TIMEOUT_MS + 1, 8).await
    });
    let second_delivery = recv_outbound(&mut rx).await;
    second_delivery
        .write_acceptance
        .as_ref()
        .expect("second delivery keeps write acceptance")
        .acknowledge();
    let summary = second
        .await
        .expect("second drain task")
        .expect("second drain");
    assert_eq!(
        summary.drained, 1,
        "stale leased rows should redeliver at least once"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .queue_depth()
            .await
            .expect("queue depth"),
        0
    );
}

#[tokio::test]
async fn complete_after_write_waits_for_local_acceptance() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let bob = full_jid("bob@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &bob, tx).await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![alice.clone(), bob.clone()]),
        0,
    )
    .await;

    let mut frames = drain_reservation_inline(state.as_ref(), &reservation, Some(&alice))
        .await
        .expect("inline drain");
    assert_eq!(
        frames.len(),
        1,
        "initiator should receive exactly one inline frame"
    );
    let completion = frames.remove(0).completion;
    let outbound = recv_outbound(&mut rx).await;
    let acceptance = outbound
        .write_acceptance
        .clone()
        .expect("local sibling delivery should carry write acceptance");

    let state_for_completion = Arc::clone(&state);
    let completion_for_task = completion.clone();
    let completion_task = tokio::spawn(async move {
        complete_after_write(state_for_completion.as_ref(), &completion_for_task).await
    });
    tokio::task::yield_now().await;
    assert!(
        !completion_task.is_finished(),
        "row completion must wait for local write acceptance"
    );
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&completion.key)
            .await
            .expect("find row")
            .is_some(),
        "the row must stay durable until the local write path accepts it"
    );

    acceptance.acknowledge();
    assert!(
        completion_task
            .await
            .expect("completion task")
            .expect("complete after write"),
        "acknowledged local writes should complete the row"
    );
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&completion.key)
            .await
            .expect("find row")
            .is_none(),
        "the row should delete once the write path accepts it"
    );
}

#[tokio::test]
async fn sequential_logging_transitions_drain_as_distinct_170_then_171_messages() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &alice, alice_tx).await;

    let enable = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &RoomMutationEffects::config(
            room_jid.clone(),
            vec![MucConfigStatusCode::LoggingEnabled],
            vec![alice.clone()],
        ),
        0,
    )
    .await;
    let disable = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision().next().expect("second revision"),
        &RoomMutationEffects::config(
            room_jid,
            vec![MucConfigStatusCode::LoggingDisabled],
            vec![alice.clone()],
        ),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&enable, 0)
        .await
        .expect("arm logging enabled");
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&disable, 0)
        .await
        .expect("arm logging disabled");

    let first_state = Arc::clone(&state);
    let first_drain =
        tokio::spawn(async move { drain_due_effects(first_state.as_ref(), 0, 8).await });
    let enabled = recv_outbound(&mut alice_rx).await;
    let enabled_xml = stanza_to_xml(&enabled.stanza);
    assert_eq!(
        muc_status_codes(&enabled),
        vec!["170".to_owned()],
        "logging enable must be its own first config message: {enabled_xml}"
    );
    enabled
        .write_acceptance
        .as_ref()
        .expect("local logging-enable message has write acceptance")
        .acknowledge();
    assert_eq!(
        first_drain
            .await
            .expect("first drain task")
            .expect("first drain")
            .drained,
        1
    );

    let second_state = Arc::clone(&state);
    let second_drain =
        tokio::spawn(async move { drain_due_effects(second_state.as_ref(), 0, 8).await });
    let disabled = recv_outbound(&mut alice_rx).await;
    let disabled_xml = stanza_to_xml(&disabled.stanza);
    assert_eq!(
        muc_status_codes(&disabled),
        vec!["171".to_owned()],
        "logging disable must follow as a separate config message: {disabled_xml}"
    );
    disabled
        .write_acceptance
        .as_ref()
        .expect("local logging-disable message has write acceptance")
        .acknowledge();
    assert_eq!(
        second_drain
            .await
            .expect("second drain task")
            .expect("second drain")
            .drained,
        1
    );
    assert!(
        alice_rx.try_recv().is_err(),
        "each transition must be delivered exactly once"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reaping_superseded_leased_head_unblocks_terminal_drain() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let lifecycle = lifecycle();
    let store = state.deps.protocol.room_effect_outbox.as_ref();
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    let config = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![alice.clone()]),
        0,
    )
    .await;
    store.arm_reservation(&config, 0).await.expect("arm config");
    let config_key = RoomEffectKey {
        lifecycle,
        revision: initial_revision(),
        ordinal: config.ordinals[0],
    };
    let claim = store
        .claim_exact(&config_key, 0)
        .await
        .expect("claim exact")
        .expect("claimed row");

    let destroy = {
        let mut tx = store.database().begin().await.expect("transaction");
        store
            .supersede_non_terminal_in_tx(&mut tx, lifecycle, 0)
            .await
            .expect("supersede non-terminal rows");
        let destroy = store
            .enqueue_in_tx(
                &mut tx,
                RoomEffectEnqueue {
                    lifecycle,
                    revision: initial_revision().next().expect("next revision"),
                    effects: &destroy_effects_for(&room_jid, vec![alice.clone()]),
                    origin: &origin(),
                    producing_node: &producing_node(),
                    now_ms: 0,
                },
            )
            .await
            .expect("enqueue destroy");
        tx.commit().await.expect("commit");
        destroy
    };
    store
        .arm_reservation(&destroy, 0)
        .await
        .expect("arm destroy");
    assert!(
        !store
            .revalidate(&config_key, &claim.lease_token)
            .await
            .expect("revalidate superseded row"),
        "destroy supersession must fence the leased non-terminal row"
    );
    assert_eq!(
        drain_due_effects(state.as_ref(), 0, 8)
            .await
            .expect("drain before reap")
            .drained,
        0,
        "the terminal row must stay blocked while the superseded lease is still live"
    );

    assert_eq!(
        store
            .reap_superseded(CLAIM_TIMEOUT_MS + 1)
            .await
            .expect("reap superseded"),
        1,
        "expired superseded heads should be deleted before terminal claim"
    );

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &alice, tx).await;
    let state_for_terminal = Arc::clone(&state);
    let terminal = tokio::spawn(async move {
        drain_due_effects(state_for_terminal.as_ref(), CLAIM_TIMEOUT_MS + 1, 8).await
    });
    let outbound = recv_outbound(&mut rx).await;
    outbound
        .write_acceptance
        .as_ref()
        .expect("terminal delivery write acceptance")
        .acknowledge();
    let summary = terminal
        .await
        .expect("terminal drain task")
        .expect("terminal drain");
    assert_eq!(
        summary.drained, 1,
        "terminal row should drain after superseded head reaping"
    );
    assert_eq!(store.queue_depth().await.expect("queue depth"), 0);
}
