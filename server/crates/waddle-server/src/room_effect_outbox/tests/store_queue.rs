use super::*;
use waddle_xmpp::muc::DestroyRecipient;
use waddle_xmpp::ownership::NodeIdentity;

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

fn destroy_effects() -> RoomMutationEffects {
    RoomMutationEffects::destroy(
        room_jid(),
        None,
        None,
        None,
        vec![DestroyRecipient {
            nick: nick("alice"),
            sessions: vec![full_jid("alice@example.test/device")],
        }],
    )
}

#[tokio::test]
async fn enqueue_assigns_contiguous_ordinals_and_empty_is_noop() {
    let (_db, store) = store_with_db("room-effect-ordinals").await;
    let lifecycle = lifecycle();
    let mut tx = store.database().begin().await.expect("transaction");
    let empty = RoomMutationEffects::none();
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: initial_revision(),
                effects: &empty,
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 10,
            },
        )
        .await
        .expect("empty enqueue");
    assert!(reservation.ordinals.is_empty());
    tx.commit().await.expect("commit");
    assert_eq!(
        store
            .pending_rows_for_lifecycle(lifecycle)
            .await
            .expect("count"),
        0
    );

    let revision = initial_revision().next().expect("next revision");
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision,
                effects: &admin_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 10,
            },
        )
        .await
        .expect("admin enqueue");
    tx.commit().await.expect("commit");
    assert_eq!(reservation.ordinals.len(), 2);
    let rows = store.list_for_lifecycle(lifecycle).await.expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key.ordinal.as_i64(), 0);
    assert_eq!(rows[1].key.ordinal.as_i64(), 1);
}

#[tokio::test]
async fn staged_rows_need_exact_claim_or_arming_and_lease_token_interlocks() {
    let (_db, store) = store_with_db("room-effect-claims").await;
    let lifecycle = lifecycle();
    let revision = initial_revision();
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
                now_ms: 100,
            },
        )
        .await
        .expect("enqueue");
    tx.commit().await.expect("commit");
    assert!(store.claim_due_head(100, 4).await.expect("due").is_empty());
    let key = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[0],
    };
    let claim = store
        .claim_exact(&key, 100)
        .await
        .expect("exact")
        .expect("claim");
    assert!(
        !store
        .revalidate(&key, &RoomEffectLeaseToken::new())
        .await
            .expect("revalidate")
    );
    assert!(
        store
        .revalidate(&key, &claim.lease_token)
        .await
            .expect("revalidate")
    );
    assert!(
        !store
        .complete(&key, &RoomEffectLeaseToken::new())
        .await
            .expect("wrong complete")
    );
    assert!(
        store
        .complete(&key, &claim.lease_token)
        .await
            .expect("complete")
    );
}

#[tokio::test]
async fn handler_window_rows_wait_for_grace_but_exact_claim_is_immediate() {
    let (_db, store) = store_with_db("room-effect-handler-window-grace").await;
    let exact_lifecycle = lifecycle();
    let due_lifecycle = lifecycle();
    let revision = initial_revision();
    let now_ms = 100;

    let mut tx = store.database().begin().await.expect("transaction");
    let exact_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: exact_lifecycle,
                revision,
                effects: &admin_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms,
            },
        )
        .await
        .expect("enqueue exact row");
    let due_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: due_lifecycle,
                revision,
                effects: &admin_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms,
            },
        )
        .await
        .expect("enqueue due row");
    tx.commit().await.expect("commit");

    let exact_key = RoomEffectKey {
        lifecycle: exact_lifecycle,
        revision,
        ordinal: exact_reservation.ordinals[0],
    };
    let due_key = RoomEffectKey {
        lifecycle: due_lifecycle,
        revision,
        ordinal: due_reservation.ordinals[0],
    };

    let exact_claim = store
        .claim_exact(&exact_key, now_ms)
        .await
        .expect("exact claim")
        .expect("exact row is immediately claimable");
    assert_eq!(exact_claim.row.key, exact_key);
    assert!(
        store
            .complete(&exact_key, &exact_claim.lease_token)
            .await
            .expect("complete exact claim")
    );

    let due_row = store
        .find(&due_key)
        .await
        .expect("find due row")
        .expect("due row present");
    assert_eq!(
        due_row.available_at_ms,
        now_ms + super::super::store::HANDLER_GRACE_MS
    );
    assert!(
        due_row.available_at_ms > now_ms,
        "handler-window rows must retain a positive grace window"
    );

    assert!(
        store
            .claim_due_head(due_row.available_at_ms - 1, 4)
            .await
            .expect("claim before grace")
            .into_iter()
            .all(|claim| claim.row.key != due_key),
        "claim_due_head must not expose a handler-window row before its grace elapses"
    );
    assert!(
        store
            .claim_due_head(due_row.available_at_ms, 4)
            .await
            .expect("claim at grace boundary")
            .into_iter()
            .any(|claim| claim.row.key == due_key),
        "claim_due_head must expose the row once the grace boundary is reached"
    );
}

