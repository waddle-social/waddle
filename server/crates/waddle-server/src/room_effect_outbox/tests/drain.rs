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
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity};
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::Stanza;
use xmpp_parsers::minidom::Element;

use super::*;
use crate::room_effect_outbox::drain::{
    complete_after_write, drain_due_effects, drain_reservation_inline,
};
use crate::room_effect_outbox::RoomEffectEnqueue;
use crate::server::routes::websocket::stanza_to_xml;
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, create_test_websocket_state_with_clustering,
    register_test_connection,
};
use crate::server::routes::websocket::WebSocketState;

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

fn inline_admin_effects_for(room_jid: &BareJid, initiator: &FullJid) -> RoomMutationEffects {
    RoomMutationEffects::admin(
        room_jid.clone(),
        vec![OccupantPresenceUpdate {
            recipient: initiator.clone(),
            is_self: true,
            occupant: full_jid("room@muc.example.com/alice"),
            nick: nick("alice"),
            occupant_bare_jid: initiator.to_bare(),
            disclosed_real_jid: Some(initiator.clone()),
            affiliation: waddle_xmpp::Affiliation::Member,
            kind: AdminPresenceKind::Kicked,
            actor: Some(BareJid::from_str("mod@example.test").expect("actor JID")),
            reason: None,
        }],
        vec![OccupantPresenceUpdate {
            recipient: initiator.clone(),
            is_self: false,
            occupant: full_jid("room@muc.example.com/alice"),
            nick: nick("alice"),
            occupant_bare_jid: initiator.to_bare(),
            disclosed_real_jid: Some(initiator.clone()),
            affiliation: waddle_xmpp::Affiliation::Member,
            kind: AdminPresenceKind::RoleChanged(waddle_xmpp::Role::Participant),
            actor: None,
            reason: None,
        }],
        Vec::new(),
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

async fn create_owned_room_and_lifecycle_for(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> RoomLifecycleId {
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
        room_jid,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    lifecycle
}

async fn create_owned_room_and_lifecycle(state: &WebSocketState) -> RoomLifecycleId {
    create_owned_room_and_lifecycle_for(state, &drain_room_jid()).await
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
        assert!(store
            .complete(&claim.row.key, &claim.lease_token)
            .await
            .expect("complete head"));
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
        assert!(store
            .complete(&claim.row.key, &claim.lease_token)
            .await
            .expect("complete FIFO row"));
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
    let blocked = drain_reservation_inline(state.as_ref(), &second, Some(&alice))
        .await
        .expect("drain second while first leased");
    assert!(
        blocked.is_empty(),
        "later revisions must stay blocked until the head completes"
    );
    assert_eq!(
        blocked.summary.blocked, 1,
        "a later revision blocked by an earlier row must be classified as blocked"
    );
    assert_eq!(blocked.summary.leased, 0);
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
async fn inline_drain_claims_later_ordinal_when_earlier_row_is_leased_by_same_drain() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let initiator = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &inline_admin_effects_for(&room_jid, &initiator),
        0,
    )
    .await;

    let mut frames = drain_reservation_inline(state.as_ref(), &reservation, Some(&initiator))
        .await
        .expect("inline drain");
    assert_eq!(
        frames.len(),
        2,
        "the same inline drain must advance past its own leased ordinal-0 head"
    );
    assert_eq!(frames.summary.inline, 2);
    for frame in frames.drain(..) {
        assert!(
            complete_after_write(state.as_ref(), &frame.completion)
                .await
                .expect("complete inline frame"),
            "each retained inline lease should complete cleanly"
        );
    }
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
async fn inline_drain_keeps_later_ordinal_blocked_when_earlier_head_has_foreign_lease() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let initiator = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &inline_admin_effects_for(&room_jid, &initiator),
        0,
    )
    .await;
    let first_key = RoomEffectKey {
        lifecycle,
        revision: initial_revision(),
        ordinal: reservation.ordinals[0],
    };
    let foreign_claim = state
        .deps
        .protocol
        .room_effect_outbox
        .claim_exact(&first_key, crate::time::now_ms())
        .await
        .expect("foreign claim")
        .expect("foreign lease");

    let blocked = drain_reservation_inline(state.as_ref(), &reservation, Some(&initiator))
        .await
        .expect("inline drain with foreign head lease");
    assert!(
        blocked.is_empty(),
        "a foreign lease on ordinal 0 must still block ordinal 1"
    );
    assert_eq!(blocked.summary.leased, 1);
    assert_eq!(blocked.summary.blocked, 1);
    assert!(state
        .deps
        .protocol
        .room_effect_outbox
        .release_unattempted(&first_key, &foreign_claim.lease_token, 0, 0)
        .await
        .expect("release foreign lease"));
}

#[tokio::test]
async fn inline_drain_reports_live_exact_row_as_leased_not_blocked() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let alice = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref()).await;
    let reservation = enqueue_effects(
        state.as_ref(),
        lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![alice.clone()]),
        0,
    )
    .await;

    let first = drain_reservation_inline(state.as_ref(), &reservation, Some(&alice))
        .await
        .expect("first inline drain");
    assert_eq!(first.len(), 1);

    let second = drain_reservation_inline(state.as_ref(), &reservation, Some(&alice))
        .await
        .expect("second inline drain while leased");
    assert!(second.is_empty());
    assert_eq!(second.summary.leased, 1);
    assert_eq!(second.summary.blocked, 0);

    assert!(
        complete_after_write(state.as_ref(), &first[0].completion)
            .await
            .expect("complete first"),
        "leased exact rows should still complete once the retained frame is acknowledged"
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
        row.available_at_ms, 15_000,
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
            async move { drain_due_effects(state_for_owned_drain.as_ref(), 15_000, 8).await },
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

#[tokio::test(flavor = "current_thread")]
async fn due_drain_times_out_a_full_local_channel_and_keeps_other_lifecycles_moving() {
    let state = create_test_websocket_state().await;
    let blocked_room: BareJid = "blocked@muc.example.com".parse().expect("blocked room");
    let healthy_room: BareJid = "healthy@muc.example.com".parse().expect("healthy room");
    let blocked_recipient = full_jid("blocked@example.test/device");
    let healthy_recipient = full_jid("healthy@example.test/device");
    let blocked_lifecycle =
        create_owned_room_and_lifecycle_for(state.as_ref(), &blocked_room).await;
    let healthy_lifecycle =
        create_owned_room_and_lifecycle_for(state.as_ref(), &healthy_room).await;

    let blocked = enqueue_effects(
        state.as_ref(),
        blocked_lifecycle,
        initial_revision(),
        &config_effects_for(&blocked_room, vec![blocked_recipient.clone()]),
        0,
    )
    .await;
    let healthy = enqueue_effects(
        state.as_ref(),
        healthy_lifecycle,
        initial_revision(),
        &config_effects_for(&healthy_room, vec![healthy_recipient.clone()]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&blocked, 0)
        .await
        .expect("arm blocked reservation");
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&healthy, 0)
        .await
        .expect("arm healthy reservation");

    let (blocked_tx, _blocked_rx) = mpsc::channel::<OutboundStanza>(1);
    register_test_connection(state.as_ref(), &blocked_recipient, blocked_tx.clone()).await;
    blocked_tx
        .send(OutboundStanza::new(Stanza::Presence(
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None),
        )))
        .await
        .expect("prefill blocked recipient channel");
    let (healthy_tx, mut healthy_rx) = mpsc::channel::<OutboundStanza>(4);
    register_test_connection(state.as_ref(), &healthy_recipient, healthy_tx).await;

    let state_for_drain = Arc::clone(&state);
    let drain =
        tokio::spawn(async move { drain_due_effects(state_for_drain.as_ref(), 0, 8).await });
    let outbound = tokio::time::timeout(Duration::from_secs(7), healthy_rx.recv())
        .await
        .expect("healthy lifecycle must drain after the blocked enqueue timeout")
        .expect("healthy outbound");
    outbound
        .write_acceptance
        .as_ref()
        .expect("healthy write acceptance")
        .acknowledge();
    let summary = drain.await.expect("drain join").expect("drain result");

    assert_eq!(summary.drained, 1);
    assert_eq!(summary.requeued, 1);
    assert_eq!(summary.stale, 0);
    assert_eq!(summary.dead_lettered, 0);

    let blocked_row = state
        .deps
        .protocol
        .room_effect_outbox
        .find(&RoomEffectKey {
            lifecycle: blocked_lifecycle,
            revision: initial_revision(),
            ordinal: blocked.ordinals[0],
        })
        .await
        .expect("find blocked row")
        .expect("blocked row persists for retry");
    assert_eq!(blocked_row.attempt_count, 1);
    assert_eq!(
        blocked_row.last_error,
        Some(RoomEffectLastError::InfrastructureTransient)
    );
    assert_eq!(
        blocked_row.available_at_ms,
        super::super::store::retry_delay_ms(1)
    );
    assert!(blocked_row.lease_token.is_none());
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&RoomEffectKey {
                lifecycle: healthy_lifecycle,
                revision: initial_revision(),
                ordinal: healthy.ordinals[0],
            })
            .await
            .expect("find healthy row")
            .is_none(),
        "the healthy lifecycle must still drain in the same sweep"
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
async fn due_drain_dead_letters_old_unclaimed_nonterminal_rows() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let lifecycle = lifecycle();
    let now_ms = 24 * 60 * 60 * 1_000 + 1;
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

    let summary = drain_due_effects(state.as_ref(), now_ms, 8)
        .await
        .expect("drain due");
    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 0);
    assert_eq!(summary.stale, 0);
    assert_eq!(summary.dead_lettered, 1);
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
            .expect("find dead-lettered row")
            .is_none(),
        "an unowned row older than 24h should be deleted"
    );
}

