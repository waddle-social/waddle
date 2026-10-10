use super::*;
use crate::db::DatabaseDriver;
use crate::ingress_uow::room_observation::ObservationWork;

struct GenerationFixture {
    observer: ConfiguredRoomObserver,
    history: MessageKey,
    publication_root: MessageKey,
    publication_work: ObservationWork,
    active: ObservationWork,
    now: i64,
}

async fn capture_started(
    fixture: &IngressFixture,
    observer: &ConfiguredRoomObserver,
    stanza: &str,
    correction: Option<&str>,
    now: chrono::DateTime<Utc>,
) -> ObservationWork {
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("capture");
    record_message(&mut tx, key).await;
    let mut source = message(stanza, stanza, Some(stanza), "question?");
    let frozen = if let Some(target) = correction {
        source.payloads.push(
            xmpp_parsers::message_correct::Replace {
                id: Id(target.to_string()),
            }
            .into(),
        );
        correction_intent(observer, target)
    } else {
        intent(observer)
    };
    capture(&mut tx, key, &room(), &source, &sender(), &[frozen], now)
        .await
        .expect("capture");
    let work = Repo::claim(&mut tx, &subscription(observer), now.timestamp_millis())
        .await
        .expect("claim")
        .expect("work");
    assert_eq!(work.message_key, key);
    assert!(Repo::start(&mut tx, &work, now.timestamp_millis())
        .await
        .expect("start"));
    tx.commit().await.expect("capture commit");
    work
}

async fn finish_completed(
    fixture: &IngressFixture,
    work: &ObservationWork,
    outputs: bool,
    now: i64,
) {
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(Repo::finish(
        &mut tx,
        work,
        &RoomObservationOutcome::Completed(RoomObservationResult {
            payloads: if outputs { vec![payload()] } else { vec![] },
            usage: None,
        }),
        now,
    )
    .await
    .expect("finish"));
    tx.commit().await.expect("finish commit");
}

async fn seed(fixture: &IngressFixture) -> GenerationFixture {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let now = Utc::now();
    let mut tx = fixture.uow.begin().await.expect("configure");
    Repo::sync_configured(
        &mut tx,
        std::slice::from_ref(&observer),
        now.timestamp_millis(),
    )
    .await
    .expect("configure");
    tx.commit().await.expect("configure commit");

    let history = capture_started(fixture, &observer, "history", None, now).await;
    finish_completed(fixture, &history, false, now.timestamp_millis()).await;
    let root = capture_started(fixture, &observer, "publication-root", None, now).await;
    let publication_work = capture_started(
        fixture,
        &observer,
        "publication-edit",
        Some("publication-root"),
        now,
    )
    .await;
    finish_completed(fixture, &publication_work, true, now.timestamp_millis()).await;
    let active = capture_started(fixture, &observer, "active", None, now).await;
    GenerationFixture {
        observer,
        history: history.message_key,
        publication_root: root.message_key,
        publication_work,
        active,
        now: now.timestamp_millis(),
    }
}

async fn assert_unsettled(fixture: &IngressFixture) {
    assert_eq!(
        fixture
            .count("extension_room_observers WHERE generation = 1")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'started'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'completed'")
            .await,
        2
    );
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE status = 'pending'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_receipts WHERE category = 'generation_changed'")
            .await,
        0
    );
}

async fn generation_bump_skips_completed_history_and_settles_live_custody(fixture: IngressFixture) {
    let seeded = seed(&fixture).await;
    let newer = configured_observer(2, 'b');
    // Holding every historical row proves the bump neither locks nor writes it.
    let mut blocker = if fixture.db.driver() == DatabaseDriver::Postgres {
        let mut tx = fixture.db.begin().await.expect("history holder");
        for sql in [
            "SELECT message_key FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE",
            "SELECT source_key FROM extension_room_sources WHERE source_key = ? FOR UPDATE",
            "SELECT id FROM extension_room_observation_work WHERE source_key = ? FOR UPDATE",
        ] {
            let mut rows = tx
                .query(
                    sql,
                    crate::db_params![seeded.history.to_storage().to_string()],
                )
                .await
                .expect("hold history");
            assert!(rows.next().await.expect("history row").is_some());
        }
        Some(tx)
    } else {
        None
    };

    let mut tx = fixture.uow.begin().await.expect("rollback bump");
    Repo::sync_configured(&mut tx, std::slice::from_ref(&newer), seeded.now + 1)
        .await
        .expect("completed history must not block generation change");
    drop(tx);
    assert_unsettled(&fixture).await;

    let mut tx = fixture.uow.begin().await.expect("committed bump");
    Repo::sync_configured(&mut tx, &[newer], seeded.now + 1)
        .await
        .expect("advance");
    tx.commit().await.expect("bump commit");
    assert_eq!(
        fixture
            .count("extension_room_observers WHERE generation = 2")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'completed'")
            .await,
        2
    );
    assert_eq!(fixture.count("extension_room_observation_work WHERE terminal_category = 'generation_changed' AND status = 'stale' AND lease_id IS NULL AND body = ''").await, 1);
    assert_eq!(
        fixture
            .count(
                "extension_room_publications WHERE status = 'stale' AND settled_at_ms IS NOT NULL"
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_receipts WHERE category = 'generation_changed'")
            .await,
        1
    );
    let mut tx = fixture.uow.begin().await.expect("old result fence");
    assert!(
        !Repo::validate_started(&mut tx, &seeded.active, seeded.now + 2)
            .await
            .expect("active fence")
    );
    assert!(
        Repo::publication(&mut tx, &subscription(&seeded.observer), seeded.now + 2)
            .await
            .expect("publication fence")
            .is_none()
    );
    tx.commit().await.expect("fence commit");
    if let Some(tx) = blocker.take() {
        tx.commit().await.expect("release history");
    }
    drop(blocker);
    fixture.close().await;
}

#[tokio::test]
async fn generation_bump_skips_completed_history_and_settles_live_custody_sqlite() {
    generation_bump_skips_completed_history_and_settles_live_custody(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn generation_bump_skips_completed_history_and_settles_live_custody_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_generation_history").await {
        generation_bump_skips_completed_history_and_settles_live_custody(fixture).await;
    }
}

#[tokio::test]
async fn postgres_generation_bump_fences_live_work_and_completed_publication_parents() {
    let Some(fixture) = IngressFixture::postgres("observer_generation_custody").await else {
        return;
    };
    let seeded = seed(&fixture).await;
    for key in [
        seeded.active.message_key,
        seeded.publication_root,
        seeded.publication_work.message_key,
    ] {
        let mut blocker = fixture.db.begin().await.expect("live parent holder");
        let mut rows = blocker
            .query(
                "SELECT message_key FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE",
                crate::db_params![key.to_storage().to_string()],
            )
            .await
            .expect("hold live parent");
        assert!(rows.next().await.expect("live parent").is_some());
        drop(rows);
        let mut tx = fixture.uow.begin().await.expect("contender");
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            Repo::sync_configured(&mut tx, &[configured_observer(2, 'b')], seeded.now + 1),
        )
        .await
        .expect("canonical prelocks must be prompt")
        .expect_err("live parent is held");
        assert_eq!(
            error,
            ObservationError::RetryableDatabase(
                crate::ingress_uow::DbRetryClass::CanonicalLockContention
            )
        );
        drop(tx);
        blocker.commit().await.expect("release live parent");
        assert_unsettled(&fixture).await;
    }
    fixture.close().await;
}
