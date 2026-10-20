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
async fn renew_lease_rejects_superseded_rows() {
    let (_db, store) = store_with_db("room-effect-renew-superseded").await;
    let lifecycle = lifecycle();
    let reservation =
        enqueue_and_arm(&store, lifecycle, initial_revision(), config_effects()).await;
    let key = RoomEffectKey {
        lifecycle,
        revision: initial_revision(),
        ordinal: reservation.ordinals[0],
    };
    let claim = store
        .claim_due_head(0, 1)
        .await
        .expect("claim due row")
        .pop()
        .expect("claimed row");

    let mut tx = store.database().begin().await.expect("transaction");
    store
        .supersede_non_terminal_in_tx(&mut tx, lifecycle, 0)
        .await
        .expect("supersede leased row");
    tx.commit().await.expect("commit supersession");

    assert!(
        !store
            .renew_lease(&key, &claim.lease_token, 1)
            .await
            .expect("renew superseded row"),
        "renewal must fail once a leased row is superseded"
    );
}

#[tokio::test]
async fn batch_64_later_chunk_uses_fresh_lease_time_against_competing_claimant() {
    let (_db, store) = store_with_db("room-effect-batch-64-fresh-lease").await;
    for _ in 0..64 {
        enqueue_and_arm(
            &store,
            RoomLifecycleId::generate(),
            initial_revision(),
            config_effects(),
        )
        .await;
    }

    for _ in 0..7 {
        let claimed = store
            .claim_due_head_with_lease_time(0, 8, 0)
            .await
            .expect("claim an early batch-64 chunk");
        assert_eq!(claimed.len(), 8);
        for effect in claimed {
            assert!(store
                .complete(&effect.row.key, &effect.lease_token)
                .await
                .expect("complete early chunk"));
        }
    }

    let late_lease_ms = CLAIM_TIMEOUT_MS + 10;
    let late_chunk = store
        .claim_due_head_with_lease_time(0, 8, late_lease_ms)
        .await
        .expect("claim late chunk with fresh lease timestamp");
    assert_eq!(late_chunk.len(), 8);

    let competing_now_ms = late_lease_ms + 1;
    assert!(
        store
            .claim_due_head(competing_now_ms, 8)
            .await
            .expect("competing claimant")
            .is_empty(),
        "a later batch-64 chunk must not be born stale and stolen immediately"
    );
    for effect in late_chunk {
        assert_eq!(
            store
                .find(&effect.row.key)
                .await
                .expect("find late chunk row")
                .expect("late chunk row remains")
                .lease_token,
            Some(effect.lease_token)
        );
    }
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
    assert!(store
        .complete(&exact_key, &exact_claim.lease_token)
        .await
        .expect("complete exact claim"));

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

    assert!(store
        .complete(&key, &claim.lease_token)
        .await
        .expect("complete first row"));
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
    assert!(store
        .complete(&second_key, &second_claim.lease_token)
        .await
        .expect("complete second row"));
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
async fn staged_reservations_up_to_returns_inert_non_terminal_rows_oldest_first() {
    let (_db, store) = store_with_db("room-effect-staged-up-to").await;
    let lifecycle = lifecycle();
    let revisions = [3_i64, 4, 5, 6, 9]
        .into_iter()
        .map(|stored| RoomRevision::from_stored(stored).expect("positive revision"))
        .collect::<Vec<_>>();

    let mut tx = store.database().begin().await.expect("transaction");
    let revision_three = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: revisions[0],
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 10,
            },
        )
        .await
        .expect("enqueue revision 3");
    let armed_revision = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: revisions[1],
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 11,
            },
        )
        .await
        .expect("enqueue revision 4");
    let revision_five = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: revisions[2],
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 12,
            },
        )
        .await
        .expect("enqueue revision 5");
    let terminal_revision = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: revisions[3],
                effects: &destroy_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 13,
            },
        )
        .await
        .expect("enqueue revision 6");
    let above_bound = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: revisions[4],
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 14,
            },
        )
        .await
        .expect("enqueue revision 9");
    tx.commit().await.expect("commit");

    store
        .arm_reservation(&armed_revision, 20)
        .await
        .expect("arm revision 4");

    let recovered = store
        .staged_reservations_up_to(
            lifecycle,
            RoomRevision::from_stored(8).expect("positive bound revision"),
        )
        .await
        .expect("lookup staged reservations up to bound");

    assert_eq!(
        recovered,
        vec![
            waddle_xmpp::muc::RoomEffectReservation {
                lifecycle,
                revision: revisions[0],
                ordinals: revision_three.ordinals,
            },
            waddle_xmpp::muc::RoomEffectReservation {
                lifecycle,
                revision: revisions[2],
                ordinals: revision_five.ordinals,
            },
        ]
    );
    assert!(
        recovered.iter().all(|reservation| {
            reservation
                .ordinals
                .windows(2)
                .all(|window| window[0].as_i64() < window[1].as_i64())
        }),
        "each revision group must preserve ascending ordinals"
    );
    assert_eq!(
        store
            .find(&RoomEffectKey {
                lifecycle,
                revision: terminal_revision.revision,
                ordinal: terminal_revision.ordinals[0],
            })
            .await
            .expect("find terminal row")
            .expect("terminal row")
            .available_at_ms,
        i64::MAX,
        "terminal rows remain inert but are excluded from staged recovery"
    );
    assert_eq!(
        store
            .find(&RoomEffectKey {
                lifecycle,
                revision: above_bound.revision,
                ordinal: above_bound.ordinals[0],
            })
            .await
            .expect("find above-bound row")
            .expect("above-bound row")
            .available_at_ms,
        i64::MAX,
        "rows above the bound remain inert but are excluded from staged recovery"
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
async fn pure_104_successor_coalesces_empty_104_but_preserves_voice_changes() {
    let (_db, store) = store_with_db("room-effect-config-pure-104-supersession").await;
    let pure_lifecycle = lifecycle();
    let first_revision = initial_revision();

    let mut tx = store.database().begin().await.expect("transaction");
    let empty_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: pure_lifecycle,
                revision: first_revision,
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 10,
            },
        )
        .await
        .expect("enqueue empty 104");
    tx.commit().await.expect("commit empty 104");
    assert_eq!(
        store
            .staged_reservation_for(pure_lifecycle, first_revision)
            .await
            .expect("lookup empty 104"),
        Some(empty_reservation.clone())
    );

    let pure_104_revision = first_revision.next().expect("next revision");
    let mut tx = store.database().begin().await.expect("transaction");
    assert_eq!(
        store
            .supersede_idempotent_config_in_tx(
                &mut tx,
                pure_lifecycle,
                &[MucConfigStatusCode::NonPrivacyConfigurationChange],
            )
            .await
            .expect("coalesce empty 104"),
        1
    );
    let pure_104_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: pure_lifecycle,
                revision: pure_104_revision,
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 11,
            },
        )
        .await
        .expect("enqueue successor 104");
    tx.commit().await.expect("commit pure 104 supersession");

    assert!(
        store
            .staged_reservation_for(pure_lifecycle, first_revision)
            .await
            .expect("lookup coalesced empty 104")
            .is_none(),
        "a pure-104 successor should consume the staged empty 104"
    );
    assert_eq!(
        store
            .staged_reservation_for(pure_lifecycle, pure_104_revision)
            .await
            .expect("lookup successor 104"),
        Some(pure_104_reservation)
    );

    let voiced_lifecycle = lifecycle();
    let voiced_revision = initial_revision();
    let voiced_effects = RoomMutationEffects::config_with_voice_changes(
        room_jid(),
        vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
        vec![full_jid("alice@example.test/device")],
        vec![OccupantVoiceChange {
            session: full_jid("carol@example.test/device"),
            voice: waddle_xmpp::Voice::Muted,
        }],
    );

    let mut tx = store.database().begin().await.expect("transaction");
    let voiced_reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: voiced_lifecycle,
                revision: voiced_revision,
                effects: &voiced_effects,
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 20,
            },
        )
        .await
        .expect("enqueue voiced 104");
    tx.commit().await.expect("commit voiced 104");
    assert_eq!(
        store
            .staged_reservation_for(voiced_lifecycle, voiced_revision)
            .await
            .expect("lookup voiced 104"),
        Some(voiced_reservation.clone())
    );

    let voiced_successor_revision = voiced_revision.next().expect("next voiced revision");
    let mut tx = store.database().begin().await.expect("transaction");
    assert_eq!(
        store
            .supersede_idempotent_config_in_tx(
                &mut tx,
                voiced_lifecycle,
                &[MucConfigStatusCode::NonPrivacyConfigurationChange],
            )
            .await
            .expect("preserve voiced 104"),
        0
    );
    let voiced_successor = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle: voiced_lifecycle,
                revision: voiced_successor_revision,
                effects: &config_effects(),
                origin: &origin(),
                producing_node: &producing_node(),
                now_ms: 21,
            },
        )
        .await
        .expect("enqueue successor after voiced 104");
    tx.commit()
        .await
        .expect("commit voiced 104 supersession attempt");

    assert_eq!(
        store
            .staged_reservation_for(voiced_lifecycle, voiced_revision)
            .await
            .expect("lookup preserved voiced 104"),
        Some(voiced_reservation),
        "a pure-104 successor must not consume staged voice transitions"
    );
    assert_eq!(
        store
            .staged_reservation_for(voiced_lifecycle, voiced_successor_revision)
            .await
            .expect("lookup voiced successor"),
        Some(voiced_successor)
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
        ("unowned_since_ms", "YES"),
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
        assert!(restarted
            .complete(expected_key, &claim.lease_token)
            .await
            .expect("complete restarted claim"));
    }

    assert_eq!(observed, expected);
    assert_eq!(restarted.queue_depth().await.expect("queue depth"), 0);
}

