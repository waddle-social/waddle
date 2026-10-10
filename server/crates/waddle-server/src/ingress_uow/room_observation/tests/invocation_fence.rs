use super::*;
use waddle_extensions::{ObservationFailure, ObservationSkip};

async fn seed(fixture: &IngressFixture) -> (ConfiguredRoomObserver, i64) {
    seed_with_stanza(fixture, "stanza").await
}

async fn seed_with_stanza(fixture: &IngressFixture, stanza: &str) -> (ConfiguredRoomObserver, i64) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let now = Utc::now();
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("seed");
    record_message(&mut tx, key).await;
    Repo::sync_configured(
        &mut tx,
        std::slice::from_ref(&observer),
        crate::time::now_ms(),
    )
    .await
    .expect("config");
    capture(
        &mut tx,
        key,
        &room(),
        &message("wire", stanza, Some("origin"), "body"),
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("seed commit");
    (observer, now.timestamp_millis())
}

async fn terminal_outcome_cannot_erase_prior_uncertainty(
    fixture: IngressFixture,
    outcome: RoomObservationOutcome,
    cancel: bool,
) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut last = None;
    for index in 0..20 {
        let at = now + index * 180_000;
        let mut tx = fixture.uow.begin().await.expect("started invocation");
        let work = Repo::claim(&mut tx, &subscription, at)
            .await
            .expect("claim")
            .expect("work");
        assert_eq!(i64::from(work.attempt), index + 1);
        assert!(Repo::start(&mut tx, &work, at).await.expect("start"));
        tx.commit()
            .await
            .expect("invocation commits before lost result");
        last = Some(work);
    }
    let first = last.expect("expired twentieth invocation");
    let at = now + 20 * 180_000;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("expired invocation recovery");
    let retry = Repo::claim(&mut tx, &subscription, at)
        .await
        .expect("reclaim")
        .expect("retry");
    assert_ne!(retry.lease, first.lease);
    assert_eq!(retry.attempt, 21);
    assert!(!Repo::finish(&mut tx, &first, &outcome, at)
        .await
        .expect("old lease remains fenced"));
    assert!(Repo::start(&mut tx, &retry, at)
        .await
        .expect("retry starts"));
    assert!(Repo::finish(&mut tx, &retry, &outcome, at)
        .await
        .expect("record retry outcome"));
    tx.commit()
        .await
        .expect("later known outcome preserves earlier unknown invocation");
    assert_eq!(fixture.count("extension_room_observation_work WHERE status = 'pending' AND terminal_category = 'unknown_after_send' AND body = 'body' AND lease_id IS NULL AND lease_until_ms IS NULL AND settled_at_ms IS NULL").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        0
    );
    assert_eq!(fixture.count("extension_room_publications").await, 0);

    let mut tx = fixture.uow.begin().await.expect("bounded retry backoff");
    assert!(Repo::claim(&mut tx, &subscription, at + 59_999)
        .await
        .expect("not due")
        .is_none());
    let next_at = at + 60_000;
    let next = Repo::claim(&mut tx, &subscription, next_at)
        .await
        .expect("due retry")
        .expect("uncertainty remains reclaimable");
    assert_ne!(next.lease, retry.lease);
    assert_eq!(next.attempt, 22);
    assert!(Repo::start(&mut tx, &next, next_at)
        .await
        .expect("new lease starts"));
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(!Repo::finish(&mut tx, &retry, &result, next_at)
        .await
        .expect("previous retry result fenced"));
    if cancel {
        Repo::sync_configured(&mut tx, &[configured_observer(2, 'b')], next_at)
            .await
            .expect("explicit generation cancellation");
        assert!(!Repo::finish(&mut tx, &next, &result, next_at)
            .await
            .expect("cancelled callback fenced"));
    } else {
        assert!(Repo::finish(&mut tx, &next, &result, next_at)
            .await
            .expect("successful completion resolves uncertainty"));
    }
    tx.commit().await.expect("explicit resolution");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        1
    );
    assert_eq!(
        fixture.count("extension_room_publications").await,
        i64::from(!cancel)
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'pending'")
            .await,
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_permanent_observer_failure_preserves_prior_unknown_invocation() {
    terminal_outcome_cannot_erase_prior_uncertainty(
        IngressFixture::sqlite().await,
        RoomObservationOutcome::PermanentFailure(ObservationFailure::Denied),
        false,
    )
    .await;
}

#[tokio::test]
async fn postgres_permanent_observer_failure_preserves_prior_unknown_invocation() {
    if let Some(fixture) = IngressFixture::postgres("observer_sticky_permanent").await {
        terminal_outcome_cannot_erase_prior_uncertainty(
            fixture,
            RoomObservationOutcome::PermanentFailure(ObservationFailure::Denied),
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn sqlite_observer_not_applicable_preserves_prior_unknown_invocation() {
    terminal_outcome_cannot_erase_prior_uncertainty(
        IngressFixture::sqlite().await,
        RoomObservationOutcome::NotApplicable(ObservationSkip::SubscriptionUnavailable),
        true,
    )
    .await;
}

#[tokio::test]
async fn postgres_observer_not_applicable_preserves_prior_unknown_invocation() {
    if let Some(fixture) = IngressFixture::postgres("observer_sticky_skip").await {
        terminal_outcome_cannot_erase_prior_uncertainty(
            fixture,
            RoomObservationOutcome::NotApplicable(ObservationSkip::SubscriptionUnavailable),
            true,
        )
        .await;
    }
}

async fn known_terminal_observer_outcomes_still_settle(fixture: IngressFixture) {
    for (index, (stanza, outcome, category)) in [
        (
            "known-permanent",
            RoomObservationOutcome::PermanentFailure(ObservationFailure::Denied),
            "denied",
        ),
        (
            "known-skip",
            RoomObservationOutcome::NotApplicable(ObservationSkip::SubscriptionUnavailable),
            "subscription_unavailable",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (observer, now) = seed_with_stanza(&fixture, stanza).await;
        let subscription = subscription(&observer);
        let mut tx = fixture.uow.begin().await.expect("known first invocation");
        let work = Repo::claim(&mut tx, &subscription, now)
            .await
            .expect("claim")
            .expect("work");
        assert!(Repo::start(&mut tx, &work, now).await.expect("first start"));
        assert!(Repo::finish(&mut tx, &work, &outcome, now)
            .await
            .expect("known terminal outcome"));
        assert!(Repo::claim(&mut tx, &subscription, now + 180_000)
            .await
            .expect("no unresolved invocation")
            .is_none());
        tx.commit().await.expect("real terminal receipt");
        let conn = fixture.db.guard().await.expect("known terminal state");
        let mut rows = conn.query("SELECT status, terminal_category, body, settled_at_ms FROM extension_room_observation_work WHERE id = ?", crate::db_params![work.id.to_string()]).await.expect("work row");
        let row = rows.next().await.expect("row").expect("terminal work");
        assert_eq!(row.get::<String>(0).expect("status"), "terminal");
        assert_eq!(row.get::<String>(1).expect("category"), category);
        assert_eq!(row.get::<String>(2).expect("body"), "");
        assert_eq!(
            row.get::<Option<i64>>(3).expect("settlement time"),
            Some(now)
        );
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            i64::try_from(index + 1).expect("count")
        );
        assert_eq!(
            fixture.count("extension_room_observation_receipts").await,
            i64::try_from(index + 1).expect("count")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_known_terminal_observer_outcomes_still_settle() {
    known_terminal_observer_outcomes_still_settle(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_known_terminal_observer_outcomes_still_settle() {
    if let Some(fixture) = IngressFixture::postgres("observer_known_terminal").await {
        known_terminal_observer_outcomes_still_settle(fixture).await;
    }
}

async fn expired_started_work_retries_with_fenced_results(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let first = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    let mut tx = fixture.uow.begin().await.expect("start");
    assert!(!Repo::finish(&mut tx, &first, &result, now)
        .await
        .expect("must start first"));
    assert!(!Repo::start(&mut tx, &first, now + 180_000)
        .await
        .expect("expired reservation"));
    assert!(Repo::start(&mut tx, &first, now).await.expect("start"));
    assert!(!Repo::start(&mut tx, &first, now).await.expect("start once"));
    tx.commit().await.expect("start commit");

    let mut tx = fixture.uow.begin().await.expect("before expiry");
    assert!(Repo::claim(&mut tx, &subscription, now + 179_999)
        .await
        .expect("claim")
        .is_none());
    assert!(Repo::due_rooms(&mut tx, &observer, None, now + 179_999, 10)
        .await
        .expect("due")
        .is_empty());
    for failure in [
        ObservationFailure::TemporaryFailure,
        ObservationFailure::DeadlineExceeded,
        ObservationFailure::RuntimeFailure,
    ] {
        assert!(!Repo::finish(
            &mut tx,
            &first,
            &RoomObservationOutcome::UnresolvedFailure(failure),
            now + 1
        )
        .await
        .expect("ambiguous"));
    }
    tx.commit().await.expect("uncertain commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("extension_room_publications").await, 0);

    // A crash after start is bounded by the lease. Replacement changes the
    // token before guest entry, so a late result from the first run is fenced.
    let mut tx = fixture.uow.begin().await.expect("expired recovery");
    assert_eq!(
        Repo::due_rooms(&mut tx, &observer, None, now + 180_000, 10)
            .await
            .expect("due"),
        vec![room()]
    );
    let mut all_rooms = observer.clone();
    all_rooms.scope = RoomObservationScope::AllHostedRooms;
    assert_eq!(
        Repo::due_rooms(&mut tx, &all_rooms, None, now + 180_000, 10)
            .await
            .expect("all hosted due"),
        vec![room()]
    );
    let retry = Repo::claim(&mut tx, &subscription, now + 180_000)
        .await
        .expect("retry")
        .expect("work");
    assert_ne!(retry.lease, first.lease);
    assert_eq!(retry.attempt, 2);
    assert!(!Repo::finish(&mut tx, &first, &result, now + 180_000)
        .await
        .expect("stale result"));
    assert!(!Repo::start(&mut tx, &first, now + 180_000)
        .await
        .expect("stale start"));
    assert!(Repo::start(&mut tx, &retry, now + 180_000)
        .await
        .expect("retry start"));
    tx.commit().await.expect("retry commit");

    // Publication and receipt still share one transaction.
    let mut tx = fixture.uow.begin().await.expect("finish rollback");
    assert!(Repo::finish(&mut tx, &retry, &result, now + 180_001)
        .await
        .expect("finish"));
    drop(tx);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(Repo::finish(&mut tx, &retry, &result, now + 180_002)
        .await
        .expect("finish"));
    assert!(!Repo::finish(&mut tx, &retry, &result, now + 180_002)
        .await
        .expect("finish once"));
    tx.commit().await.expect("finish commit");
    assert_eq!(fixture.count("extension_room_publications").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

async fn not_invoked_retries_before_ambiguous_lease_expiry(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let first = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    assert!(Repo::start(&mut tx, &first, now).await.expect("start"));
    tx.commit().await.expect("start commit");
    let mut tx = fixture.uow.begin().await.expect("not invoked");
    assert!(
        Repo::finish(&mut tx, &first, &RoomObservationOutcome::NotInvoked, now)
            .await
            .expect("retry")
    );
    assert!(Repo::claim(&mut tx, &subscription, now + 999)
        .await
        .expect("backoff")
        .is_none());
    let second = Repo::claim(&mut tx, &subscription, now + 1000)
        .await
        .expect("claim")
        .expect("work");
    assert_ne!(first.lease, second.lease);
    assert!(!Repo::start(&mut tx, &first, now + 1000)
        .await
        .expect("stale token"));
    assert!(Repo::start(&mut tx, &second, now + 1000)
        .await
        .expect("new token"));
    tx.commit().await.expect("retry commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let mut tx = fixture.uow.begin().await.expect("revoke");
    Repo::sync_configured(
        &mut tx,
        &[configured_observer(2, 'b')],
        crate::time::now_ms(),
    )
    .await
    .expect("revoke started");
    assert!(!Repo::finish(
        &mut tx,
        &second,
        &RoomObservationOutcome::NotInvoked,
        now + 1000
    )
    .await
    .expect("stale callback"));
    tx.commit().await.expect("revoke commit");
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'stale' AND body = ''")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn expired_started_work_retries_with_fenced_results_sqlite() {
    expired_started_work_retries_with_fenced_results(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn expired_started_work_retries_with_fenced_results_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_start").await {
        expired_started_work_retries_with_fenced_results(fixture).await;
    }
}
#[tokio::test]
async fn not_invoked_retries_before_ambiguous_lease_expiry_sqlite() {
    not_invoked_retries_before_ambiguous_lease_expiry(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn not_invoked_retries_before_ambiguous_lease_expiry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_not_invoked").await {
        not_invoked_retries_before_ambiguous_lease_expiry(fixture).await;
    }
}

/// Remove later schema and ledger entries so callers can model pre-V1022 state.
/// Both must agree before the normal runner upgrades the fixture again.
async fn revert_after_v1022(fixture: &IngressFixture) {
    revert_v1025(fixture).await;
    fixture
        .execute(
            "ALTER TABLE ingress_effect_receipts DROP COLUMN policy_discard_reason",
            (),
        )
        .await;
    fixture
        .execute("DELETE FROM _migrations WHERE version = 1024", ())
        .await;
    for index in [
        "extension_room_observation_work_settled",
        "extension_room_observation_work_source",
        "extension_room_publications_settled",
        "extension_room_publications_source",
        "extension_room_observation_receipts_recorded",
        "extension_room_sources_retracted_captured",
        "extension_room_source_revisions_source",
        "extension_room_observation_work_active_guard",
    ] {
        fixture
            .execute(&format!("DROP INDEX IF EXISTS {index}"), ())
            .await;
    }
    for (table, column) in [
        ("extension_room_observation_work", "settled_at_ms"),
        ("extension_room_publications", "settled_at_ms"),
        ("extension_room_observation_receipts", "recorded_at_ms"),
        ("extension_room_sources", "captured_at_ms"),
    ] {
        fixture
            .execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), ())
            .await;
    }
    fixture
        .execute("DELETE FROM _migrations WHERE version = 1023", ())
        .await;
}

async fn startup_upgrades_existing_work_and_retraction_fences_start(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    // Model a database created before node ownership columns were introduced.
    fixture
        .execute(
            "ALTER TABLE extension_room_observation_work DROP COLUMN lease_node_id",
            (),
        )
        .await;
    fixture
        .execute(
            "ALTER TABLE extension_room_observation_work DROP COLUMN lease_node_incarnation",
            (),
        )
        .await;
    fixture
        .execute(
            "ALTER TABLE ingress_send_attempts DROP COLUMN recovered",
            (),
        )
        .await;
    revert_after_v1022(&fixture).await;
    fixture
        .execute("DELETE FROM _migrations WHERE version = 1022", ())
        .await;
    assert_eq!(
        crate::db::MigrationRunner::single()
            .run(&fixture.db)
            .await
            .expect("upgrade"),
        vec![1022, 1023, 1024, 1025]
    );
    initialize_room_observations(&fixture.db)
        .await
        .expect("upgrade");
    initialize_room_observations(&fixture.db)
        .await
        .expect("idempotent startup");
    assert_eq!(fixture.count("extension_room_observation_work").await, 1);
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    let mut tx = fixture.uow.begin().await.expect("retract");
    Repo::retract(
        &mut tx,
        &room(),
        &StanzaId::new("stanza", room().into()),
        crate::time::now_ms(),
    )
    .await
    .expect("retract");
    assert!(!Repo::start(&mut tx, &work, now)
        .await
        .expect("retracted lease"));
    tx.commit().await.expect("retract commit");
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'stale' AND body = ''")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn startup_upgrades_existing_work_and_retraction_fences_start_sqlite() {
    startup_upgrades_existing_work_and_retraction_fences_start(IngressFixture::sqlite().await)
        .await;
}
#[tokio::test]
async fn startup_upgrades_existing_work_and_retraction_fences_start_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_upgrade").await {
        startup_upgrades_existing_work_and_retraction_fences_start(fixture).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn node_rotation_fences_observation_reservations_and_results() {
    use crate::ingress_uow::IngressFencing;
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

    let mut fixture = IngressFixture::sqlite().await;
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let identity = SharedNodeIdentity::new(NodeIdentity::new("node", "first"));
    fixture.uow.fencing = IngressFencing::Clustered(identity.clone());
    let mut tx = fixture.uow.begin().await.expect("claim");
    let first = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    assert_eq!(fixture.count("extension_room_observation_work WHERE lease_node_id = 'node' AND lease_node_incarnation = 'first'").await, 1);
    identity.rotate(NodeIdentity::new("node", "second")).await;
    let mut tx = fixture.uow.begin().await.expect("stale start");
    assert!(!Repo::start(&mut tx, &first, now).await.expect("old owner"));
    tx.commit().await.expect("stale commit");
    let mut tx = fixture.uow.begin().await.expect("reclaim");
    let second = Repo::claim(&mut tx, &subscription, now + 180_001)
        .await
        .expect("reclaim")
        .expect("work");
    // Queue rotation behind the claim's retained guard. Starting in this
    // transaction must reuse that guard, avoiding a recursive-reader deadlock.
    let rotation = identity.rotate(NodeIdentity::new("node", "third"));
    tokio::pin!(rotation);
    assert!(futures::poll!(&mut rotation).is_pending());
    assert!(Repo::start(&mut tx, &second, now + 180_001)
        .await
        .expect("current owner"));
    tx.commit().await.expect("start commit");
    rotation.await;
    let mut tx = fixture.uow.begin().await.expect("stale completion");
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(!Repo::finish(&mut tx, &second, &result, now + 180_002)
        .await
        .expect("rotated owner"));
    assert!(Repo::claim(&mut tx, &subscription, now + 1_000_000)
        .await
        .expect("started retry after rotation")
        .is_some());
    tx.commit().await.expect("stale commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn legacy_upgrade_bounds_ambiguity_once(fixture: IngressFixture) {
    let (observer, _) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut cases = Vec::new();
    for (index, (status, attempt, expiry, expected)) in [
        ("leased", 0_i64, Some(0_i64), "started"),
        ("leased", 1, None, "started"),
        ("pending", 1, None, "started"),
        ("started", 1, None, "started"),
        ("terminal", 1, Some(0), "terminal"),
        ("completed", 1, Some(0), "completed"),
        ("stale", 1, Some(0), "stale"),
    ]
    .into_iter()
    .enumerate()
    {
        let key = MessageKey::new();
        let mut tx = fixture.uow.begin().await.expect("legacy capture");
        record_message(&mut tx, key).await;
        capture(
            &mut tx,
            key,
            &room(),
            &message(
                &format!("legacy-wire-{index}"),
                &format!("legacy-stanza-{index}"),
                Some(&format!("legacy-origin-{index}")),
                "legacy evidence",
            ),
            &sender(),
            &[intent(&observer)],
            Utc::now(),
        )
        .await
        .expect("legacy capture");
        tx.commit().await.expect("legacy commit");
        fixture.execute("UPDATE extension_room_observation_work SET status = ?, attempt = ?, lease_id = 'legacy-token', lease_until_ms = ?, terminal_category = 'legacy_original' WHERE message_key = ?", crate::db_params![status, attempt, expiry, key.to_storage().to_string()]).await;
        cases.push((key, status, expiry, expected));
    }
    fixture
        .execute(
            "ALTER TABLE extension_room_observation_work DROP COLUMN lease_node_id",
            (),
        )
        .await;
    fixture
        .execute(
            "ALTER TABLE extension_room_observation_work DROP COLUMN lease_node_incarnation",
            (),
        )
        .await;
    fixture
        .execute(
            "ALTER TABLE ingress_send_attempts DROP COLUMN recovered",
            (),
        )
        .await;
    revert_after_v1022(&fixture).await;
    fixture
        .execute("DELETE FROM _migrations WHERE version = 1022", ())
        .await;
    let before = Utc::now().timestamp_millis();
    assert_eq!(
        crate::db::MigrationRunner::single()
            .run(&fixture.db)
            .await
            .expect("legacy upgrade"),
        vec![1022, 1023, 1024, 1025]
    );
    let after = Utc::now().timestamp_millis();
    assert!(crate::db::MigrationRunner::single()
        .run(&fixture.db)
        .await
        .expect("idempotent migration")
        .is_empty());
    initialize_room_observations(&fixture.db)
        .await
        .expect("second startup");
    for (key, previous, expiry, expected) in cases {
        let category = if matches!(previous, "leased" | "pending" | "started") {
            "legacy_unknown_attempt"
        } else {
            "legacy_original"
        };
        let conn = fixture.db.guard().await.expect("read work");
        let mut rows = conn.query("SELECT status, terminal_category, body, lease_id, lease_until_ms FROM extension_room_observation_work WHERE message_key = ?", crate::db_params![key.to_storage().to_string()]).await.expect("work");
        let row = rows.next().await.expect("row").expect("work");
        assert_eq!(row.get::<String>(0).expect("status"), expected);
        assert_eq!(row.get::<String>(1).expect("category"), category);
        assert_eq!(row.get::<String>(2).expect("body"), "legacy evidence");
        assert_eq!(row.get::<String>(3).expect("token"), "legacy-token");
        let actual = row.get::<i64>(4).expect("bounded expiry");
        if let Some(expiry) = expiry {
            assert_eq!(actual, expiry);
        } else {
            assert!((before + 180_000..=after + 180_000).contains(&actual));
        }
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        0
    );
    assert_eq!(fixture.count("extension_room_publications").await, 0);

    // No startup outside the ledger may quarantine legacy-looking work.
    fixture.execute("UPDATE extension_room_observation_work SET status = 'leased', lease_until_ms = ? WHERE body = 'body'", crate::db_params![after + 1_000_000]).await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("later startup");
    assert_eq!(fixture.count("extension_room_observation_work WHERE body = 'body' AND status = 'leased' AND lease_node_id IS NULL").await, 1);

    let mut tx = fixture.uow.begin().await.expect("recover legacy");
    let retry_at = after + 180_001;
    let retry = Repo::claim(&mut tx, &subscription, retry_at)
        .await
        .expect("claim")
        .expect("legacy work");
    assert_ne!(retry.lease.to_string(), "legacy-token");
    assert_eq!(retry.body.as_str(), "legacy evidence");
    assert!(Repo::start(&mut tx, &retry, retry_at)
        .await
        .expect("start retry"));
    tx.commit().await.expect("retry commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn legacy_upgrade_bounds_ambiguity_once_sqlite() {
    legacy_upgrade_bounds_ambiguity_once(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn legacy_upgrade_bounds_ambiguity_once_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_legacy_attempts").await {
        legacy_upgrade_bounds_ambiguity_once(fixture).await;
    }
}

async fn unknown_started_outcome_remains_retryable_after_twenty_attempts(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut last = None;
    for index in 0..20 {
        let at = now + index * 180_000;
        let mut tx = fixture.uow.begin().await.expect("attempt");
        let work = Repo::claim(&mut tx, &subscription, at)
            .await
            .expect("claim")
            .expect("work");
        assert_eq!(i64::from(work.attempt), index + 1);
        assert!(Repo::start(&mut tx, &work, at).await.expect("start"));
        tx.commit().await.expect("start commit");
        last = Some(work);
    }
    let mut tx = fixture.uow.begin().await.expect("unknown recovery");
    let retry = Repo::claim(&mut tx, &subscription, now + 20 * 180_000)
        .await
        .expect("unknown remains retryable")
        .expect("twenty-first attempt");
    assert_eq!(retry.attempt, 21);
    let last = last.expect("last work");
    assert_ne!(retry.lease, last.lease);
    assert_eq!(retry.message_key, last.message_key);
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(!Repo::finish(&mut tx, &last, &result, now + 20 * 180_000)
        .await
        .expect("late result"));
    assert!(Repo::start(&mut tx, &retry, now + 20 * 180_000)
        .await
        .expect("retry start"));
    // A later pre-invocation refusal cannot erase an earlier unknown send.
    assert!(Repo::finish(
        &mut tx,
        &retry,
        &RoomObservationOutcome::NotInvoked,
        now + 20 * 180_000
    )
    .await
    .expect("known noninvocation retains earlier uncertainty"));
    assert!(
        Repo::claim(&mut tx, &subscription, now + 20 * 180_000 + 60_000)
            .await
            .expect("retry after backoff")
            .is_some()
    );
    tx.commit().await.expect("retry commit");
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'terminal'")
            .await,
        0
    );
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        0
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn unknown_started_outcome_remains_retryable_after_twenty_attempts_sqlite() {
    unknown_started_outcome_remains_retryable_after_twenty_attempts(IngressFixture::sqlite().await)
        .await;
}
#[tokio::test]
async fn unknown_started_outcome_remains_retryable_after_twenty_attempts_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_exhausted").await {
        unknown_started_outcome_remains_retryable_after_twenty_attempts(fixture).await;
    }
}

#[tokio::test]
async fn postgres_observer_upgrade_preserves_owned_work() {
    let Some(fixture) = IngressFixture::postgres("observation_owned_upgrade").await else {
        return;
    };
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    assert!(Repo::start(&mut tx, &work, now).await.expect("start"));
    tx.commit().await.expect("start commit");
    // Existing owner evidence is never rewritten, even if schema columns
    // already exist when the one-time PostgreSQL migration runs.
    fixture
        .execute(
            "ALTER TABLE ingress_send_attempts DROP COLUMN recovered",
            (),
        )
        .await;
    revert_after_v1022(&fixture).await;
    fixture
        .execute("DELETE FROM _migrations WHERE version = 1022", ())
        .await;
    assert_eq!(
        crate::db::MigrationRunner::single()
            .run(&fixture.db)
            .await
            .expect("upgrade"),
        vec![1022, 1023, 1024, 1025]
    );
    assert_eq!(fixture.count("extension_room_observation_work WHERE status = 'started' AND lease_node_id IS NOT NULL AND lease_node_incarnation IS NOT NULL AND terminal_category IS NULL").await, 1);
    let mut tx = fixture.uow.begin().await.expect("finish");
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(Repo::finish(&mut tx, &work, &result, now + 1)
        .await
        .expect("unchanged lease"));
    tx.commit().await.expect("finish commit");
    assert_eq!(fixture.count("extension_room_publications").await, 1);
    fixture.close().await;
}

async fn started_delivery_authority_expires_before_reclaim(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    assert!(!Repo::validate_started(&mut tx, &work, now)
        .await
        .expect("not yet started"));
    assert!(Repo::start(&mut tx, &work, now).await.expect("start"));
    assert!(Repo::validate_started(&mut tx, &work, now)
        .await
        .expect("current authority"));
    assert!(!Repo::validate_started(&mut tx, &work, now + 180_000)
        .await
        .expect("expired authority"));
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    assert!(!Repo::finish(&mut tx, &work, &result, now + 180_000)
        .await
        .expect("late result before reclaim"));
    tx.commit().await.expect("commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn started_delivery_authority_expires_before_reclaim_sqlite() {
    started_delivery_authority_expires_before_reclaim(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn started_delivery_authority_expires_before_reclaim_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_capability_expiry").await {
        started_delivery_authority_expires_before_reclaim(fixture).await;
    }
}

#[tokio::test]
async fn postgres_observer_parent_contention_is_prompt_and_retryable() {
    let Some(fixture) = IngressFixture::postgres("observation_parent_nowait").await else {
        return;
    };
    let (observer, now) = seed(&fixture).await;
    let mut tx = fixture.uow.begin().await.expect("start");
    let work = Repo::claim(&mut tx, &subscription(&observer), now)
        .await
        .expect("claim")
        .expect("work");
    assert!(Repo::start(&mut tx, &work, now).await.expect("start"));
    tx.commit().await.expect("start commit");
    let mut blocker = fixture.db.begin().await.expect("parent holder");
    let mut rows = blocker
        .query(
            "SELECT message_key FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE",
            crate::db_params![work.message_key.to_storage().to_string()],
        )
        .await
        .expect("hold parent");
    assert!(rows.next().await.expect("row").is_some());
    drop(rows);
    let mut tx = fixture.uow.begin().await.expect("contender");
    let error = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        Repo::validate_started(&mut tx, &work, now),
    )
    .await
    .expect("canonical NOWAIT must not wait for inherited observer locks")
    .expect_err("parent held");
    assert_eq!(
        error,
        ObservationError::RetryableDatabase(
            crate::ingress_uow::DbRetryClass::CanonicalLockContention
        )
    );
    assert_eq!(
        crate::ingress_uow::IngressUowError::from(error).retry_class(),
        crate::ingress_uow::DbRetryClass::CanonicalLockContention
    );
    drop(tx);
    blocker.commit().await.expect("release parent");
    let mut tx = fixture.uow.begin().await.expect("fresh retry");
    assert!(Repo::validate_started(&mut tx, &work, now)
        .await
        .expect("current after contention"));
    tx.commit().await.expect("retry commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}