#[tokio::test]
async fn due_drain_retains_old_nonterminal_rows_when_a_foreign_claim_exists() {
    let room_jid = drain_room_jid();
    let claim_store = Arc::new(InProcessClaimStore::new());
    claim_store.ensure_schema().await.expect("claim schema");
    claim_store
        .acquire(
            &Entity::new(EntityType::RoomActor, room_jid.to_string()),
            &NodeIdentity::new("foreign-node", "foreign-epoch"),
        )
        .await
        .expect("foreign room claim");
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            claim_store: Some(claim_store as Arc<dyn ClaimStore>),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(InMemorySmSessionRegistry::new()),
    )
    .await;
    let lifecycle = lifecycle();
    let now_ms = 24 * 60 * 60 * 1_000 + 1;
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

    let summary = drain_due_effects(state.as_ref(), now_ms, 8)
        .await
        .expect("drain due");
    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 1);
    assert_eq!(summary.stale, 0);
    assert_eq!(summary.dead_lettered, 0);
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
        .expect("find requeued row")
        .expect("requeued row");
    assert_eq!(row.available_at_ms, now_ms + 15_000);
    assert!(row.lease_token.is_none());
}

#[tokio::test]
async fn due_drain_never_dead_letters_old_terminal_rows() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let lifecycle = lifecycle();
    let recipient = full_jid("alice@example.test/device");
    let now_ms = 24 * 60 * 60 * 1_000 + 1;
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
        tokio::spawn(async move { drain_due_effects(state_for_drain.as_ref(), now_ms, 8).await });
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
    assert_eq!(summary.dead_lettered, 0);
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
async fn due_drain_discards_nonterminal_rows_for_tombstoned_lifecycle() {
    let state = create_test_websocket_state().await;
    let room_jid = drain_room_jid();
    let tombstoned_lifecycle = lifecycle();
    let recipient = full_jid("alice@example.test/device");
    insert_lifecycle_row(
        state.as_ref(),
        &room_jid,
        tombstoned_lifecycle,
        initial_revision(),
        RoomLifecycleState::Tombstoned,
    )
    .await;
    let reservation = enqueue_effects(
        state.as_ref(),
        tombstoned_lifecycle,
        initial_revision(),
        &config_effects_for(&room_jid, vec![recipient]),
        0,
    )
    .await;
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm non-terminal tombstoned reservation");

    let key = RoomEffectKey {
        lifecycle: tombstoned_lifecycle,
        revision: initial_revision(),
        ordinal: reservation.ordinals[0],
    };
    let row = state
        .deps
        .protocol
        .room_effect_outbox
        .find(&key)
        .await
        .expect("find non-terminal tombstoned row")
        .expect("non-terminal tombstoned row");
    assert!(
        !state
            .deps
            .protocol
            .room_effect_outbox
            .lifecycle_is_executable(&row)
            .await
            .expect("non-terminal tombstone fence"),
        "only terminal rows may execute from a tombstoned lifecycle"
    );

    let summary = drain_due_effects(state.as_ref(), 0, 8)
        .await
        .expect("drain tombstoned non-terminal");
    assert_eq!(summary.stale, 1);
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .find(&key)
            .await
            .expect("find drained stale row")
            .is_none(),
        "non-terminal rows fenced by a tombstoned lifecycle must be discarded"
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
