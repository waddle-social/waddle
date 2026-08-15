use super::*;

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
    assert!(!store
        .revalidate(&key, &RoomEffectLeaseToken::new())
        .await
        .expect("revalidate"));
    assert!(store
        .revalidate(&key, &claim.lease_token)
        .await
        .expect("revalidate"));
    assert!(!store
        .complete(&key, &RoomEffectLeaseToken::new())
        .await
        .expect("wrong complete"));
    assert!(store
        .complete(&key, &claim.lease_token)
        .await
        .expect("complete"));
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