#[tokio::test]
async fn reservation_lookup_includes_handler_window_and_leased_rows_until_they_drain() {
    let (_db, store) = store_with_db("room-effect-revision-lookup").await;
    let lifecycle = lifecycle();
    let revision = initial_revision();
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision,
                effects: &admin_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 100,
            },
        )
        .await
        .expect("enqueue reservation");
    tx.commit().await.expect("commit");

    assert_eq!(
        store
            .reservation_for_revision(lifecycle, revision)
            .await
            .expect("lookup handler-window reservation"),
        Some(reservation.clone()),
        "recovery lookup must see handler-window rows before their grace elapses"
    );

    let key = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[0],
    };
    let claim = store
        .claim_exact(&key, 100)
        .await
        .expect("claim leased row")
        .expect("leased row exists");
    assert_eq!(
        store
            .reservation_for_revision(lifecycle, revision)
            .await
            .expect("lookup leased reservation"),
        Some(reservation.clone()),
        "recovery lookup must preserve rows that are currently leased"
    );

    assert!(
        store
            .complete(&key, &claim.lease_token)
            .await
            .expect("complete first row")
    );
    let second_key = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[1],
    };
    let second_claim = store
        .claim_exact(&second_key, 100)
        .await
        .expect("claim second row")
        .expect("second row exists");
    assert!(
        store
            .complete(&second_key, &second_claim.lease_token)
            .await
            .expect("complete second row")
    );
    assert!(
        store
            .reservation_for_revision(lifecycle, revision)
            .await
            .expect("lookup drained reservation")
            .is_none(),
        "recovery lookup must stop returning drained rows"
    );
}

#[tokio::test]
async fn infrastructure_transient_release_keeps_effect_for_a_later_drain() {
    let (_db, store) = store_with_db("room-effect-infrastructure-retry").await;
    let lifecycle = lifecycle();
    let revision = initial_revision();
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
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");
    tx.commit().await.expect("commit");
    store.arm_reservation(&reservation, 0).await.expect("arm");
    let claim = store
        .claim_due_head(0, 1)
        .await
        .expect("claim")
        .pop()
        .expect("claimed effect");

    assert_eq!(
        store
            .release(
                &claim.row.key,
                &claim.lease_token,
                0,
                RoomEffectLastError::InfrastructureTransient,
            )
            .await
            .expect("release transient failure"),
        RoomEffectReleaseOutcome::Released { attempt_count: 1 }
    );
    let row = store
        .find(&claim.row.key)
        .await
        .expect("find released effect")
        .expect("transient failure must retain row");
    assert_eq!(
        row.last_error,
        Some(RoomEffectLastError::InfrastructureTransient)
    );
    assert_eq!(row.attempt_count, 1);
    assert!(row.lease_token.is_none());
    assert!(
        store
            .claim_due_head(row.available_at_ms, 1)
            .await
            .expect("claim after retry delay")
            .into_iter()
            .any(|reclaimed| reclaimed.row.key == claim.row.key),
        "the infrastructure-transient row becomes drainable again"
    );
}

#[tokio::test]
async fn staged_reservation_recovery_and_idempotent_supersession_preserve_transitions() {
    let (_db, store) = store_with_db("room-effect-config-supersession").await;
    let lifecycle = lifecycle();
    let first_revision = initial_revision();
    let mut tx = store.database().begin().await.expect("transaction");
    let first = config_effects();
    let first_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: first_revision,
                effects: &first,
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 10,
            },
        )
        .await
        .expect("enqueue 104");
    tx.commit().await.expect("commit 104");
    assert_eq!(
        store
            .staged_reservation_for(lifecycle, first_revision)
            .await
            .expect("recover reservation"),
        Some(first_reservation)
    );

    let logging_revision = first_revision.next().expect("next revision");
    let logging = RoomMutationEffects::config(
        room_jid(),
        vec![MucConfigStatusCode::LoggingEnabled],
        vec![full_jid("alice@example.test/device")],
    );
    let mut tx = store.database().begin().await.expect("transaction");
    let logging_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: logging_revision,
                effects: &logging,
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 11,
            },
        )
        .await
        .expect("enqueue 170");
    store
        .supersede_idempotent_config_in_tx(
            &mut tx,
            lifecycle,
            &[MucConfigStatusCode::LoggingEnabled],
        )
        .await
        .expect("preserve 104 for a logging-only successor");
    tx.commit().await.expect("commit supersession");

    assert!(
        store
            .staged_reservation_for(lifecycle, first_revision)
            .await
            .expect("first lookup")
            .is_some(),
        "a logging-only successor must not replace a pending 104"
    );
    assert_eq!(
        store
            .staged_reservation_for(lifecycle, logging_revision)
            .await
            .expect("logging lookup"),
        Some(logging_reservation),
        "a pending logging transition must never be superseded"
    );
}