#[tokio::test]
async fn owner_release_clears_unowned_since() {
    let (_db, store) = store_with_db("room-effect-owner-release-clears-unowned").await;
    let lifecycle = lifecycle();
    let reservation =
        enqueue_and_arm(&store, lifecycle, initial_revision(), config_effects()).await;
    let key = RoomEffectKey {
        lifecycle,
        revision: initial_revision(),
        ordinal: reservation.ordinals[0],
    };
    let claim = store
        .claim_due_head(0, 1)
        .await
        .expect("claim due row")
        .pop()
        .expect("claimed row");

    assert_eq!(
        store
            .note_unowned_since_if_absent(&key, &claim.lease_token, 10)
            .await
            .expect("mark unowned"),
        Some(10)
    );
    assert_eq!(
        store
            .find(&key)
            .await
            .expect("find row")
            .expect("row")
            .unowned_since_ms,
        Some(10)
    );

    assert_eq!(
        store
            .release(
                &key,
                &claim.lease_token,
                11,
                RoomEffectLastError::InfrastructureTransient,
            )
            .await
            .expect("release"),
        RoomEffectReleaseOutcome::Released { attempt_count: 1 }
    );
    assert_eq!(
        store
            .find(&key)
            .await
            .expect("find row")
            .expect("row")
            .unowned_since_ms,
        None
    );
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

#[tokio::test]
async fn requeued_head_never_lets_its_successor_absorb_a_small_claim_batch() {
    // Regression (sol impl-review r2 #2): the due-candidate preselection must
    // filter to per-lifecycle FIFO heads BEFORE ORDER BY/LIMIT. A requeued
    // head carries a LATER available_at_ms than its successor ordinal; with a
    // batch of 1 the successor would otherwise be selected every sweep,
    // rejected by the FIFO CAS, and the lifecycle would never drain.
    let (_db, store) = store_with_db("room-effect-requeued-head-batch").await;
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
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue two admin ordinals");
    tx.commit().await.expect("commit");
    store
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm both ordinals");
    let head = RoomEffectKey {
        lifecycle,
        revision,
        ordinal: reservation.ordinals[0],
    };
    let claim = store
        .claim_exact(&head, 10)
        .await
        .expect("claim head")
        .expect("head claimable");
    store
        .release(
            &head,
            &claim.lease_token,
            10,
            RoomEffectLastError::InfrastructureTransient,
        )
        .await
        .expect("requeue head with backoff");
    let head_available = store
        .find(&head)
        .await
        .expect("find requeued head")
        .expect("head row")
        .available_at_ms;
    assert!(head_available > 10, "release must push the head's due time");

    // While only the successor is due, a batch of 1 must select NOTHING —
    // not the unclaimable successor.
    let claimed = store
        .claim_due_head(head_available - 1, 1)
        .await
        .expect("claim before head due");
    assert!(
        claimed.is_empty(),
        "successor ordinal must not absorb the batch window while the head is requeued"
    );

    // Once the head is due again, the same batch of 1 selects the head.
    let claimed = store
        .claim_due_head(head_available, 1)
        .await
        .expect("claim at head due");
    assert_eq!(claimed.len(), 1, "head must be claimable at its due time");
    assert_eq!(claimed[0].row.key, head, "the FIFO head must be selected");
}
