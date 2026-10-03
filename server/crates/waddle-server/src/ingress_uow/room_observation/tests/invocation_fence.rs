use super::*;
use waddle_extensions::ObservationFailure;

async fn seed(fixture: &IngressFixture) -> (ConfiguredRoomObserver, i64) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let now = Utc::now();
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("seed");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("config");
    capture(
        &mut tx,
        key,
        &room(),
        &message("wire", "stanza", Some("origin"), "body"),
        &sender(),
        &[intent(&observer)],
        now,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("seed commit");
    (observer, now.timestamp_millis())
}

async fn started_work_survives_expiry_without_replay(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, &subscription, now)
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("claim commit");
    let result = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![payload()],
        usage: None,
    });
    let mut tx = fixture.uow.begin().await.expect("start");
    assert!(!Repo::finish(&mut tx, &work, &result, now)
        .await
        .expect("must start first"));
    assert!(!Repo::start(&mut tx, &work, now + 180_000)
        .await
        .expect("expired reservation"));
    assert!(Repo::start(&mut tx, &work, now).await.expect("start"));
    assert!(!Repo::start(&mut tx, &work, now).await.expect("start once"));
    tx.commit().await.expect("start commit");

    // Model process death after committing start: fresh transactions must not
    // reacquire this callback, even after arbitrarily many lease intervals.
    let mut tx = fixture.uow.begin().await.expect("recovery");
    assert!(Repo::claim(&mut tx, &subscription, now + 1_000_000)
        .await
        .expect("claim")
        .is_none());
    assert!(
        Repo::due_rooms(&mut tx, &observer, None, now + 1_000_000, 10)
            .await
            .expect("due")
            .is_empty()
    );
    for failure in [
        ObservationFailure::TemporaryFailure,
        ObservationFailure::DeadlineExceeded,
        ObservationFailure::RuntimeFailure,
    ] {
        assert!(!Repo::finish(
            &mut tx,
            &work,
            &RoomObservationOutcome::UnresolvedFailure(failure),
            now + 1_000_000
        )
        .await
        .expect("ambiguous"));
    }
    tx.commit().await.expect("recovery commit");
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'started'")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("extension_room_publications").await, 0);

    // A positively observed result still belongs to the started token after
    // expiry; publication and receipt are rolled back together on failure.
    let mut tx = fixture.uow.begin().await.expect("finish rollback");
    assert!(Repo::finish(&mut tx, &work, &result, now + 1_000_000)
        .await
        .expect("finish"));
    drop(tx);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(Repo::finish(&mut tx, &work, &result, now + 1_000_001)
        .await
        .expect("finish"));
    assert!(!Repo::finish(&mut tx, &work, &result, now + 1_000_001)
        .await
        .expect("finish once"));
    tx.commit().await.expect("finish commit");
    assert_eq!(fixture.count("extension_room_publications").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

async fn not_invoked_is_the_only_retryable_outcome(fixture: IngressFixture) {
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
    Repo::sync_configured(&mut tx, &[configured_observer(2, 'b')])
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
async fn started_work_survives_expiry_without_replay_sqlite() {
    started_work_survives_expiry_without_replay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn started_work_survives_expiry_without_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_start").await {
        started_work_survives_expiry_without_replay(fixture).await;
    }
}
#[tokio::test]
async fn not_invoked_is_the_only_retryable_outcome_sqlite() {
    not_invoked_is_the_only_retryable_outcome(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn not_invoked_is_the_only_retryable_outcome_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_not_invoked").await {
        not_invoked_is_the_only_retryable_outcome(fixture).await;
    }
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
    Repo::retract(&mut tx, &room(), &StanzaId::new("stanza", room().into()))
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
        .expect("no started reclaim")
        .is_none());
    tx.commit().await.expect("stale commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn startup_quarantines_legacy_attempts_without_fabricating_receipts(fixture: IngressFixture) {
    let (observer, now) = seed(&fixture).await;
    let subscription = subscription(&observer);
    let mut cases = Vec::new();
    for (index, (status, attempt, expected)) in [
        ("leased", 0_i64, "started"),
        ("leased", 1, "started"),
        ("pending", 1, "started"),
        ("started", 1, "started"),
        ("terminal", 1, "terminal"),
        ("completed", 1, "completed"),
        ("stale", 1, "stale"),
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
        fixture.execute(
            "UPDATE extension_room_observation_work SET status = ?, attempt = ?, lease_id = 'legacy-token', lease_until_ms = 0, terminal_category = 'legacy_original' WHERE message_key = ?",
            crate::db_params![status, attempt, key.to_storage().to_string()],
        ).await;
        cases.push((key, status, expected));
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
    initialize_room_observations(&fixture.db)
        .await
        .expect("legacy upgrade");
    initialize_room_observations(&fixture.db)
        .await
        .expect("idempotent upgrade");
    for (key, before, expected) in cases {
        let category = if matches!(before, "leased" | "pending") {
            "legacy_unknown_attempt"
        } else {
            "legacy_original"
        };
        assert_eq!(fixture.count(&format!(
            "extension_room_observation_work WHERE message_key = '{}' AND status = '{expected}' AND terminal_category = '{category}' AND body = 'legacy evidence' AND lease_id = 'legacy-token' AND lease_until_ms = 0",
            key.to_storage()
        )).await, 1, "legacy {before} evidence must survive startup");
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        0
    );
    assert_eq!(fixture.count("extension_room_publications").await, 0);

    // Pristine pending work can still run. A new, proven NotInvoked retry
    // keeps ownership evidence and must survive every subsequent startup.
    let mut tx = fixture.uow.begin().await.expect("pristine claim");
    let work = Repo::claim(&mut tx, &subscription, now + 1_000_000)
        .await
        .expect("claim")
        .expect("pristine work");
    assert_eq!(work.attempt, 1);
    assert_eq!(work.body.as_str(), "body");
    assert!(Repo::start(&mut tx, &work, now + 1_000_000)
        .await
        .expect("start"));
    tx.commit().await.expect("start commit");
    let mut tx = fixture.uow.begin().await.expect("not invoked");
    assert!(Repo::finish(
        &mut tx,
        &work,
        &RoomObservationOutcome::NotInvoked,
        now + 1_000_000
    )
    .await
    .expect("retry"));
    tx.commit().await.expect("retry commit");
    initialize_room_observations(&fixture.db)
        .await
        .expect("new retry survives startup");
    let mut tx = fixture.uow.begin().await.expect("retry claim");
    let retry = Repo::claim(&mut tx, &subscription, now + 2_000_000)
        .await
        .expect("claim")
        .expect("proven retry");
    assert_eq!(retry.id, work.id);
    assert_eq!(retry.attempt, 2);
    assert!(Repo::start(&mut tx, &retry, now + 2_000_000)
        .await
        .expect("start retry"));
    assert!(Repo::claim(&mut tx, &subscription, now + 3_000_000)
        .await
        .expect("no legacy replay")
        .is_none());
    assert!(
        Repo::due_rooms(&mut tx, &observer, None, now + 3_000_000, 10)
            .await
            .expect("no legacy scheduling")
            .is_empty()
    );
    tx.commit().await.expect("retry start commit");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn startup_quarantines_legacy_attempts_without_fabricating_receipts_sqlite() {
    startup_quarantines_legacy_attempts_without_fabricating_receipts(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn startup_quarantines_legacy_attempts_without_fabricating_receipts_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observation_legacy_attempts").await {
        startup_quarantines_legacy_attempts_without_fabricating_receipts(fixture).await;
    }
}