#[tokio::test]
async fn postgres_schema_has_required_room_effect_columns() {
    let Some(url) = std::env::var("WADDLE_TEST_POSTGRES_URL").ok() else {
        return;
    };
    let db = Database::from_config(
        "room-effect-outbox-schema-test",
        &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url),
    )
    .await
    .expect("postgres database");
    RoomEffectOutboxStore::new(db.clone())
        .await
        .expect("schema");
    RoomEffectOutboxStore::new(db.clone())
        .await
        .expect("restart schema");
    let connection = db.guard().await.expect("connection");
    for (column, nullable) in [
        ("lifecycle_id", "NO"),
        ("revision", "NO"),
        ("ordinal", "NO"),
        ("room_jid", "NO"),
        ("kind", "NO"),
        ("terminal", "NO"),
        ("payload_json", "NO"),
        ("available_at_ms", "NO"),
        ("superseded", "NO"),
        ("origin_instance_id", "NO"),
        ("producing_node", "NO"),
        ("lease_token", "YES"),
        ("leased_at_ms", "YES"),
        ("attempt_count", "NO"),
        ("last_error", "YES"),
        ("created_at_ms", "NO"),
    ] {
        let mut rows = connection.query(
            "SELECT is_nullable FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'clustering_muc_room_effects' AND column_name = ?",
            crate::db_params![column],
        ).await.expect("column catalog");
        assert_eq!(
            rows.next()
                .await
                .expect("column row")
                .expect("column")
                .get::<String>(0)
                .expect("nullable"),
            nullable
        );
    }
}

#[tokio::test]
async fn fifo_order_survives_store_restart() {
    let (db, store) = store_with_db("room-effect-restart-fifo").await;
    let lifecycle = lifecycle();
    let first_revision = initial_revision();
    let second_revision = first_revision.next().expect("next revision");

    let first = enqueue_and_arm(&store, lifecycle, first_revision, admin_effects()).await;
    let second = enqueue_and_arm(&store, lifecycle, second_revision, admin_effects()).await;

    drop(store);
    let restarted = RoomEffectOutboxStore::new(db.clone())
        .await
        .expect("restart store");

    let expected = [
        RoomEffectKey {
            lifecycle,
            revision: first_revision,
            ordinal: first.ordinals[0],
        },
        RoomEffectKey {
            lifecycle,
            revision: first_revision,
            ordinal: first.ordinals[1],
        },
        RoomEffectKey {
            lifecycle,
            revision: second_revision,
            ordinal: second.ordinals[0],
        },
        RoomEffectKey {
            lifecycle,
            revision: second_revision,
            ordinal: second.ordinals[1],
        },
    ];

    let mut observed = Vec::new();
    for expected_key in &expected {
        let claim = restarted
            .claim_due_head(super::super::store::HANDLER_GRACE_MS, 8)
            .await
            .expect("claim after restart")
            .into_iter()
            .next()
            .expect("next FIFO row");
        observed.push(claim.row.key);
        assert_eq!(
            observed.last().expect("observed key"),
            expected_key,
            "restarted store must keep revision/ordinal FIFO order"
        );
        assert!(
            restarted
                .complete(expected_key, &claim.lease_token)
                .await
                .expect("complete restarted claim")
        );
    }

    assert_eq!(observed, expected);
    assert_eq!(restarted.queue_depth().await.expect("queue depth"), 0);
}

#[tokio::test]
async fn foreign_stale_epoch_terminal_rows_stay_inert_while_config_rows_arm() {
    let (_db, store) = store_with_db("room-effect-foreign-inert-terminal").await;
    let stale_node = producing_node();
    let current_nodes = vec![RoomEffectProducingNode::from_node_identity(
        NodeIdentity::new("node-a", "epoch-b"),
    )];
    let config_lifecycle = lifecycle();
    let terminal_lifecycle = lifecycle();
    let revision = initial_revision();
    let now_ms = 250;

    let mut tx = store.database().begin().await.expect("transaction");
    let config_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: config_lifecycle,
                revision,
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &stale_node,
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue foreign config row");
    let terminal_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: terminal_lifecycle,
                revision,
                effects: &destroy_effects(),
                origin: &origin(),
                producing_node: &stale_node,
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue foreign terminal row");
    tx.commit().await.expect("commit");

    let foreign_rows = store
        .list_foreign_inert(&current_nodes)
        .await
        .expect("list foreign inert rows");
    assert_eq!(
        foreign_rows
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>(),
        vec![RoomEffectKey {
            lifecycle: config_lifecycle,
            revision,
            ordinal: config_reservation.ordinals[0],
        }],
        "terminal rows must be excluded from foreign inert inventory"
    );

    assert_eq!(
        store
            .arm_foreign_inert(&current_nodes, now_ms)
            .await
            .expect("arm foreign inert rows"),
        1,
        "only stale-epoch config rows should arm"
    );

    let config_row = store
        .find(&RoomEffectKey {
            lifecycle: config_lifecycle,
            revision,
            ordinal: config_reservation.ordinals[0],
        })
        .await
        .expect("find config row")
        .expect("config row exists");
    assert_eq!(config_row.available_at_ms, now_ms);

    let terminal_row = store
        .find(&RoomEffectKey {
            lifecycle: terminal_lifecycle,
            revision,
            ordinal: terminal_reservation.ordinals[0],
        })
        .await
        .expect("find terminal row")
        .expect("terminal row exists");
    assert_eq!(
        terminal_row.available_at_ms,
        i64::MAX,
        "terminal rows must remain inert until their exact terminal completion path arms them"
    );
}
