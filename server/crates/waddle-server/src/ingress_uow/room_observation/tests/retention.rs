//! Bounded retention of settled observer history (#1901).

use super::*;
use crate::db::DatabaseDriver;
use crate::ingress::gc::RetentionGcBudget;
use crate::ingress::maintenance::{
    collect_observer_history, run_maintenance_pass, MaintenanceBudget, MaintenanceOutcome,
};
use crate::ingress::test_support::{runbook_sql_block, runbook_statements};
use chrono::{DateTime, Duration};
use uuid::Uuid;
use waddle_extensions::ObservationFailure;

use super::super::retention::OBSERVER_HISTORY_RETENTION;
use super::super::ObserverRetentionBatch;

const BATCH: u32 = 256;

fn ms(at: DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

/// First instant at which a row settled at `at` is past the horizon.
fn horizon(at: DateTime<Utc>) -> i64 {
    ms(at + OBSERVER_HISTORY_RETENTION)
}

async fn collect(fixture: &IngressFixture, now_ms: i64, limit: u32) -> ObserverRetentionBatch {
    let mut tx = fixture.db.begin_immediate().await.expect("retention tx");
    let batch = Repo::collect_expired(&mut tx, now_ms, limit)
        .await
        .expect("collect expired history");
    tx.commit().await.expect("retention commit");
    batch
}

async fn seed(
    fixture: &IngressFixture,
    observer: &ConfiguredRoomObserver,
    stanza: &str,
    at: DateTime<Utc>,
) -> MessageKey {
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("seed");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(observer), ms(at))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        &room(),
        &message(
            &format!("wire-{stanza}"),
            stanza,
            Some(&format!("origin-{stanza}")),
            "body",
        ),
        &sender(),
        &[intent(observer)],
        at,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("seed commit");
    key
}

async fn settle(
    fixture: &IngressFixture,
    subscription: &RoomObservationSubscription,
    at_ms: i64,
    outcome: RoomObservationOutcome,
) {
    let mut tx = fixture.uow.begin().await.expect("claim");
    let work = Repo::claim(&mut tx, subscription, at_ms)
        .await
        .expect("claim")
        .expect("due work");
    tx.commit().await.expect("lease");
    let mut tx = fixture.uow.begin().await.expect("start");
    assert!(Repo::start(&mut tx, &work, at_ms).await.expect("start"));
    tx.commit().await.expect("started");
    let mut tx = fixture.uow.begin().await.expect("finish");
    assert!(Repo::finish(&mut tx, &work, &outcome, at_ms)
        .await
        .expect("finish"));
    tx.commit().await.expect("finished");
}

fn completed(payloads: Vec<ExtensionPayload>) -> RoomObservationOutcome {
    RoomObservationOutcome::Completed(RoomObservationResult {
        payloads,
        usage: None,
    })
}

fn failed() -> RoomObservationOutcome {
    RoomObservationOutcome::PermanentFailure(ObservationFailure::Denied)
}

async fn retract(fixture: &IngressFixture, stanza: &str, at_ms: i64) {
    let mut tx = fixture.uow.begin().await.expect("retract");
    Repo::retract(
        &mut tx,
        &room(),
        &StanzaId::new(stanza, room().into()),
        at_ms,
    )
    .await
    .expect("retract");
    tx.commit().await.expect("retract commit");
}

async fn publish(fixture: &IngressFixture, subscription: &RoomObservationSubscription, at_ms: i64) {
    let mut tx = fixture.uow.begin().await.expect("publish");
    let publication = Repo::publication(&mut tx, subscription, at_ms)
        .await
        .expect("publication")
        .expect("pending publication");
    assert!(Repo::assert_publication(&mut tx, &publication)
        .await
        .expect("assert"));
    assert!(Repo::mark_published(&mut tx, &publication.id, at_ms)
        .await
        .expect("mark"));
    tx.commit().await.expect("publish commit");
}

async fn terminalize(fixture: &IngressFixture, key: MessageKey) {
    let mut tx = fixture.uow.begin().await.expect("terminalize");
    CanonicalMessageRepository::terminalize(&mut tx, key, Utc::now())
        .await
        .expect("terminalize canonical row");
    tx.commit().await.expect("terminal commit");
}

async fn insert_settled_work(fixture: &IngressFixture, count: usize, settled_at_ms: i64) {
    let mut tx = fixture.db.begin_immediate().await.expect("bulk work");
    for _ in 0..count {
        tx.execute(
            "INSERT INTO extension_room_observation_work (id, source_key, message_key, plugin_id, generation, identity, room_jid, revision, source_json, body, status, attempt, due_at_ms, terminal_category, settled_at_ms) VALUES (?, ?, ?, 'observer-fixture', 1, 'identity', 'room@conference.example.org', 0, '{}', '', 'completed', 1, ?, 'completed', ?)",
            crate::db_params![Uuid::now_v7().to_string(), Uuid::now_v7().to_string(), Uuid::now_v7().to_string(), settled_at_ms, settled_at_ms],
        )
        .await
        .expect("settled work row");
    }
    tx.commit().await.expect("bulk work commit");
}

async fn history_counts(fixture: &IngressFixture) -> [i64; 5] {
    [
        fixture.count("extension_room_publications").await,
        fixture.count("extension_room_observation_work").await,
        fixture.count("extension_room_observation_receipts").await,
        fixture.count("extension_room_source_revisions").await,
        fixture.count("extension_room_sources").await,
    ]
}

async fn active_work_is_never_collected(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    let mut keys = Vec::new();
    for stanza in ["active-1", "active-2", "active-3"] {
        keys.push(seed(&fixture, &observer, stanza, t0).await);
    }
    let mut tx = fixture.uow.begin().await.expect("lease two");
    let started = Repo::claim(&mut tx, &subscription, ms(t0))
        .await
        .expect("claim")
        .expect("work");
    assert!(Repo::start(&mut tx, &started, ms(t0)).await.expect("start"));
    Repo::claim(&mut tx, &subscription, ms(t0))
        .await
        .expect("claim")
        .expect("second work");
    tx.commit().await.expect("leases");
    assert_eq!(
        fixture
            .count(
                "extension_room_observation_work WHERE status IN ('pending', 'leased', 'started')"
            )
            .await,
        3
    );
    // Evidence for active work must survive even if old and its canonical
    // rows are already terminal.
    for key in &keys {
        fixture
            .execute(
                "INSERT INTO extension_room_observation_receipts (plugin_id, generation, room_jid, message_key, category, recorded_at_ms) VALUES ('observer-fixture', 1, ?, ?, 'legacy', ?)",
                crate::db_params![room().to_string(), key.to_storage().to_string(), ms(t0)],
            )
            .await;
        terminalize(&fixture, *key).await;
    }
    let before = history_counts(&fixture).await;
    // Far past the horizon and every lease expiry.
    let far = ms(t0 + OBSERVER_HISTORY_RETENTION + Duration::days(30));
    for _ in 0..2 {
        assert_eq!(collect(&fixture, far, BATCH).await.total(), 0);
    }
    assert_eq!(history_counts(&fixture).await, before);
    assert_eq!(before[1], 3);
    assert_eq!(before[2], 3);
    assert_eq!(before[4], 3);
    fixture.close().await;
}

#[tokio::test]
async fn active_work_is_never_collected_sqlite() {
    active_work_is_never_collected(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn active_work_is_never_collected_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_active").await {
        active_work_is_never_collected(fixture).await;
    }
}

async fn pending_publication_protects_work_and_source(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    let key = seed(&fixture, &observer, "published", t0).await;
    settle(&fixture, &subscription, ms(t0), completed(vec![payload()])).await;
    terminalize(&fixture, key).await;

    let far = ms(t0 + OBSERVER_HISTORY_RETENTION + Duration::days(30));
    let batch = collect(&fixture, far, BATCH).await;
    assert_eq!(
        (
            batch.publications,
            batch.work,
            batch.receipts,
            batch.sources
        ),
        (0, 0, 1, 0),
        "only the settlement receipt is collectable while the result is unpublished"
    );
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE status = 'pending'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'completed'")
            .await,
        1
    );

    // Publishing settles the publication. Its source only becomes
    // collectable once retracted: live identity must keep resolving
    // corrections for as long as the archive holds the message.
    let t1 = t0 + Duration::days(1);
    publish(&fixture, &subscription, ms(t1)).await;
    retract(&fixture, "published", ms(t1)).await;
    // The work settled at t0 goes once its publication is no longer
    // pending; the publication and the source it references wait for t1's
    // horizon.
    let batch = collect(&fixture, horizon(t1) - 1, BATCH).await;
    assert_eq!(
        (
            batch.publications,
            batch.work,
            batch.revisions,
            batch.sources
        ),
        (0, 1, 0, 0)
    );
    let batch = collect(&fixture, horizon(t1), BATCH).await;
    assert_eq!(
        (
            batch.publications,
            batch.work,
            batch.revisions,
            batch.sources
        ),
        (1, 0, 1, 1)
    );
    assert_eq!(history_counts(&fixture).await, [0, 0, 0, 0, 0]);
    fixture.close().await;
}

#[tokio::test]
async fn pending_publication_protects_work_and_source_sqlite() {
    pending_publication_protects_work_and_source(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn pending_publication_protects_work_and_source_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_publication").await {
        pending_publication_protects_work_and_source(fixture).await;
    }
}

async fn nonterminal_canonical_row_protects_receipts_and_work(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    let key = seed(&fixture, &observer, "nonterminal", t0).await;
    settle(&fixture, &subscription, ms(t0), failed()).await;
    let far = ms(t0 + OBSERVER_HISTORY_RETENTION + Duration::days(30));
    assert_eq!(
        collect(&fixture, far, BATCH).await.total(),
        0,
        "recovery may still rebuild the observer effect for a live canonical row"
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'terminal'")
            .await,
        1
    );
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        1
    );

    terminalize(&fixture, key).await;
    let batch = collect(&fixture, far, BATCH).await;
    assert_eq!((batch.work, batch.receipts), (1, 1));
    assert_eq!(
        fixture.count("extension_room_sources").await,
        1,
        "a live source is never collected"
    );
    fixture.close().await;
}

#[tokio::test]
async fn nonterminal_canonical_row_protects_receipts_and_work_sqlite() {
    nonterminal_canonical_row_protects_receipts_and_work(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn nonterminal_canonical_row_protects_receipts_and_work_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_canonical").await {
        nonterminal_canonical_row_protects_receipts_and_work(fixture).await;
    }
}

async fn retention_expiry_collects_completed_terminal_and_stale_history(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    let mut keys = Vec::new();
    // Settled at t0: completed + published, terminal, and stale (retracted).
    keys.push(seed(&fixture, &observer, "completed", t0).await);
    settle(&fixture, &subscription, ms(t0), completed(vec![payload()])).await;
    publish(&fixture, &subscription, ms(t0)).await;
    keys.push(seed(&fixture, &observer, "terminal", t0).await);
    settle(&fixture, &subscription, ms(t0), failed()).await;
    keys.push(seed(&fixture, &observer, "stale", t0).await);
    retract(&fixture, "stale", ms(t0)).await;
    // Settled two days later: inside the horizon at the collection time.
    let late = t0 + Duration::days(2);
    keys.push(seed(&fixture, &observer, "late", late).await);
    settle(&fixture, &subscription, ms(late), failed()).await;
    for key in &keys {
        terminalize(&fixture, *key).await;
    }
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE settled_at_ms IS NULL")
            .await,
        0,
        "every settled work row carries its settlement time"
    );
    assert_eq!(
        fixture
            .count("extension_room_publications WHERE settled_at_ms IS NULL")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_receipts WHERE recorded_at_ms = 0")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("extension_room_sources WHERE captured_at_ms = 0")
            .await,
        0
    );

    assert_eq!(collect(&fixture, horizon(t0) - 1, BATCH).await.total(), 0);
    let before = history_counts(&fixture).await;
    assert_eq!(
        collect_observer_history(
            &fixture.db,
            RetentionGcBudget::DEFAULT,
            horizon(t0) + 3_600_000
        )
        .await,
        MaintenanceOutcome::Complete
    );
    let after = history_counts(&fixture).await;
    let deleted: Vec<i64> = before.iter().zip(after).map(|(b, a)| b - a).collect();
    assert_eq!(deleted, vec![1, 3, 3, 1, 1]);
    assert_eq!(
        fixture
            .count(&format!(
                "extension_room_observation_work WHERE settled_at_ms = {}",
                ms(late)
            ))
            .await,
        1,
        "history settled after the cutoff survives"
    );
    assert_eq!(after[2], 1);
    assert_eq!(after[4], 3, "live sources are never collected");
    let reclaimed = |table: &str| {
        metrics.counter_sum(
            "ingress.maintenance.reclaimed_observer_rows",
            &[("table", table)],
        )
    };
    assert_eq!(reclaimed("publication"), Some(1));
    assert_eq!(reclaimed("work"), Some(3));
    assert_eq!(reclaimed("receipt"), Some(3));
    assert_eq!(reclaimed("source"), Some(2));
    fixture.close().await;
}

#[tokio::test]
async fn retention_expiry_collects_completed_terminal_and_stale_history_sqlite() {
    retention_expiry_collects_completed_terminal_and_stale_history(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn retention_expiry_collects_completed_terminal_and_stale_history_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_expiry").await {
        retention_expiry_collects_completed_terminal_and_stale_history(fixture).await;
    }
}

async fn repeat_collection_is_idempotent_and_bounded(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let t0 = Utc::now();
    insert_settled_work(&fixture, 300, ms(t0)).await;
    let first = collect(&fixture, horizon(t0), BATCH).await;
    assert_eq!(
        (first.work, first.total(), first.exhausted),
        (256, 256, true)
    );
    let second = collect(&fixture, horizon(t0), BATCH).await;
    assert_eq!((second.work, second.exhausted), (44, false));
    let third = collect(&fixture, horizon(t0), BATCH).await;
    assert_eq!(third, ObserverRetentionBatch::default());

    // The maintenance driver reports a budget-cut pass as partial and
    // finishes the backlog on its continuation.
    insert_settled_work(&fixture, 300, ms(t0)).await;
    let exhausted_budget = RetentionGcBudget {
        cooperative: std::time::Duration::ZERO,
        ..RetentionGcBudget::DEFAULT
    };
    assert_eq!(
        collect_observer_history(&fixture.db, exhausted_budget, horizon(t0)).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(fixture.count("extension_room_observation_work").await, 44);
    for _ in 0..2 {
        assert_eq!(
            collect_observer_history(&fixture.db, RetentionGcBudget::DEFAULT, horizon(t0)).await,
            MaintenanceOutcome::Complete
        );
    }
    assert_eq!(fixture.count("extension_room_observation_work").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn repeat_collection_is_idempotent_and_bounded_sqlite() {
    repeat_collection_is_idempotent_and_bounded(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn repeat_collection_is_idempotent_and_bounded_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_bounded").await {
        repeat_collection_is_idempotent_and_bounded(fixture).await;
    }
}

async fn retracted_source_with_oversized_revision_chain_drains_within_budget(
    fixture: IngressFixture,
) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let t0 = Utc::now();
    let key = seed(&fixture, &observer, "chain-root", t0).await;
    retract(&fixture, "chain-root", ms(t0)).await;
    terminalize(&fixture, key).await;
    let source_key = key.to_storage().to_string();
    let mut tx = fixture.db.begin_immediate().await.expect("chain");
    for index in 0..300 {
        tx.execute(
            "INSERT INTO extension_room_source_revisions (room_jid, room_stanza_id, source_key) VALUES (?, ?, ?)",
            crate::db_params![room().to_string(), format!("chain-edit-{index}"), &source_key],
        )
        .await
        .expect("revision mapping");
    }
    tx.commit().await.expect("chain commit");
    assert_eq!(fixture.count("extension_room_source_revisions").await, 301);

    let now = horizon(t0);
    let first = collect(&fixture, now, BATCH).await;
    assert_eq!(
        (first.work, first.receipts, first.revisions, first.sources),
        (1, 1, 254, 0)
    );
    assert!(first.exhausted);
    assert_eq!(fixture.count("extension_room_sources").await, 1);

    // A row that references the source mid-drain stops the drain: neither
    // its remaining mappings nor the source row are touched.
    fixture
        .execute(
            "INSERT INTO extension_room_observation_work (id, source_key, message_key, plugin_id, generation, identity, room_jid, revision, source_json, body, status, attempt, due_at_ms) VALUES ('rereference', ?, ?, 'observer-fixture', 1, 'identity', ?, 1, '{}', 'body', 'pending', 0, ?)",
            crate::db_params![&source_key, Uuid::now_v7().to_string(), room().to_string(), ms(t0)],
        )
        .await;
    assert_eq!(collect(&fixture, now, BATCH).await.total(), 0);
    assert_eq!(fixture.count("extension_room_source_revisions").await, 47);
    assert_eq!(fixture.count("extension_room_sources").await, 1);
    fixture
        .execute(
            "DELETE FROM extension_room_observation_work WHERE id = 'rereference'",
            (),
        )
        .await;

    // A correction aimed at the retracted chain never resurrects it.
    let edit_key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("late correction");
    record_message(&mut tx, edit_key).await;
    let mut edit = message(
        "wire-chain-late",
        "chain-late",
        Some("wire-chain-late"),
        "edit",
    );
    edit.payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-chain-root",
        ));
    capture(
        &mut tx,
        edit_key,
        &room(),
        &edit,
        &sender(),
        &[correction_intent(&observer, "chain-edit-299")],
        t0,
    )
    .await
    .expect("correction of retracted source");
    tx.commit().await.expect("correction commit");
    terminalize(&fixture, edit_key).await;
    assert_eq!(
        fixture.count("extension_room_observation_work").await,
        0,
        "a retracted source schedules no work"
    );

    let mut batches = Vec::new();
    loop {
        let batch = collect(&fixture, now, BATCH).await;
        assert!(batch.total() <= u64::from(BATCH));
        if batch.sources == 0 {
            assert_eq!(fixture.count("extension_room_sources").await, 1);
        }
        batches.push(batch);
        if !batch.exhausted {
            break;
        }
    }
    let drained: u64 = batches.iter().map(|batch| batch.revisions).sum();
    assert_eq!(drained, 47);
    assert_eq!(batches.iter().map(|batch| batch.sources).sum::<u64>(), 1);
    assert_eq!(history_counts(&fixture).await, [0, 0, 0, 0, 0]);

    if fixture.db.driver() == DatabaseDriver::Postgres {
        // A writer holding the source lock (capture, correction, claim and
        // finish all lock it before referencing it) makes GC skip the row.
        let key = seed(&fixture, &observer, "locked-root", t0).await;
        retract(&fixture, "locked-root", ms(t0)).await;
        terminalize(&fixture, key).await;
        let mut holder = fixture.db.begin().await.expect("lock holder");
        holder
            .query(
                "SELECT source_key FROM extension_room_sources WHERE source_key = ? FOR UPDATE",
                crate::db_params![key.to_storage().to_string()],
            )
            .await
            .expect("hold source lock");
        let locked = collect(&fixture, now, BATCH).await;
        assert_eq!((locked.revisions, locked.sources), (1, 0));
        assert_eq!(fixture.count("extension_room_sources").await, 1);
        holder.rollback().await.expect("release source lock");
        assert_eq!(collect(&fixture, now, BATCH).await.sources, 1);
        assert_eq!(fixture.count("extension_room_sources").await, 0);
    }
    fixture.close().await;
}

async fn full_batch_of_retracted_sources_is_collected_in_one_batch(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let t0 = Utc::now();
    // Earlier batches spent their budget on revision mappings, leaving a full
    // budget of retracted sources with nothing left to drain.
    let mut tx = fixture.db.begin_immediate().await.expect("sources");
    for index in 0..BATCH {
        tx.execute(
            "INSERT INTO extension_room_sources (source_key, room_jid, sender_jid, root_stanza_id, revision_stanza_id, root_origin_id, revision, source_json, retracted, captured_at_ms) VALUES (?, ?, 'author@example.org', ?, ?, NULL, 0, '{}', 1, ?)",
            crate::db_params![Uuid::now_v7().to_string(), room().to_string(), format!("retracted-{index}"), format!("retracted-{index}"), ms(t0)],
        )
        .await
        .expect("retracted source");
    }
    tx.commit().await.expect("sources commit");
    let batch = collect(&fixture, horizon(t0), BATCH).await;
    assert_eq!(
        batch,
        ObserverRetentionBatch {
            sources: u64::from(BATCH),
            exhausted: true,
            ..ObserverRetentionBatch::default()
        }
    );
    assert_eq!(fixture.count("extension_room_sources").await, 0);
    assert_eq!(collect(&fixture, horizon(t0), BATCH).await.total(), 0);
    fixture.close().await;
}

#[tokio::test]
async fn full_batch_of_retracted_sources_is_collected_in_one_batch_sqlite() {
    full_batch_of_retracted_sources_is_collected_in_one_batch(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn full_batch_of_retracted_sources_is_collected_in_one_batch_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_source_batch").await {
        full_batch_of_retracted_sources_is_collected_in_one_batch(fixture).await;
    }
}

#[tokio::test]
async fn retracted_source_with_oversized_revision_chain_drains_within_budget_sqlite() {
    retracted_source_with_oversized_revision_chain_drains_within_budget(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn retracted_source_with_oversized_revision_chain_drains_within_budget_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_chain").await {
        retracted_source_with_oversized_revision_chain_drains_within_budget(fixture).await;
    }
}

async fn aged_nonretracted_source_still_resolves_a_valid_correction(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    let key = seed(&fixture, &observer, "aged-root", t0).await;
    settle(&fixture, &subscription, ms(t0), completed(Vec::new())).await;
    terminalize(&fixture, key).await;
    let later = t0 + OBSERVER_HISTORY_RETENTION + Duration::days(30);
    let batch = collect(&fixture, ms(later), BATCH).await;
    assert_eq!((batch.work, batch.receipts, batch.sources), (1, 1, 0));
    assert_eq!(fixture.count("extension_room_sources").await, 1);
    assert_eq!(fixture.count("extension_room_source_revisions").await, 1);

    let edit_key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("aged correction");
    record_message(&mut tx, edit_key).await;
    let mut edit = message(
        "wire-aged-edit",
        "aged-edit",
        Some("wire-aged-edit"),
        "edit",
    );
    edit.payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element(
            "wire-aged-root",
        ));
    capture(
        &mut tx,
        edit_key,
        &room(),
        &edit,
        &sender(),
        &[correction_intent(&observer, "aged-root")],
        later,
    )
    .await
    .expect("correction of retained source");
    tx.commit().await.expect("correction commit");
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'pending' AND revision = 1")
            .await,
        1,
        "the retained source identity still accepts the correction"
    );
    assert_eq!(
        fixture
            .count(
                "extension_room_observation_receipts WHERE category = 'unknown_correction_target'"
            )
            .await,
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn aged_nonretracted_source_still_resolves_a_valid_correction_sqlite() {
    aged_nonretracted_source_still_resolves_a_valid_correction(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn aged_nonretracted_source_still_resolves_a_valid_correction_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_aged_source").await {
        aged_nonretracted_source_still_resolves_a_valid_correction(fixture).await;
    }
}

/// The write half of the operator disposition runbook (#1901,
/// `ingress-authority.md`), applied to a reviewed manifest of one Class A
/// pending work row and one Class B stranded publication.
async fn apply_disposition(
    fixture: &IngressFixture,
    work_ids: &[String],
    publication_ids: &[String],
    unsupported: MessageKey,
    at_ms: i64,
) {
    let postgres = fixture.db.driver() == DatabaseDriver::Postgres;
    let key_param = if postgres { "CAST(? AS UUID)" } else { "?" };
    let mut tx = fixture.uow.begin().await.expect("disposition");
    let raw = tx.transaction_mut();
    for id in work_ids {
        assert_eq!(
            raw.execute(
                "UPDATE extension_room_observation_work SET status = 'terminal', terminal_category = 'operator_unsupported', body = '', lease_id = NULL, lease_until_ms = NULL, settled_at_ms = ? WHERE id = ? AND status = 'pending'",
                crate::db_params![at_ms, id],
            )
            .await
            .expect("terminal work"),
            1
        );
        raw.execute(
            "UPDATE extension_room_publications SET status = 'stale', settled_at_ms = ? WHERE work_id = ? AND status = 'pending'",
            crate::db_params![at_ms, id],
        )
        .await
        .expect("dependent publications");
        raw.execute(
            "INSERT INTO extension_room_observation_receipts (plugin_id, generation, room_jid, message_key, category, recorded_at_ms) SELECT plugin_id, generation, room_jid, message_key, 'operator_unsupported', ? FROM extension_room_observation_work WHERE id = ? ON CONFLICT DO NOTHING",
            crate::db_params![at_ms, id],
        )
        .await
        .expect("observer receipt");
    }
    for id in publication_ids {
        assert_eq!(
            raw.execute(
                "UPDATE extension_room_publications SET status = 'stale', settled_at_ms = ? WHERE id = ? AND status = 'pending'",
                crate::db_params![at_ms, id],
            )
            .await
            .expect("stranded publication"),
            1
        );
    }
    let abandonment = format!(
        "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT intent.message_key, intent.kind, intent.semantic_identity_hash FROM ingress_effect_intents intent WHERE intent.message_key = {key_param} AND intent.kind = 28 AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts receipt WHERE receipt.message_key = intent.message_key AND receipt.kind = intent.kind AND receipt.semantic_identity_hash = intent.semantic_identity_hash) ON CONFLICT DO NOTHING"
    );
    assert_eq!(
        raw.execute(
            &abandonment,
            crate::db_params![unsupported.to_storage().to_string()],
        )
        .await
        .expect("abandonment receipt"),
        1
    );
    tx.commit().await.expect("disposition commit");
}

/// One Class A row of a reviewed disposition manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewedWork {
    id: String,
    status: String,
    attempt: i64,
    reason: String,
}

/// The reviewed output of the runbook's dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DispositionManifest {
    work: Vec<ReviewedWork>,
    publications: Vec<String>,
    /// `(work_id, message_key, hex semantic_identity_hash)`, kind 28.
    pairs: Vec<(String, String, String)>,
}

fn sql_room_list(rooms: &[BareJid]) -> String {
    if rooms.is_empty() {
        // A list that matches nothing selects only retired generations.
        return "''".to_string();
    }
    rooms
        .iter()
        .map(|room| format!("'{room}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// PostgreSQL: run the runbook's dry-run block verbatim on one connection,
/// inside its documented `REPEATABLE READ READ ONLY` transaction, with the
/// reviewed permanently-lost rooms substituted, and decode its manifests.
async fn documented_dry_run(pool: &sqlx::PgPool, lost_rooms: &[BareJid]) -> DispositionManifest {
    documented_dry_run_observed(pool, lost_rooms, std::future::ready(())).await
}

/// [`documented_dry_run`], running `after_first_manifest` on another
/// connection once the first manifest query has fixed the snapshot.
async fn documented_dry_run_observed(
    pool: &sqlx::PgPool,
    lost_rooms: &[BareJid],
    after_first_manifest: impl std::future::Future<Output = ()>,
) -> DispositionManifest {
    use sqlx::Row as _;
    let dry_run = runbook_sql_block("observer-disposition-dry-run")
        .replace("'<permanently-lost-room>'", &sql_room_list(lost_rooms));
    let statements = runbook_statements(&dry_run);
    assert_eq!(
        statements.first().map(String::as_str),
        Some("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY"),
        "the dry run reads one snapshot"
    );
    assert_eq!(statements.last().map(String::as_str), Some("COMMIT"));
    let mut conn = pool.acquire().await.expect("dry-run connection");
    let mut reviewed = Vec::new();
    let mut after_first_manifest = Some(after_first_manifest);
    for statement in &statements {
        if statement.starts_with("SELECT") {
            reviewed.push(
                sqlx::query(statement)
                    .fetch_all(&mut *conn)
                    .await
                    .unwrap_or_else(|error| panic!("dry run {statement}: {error}")),
            );
            if let Some(hook) = after_first_manifest.take() {
                hook.await;
            }
        } else {
            sqlx::raw_sql(statement)
                .execute(&mut *conn)
                .await
                .unwrap_or_else(|error| panic!("dry run {statement}: {error}"));
        }
    }
    assert_eq!(reviewed.len(), 3, "dry run has three manifest queries");
    DispositionManifest {
        work: reviewed[0]
            .iter()
            .map(|row| ReviewedWork {
                id: row.get("id"),
                status: row.get("status"),
                attempt: row.get("attempt"),
                reason: row.get("reason"),
            })
            .collect(),
        publications: reviewed[1].iter().map(|row| row.get("id")).collect(),
        pairs: reviewed[2]
            .iter()
            .map(|row| {
                assert_eq!(row.get::<i32, _>("kind"), 28);
                (
                    row.get("work_id"),
                    row.get::<Uuid, _>("message_key").to_string(),
                    row.get("semantic_identity_hash"),
                )
            })
            .collect(),
    }
}

/// Expand one single-row `VALUES` template into the reviewed rows, or drop
/// the whole `INSERT` when the reviewed list is empty, as the runbook says.
fn expand_values(sql: &str, header: &str, template: &str, rows: &[String]) -> String {
    let block = format!("{header}\n{template}\n");
    assert!(
        sql.contains(&block),
        "runbook keeps the template {template}"
    );
    if rows.is_empty() {
        return sql.replace(&block, "");
    }
    let rows: Vec<String> = rows.iter().map(|row| format!("  {row}")).collect();
    sql.replace(&block, &format!("{header}\n{};\n", rows.join(",\n")))
}

/// PostgreSQL: the runbook's write transaction with the manifest filled in.
async fn documented_write(pool: &sqlx::PgPool, manifest: &DispositionManifest) -> String {
    let epoch: String =
        sqlx::query_scalar("SELECT epoch::text FROM ingress_protocol_epoch WHERE id = 1")
            .fetch_one(pool)
            .await
            .expect("live epoch");
    let mut write = runbook_sql_block("observer-disposition-write")
        .replace("\\set ON_ERROR_STOP on", "")
        .replace("<current epoch>", &epoch);
    write = expand_values(
        &write,
        "INSERT INTO reviewed_work (id, status, attempt, reason) VALUES",
        "  ('<reviewed-work-id>', '<reviewed-status>', <reviewed-attempt>, '<generation_retired|room_lost>');",
        &manifest
            .work
            .iter()
            .map(|work| {
                format!(
                    "('{}', '{}', {}, '{}')",
                    work.id, work.status, work.attempt, work.reason
                )
            })
            .collect::<Vec<_>>(),
    );
    write = expand_values(
        &write,
        "INSERT INTO reviewed_publications (id) VALUES",
        "  ('<reviewed-publication-id>');",
        &manifest
            .publications
            .iter()
            .map(|id| format!("('{id}')"))
            .collect::<Vec<_>>(),
    );
    write = expand_values(
        &write,
        "INSERT INTO reviewed_pending (work_id, message_key, kind, semantic_identity_hash) VALUES",
        "  ('<reviewed-work-id>', '<reviewed-message-key>'::uuid, 28, decode('<reviewed-hex-hash>', 'hex'));",
        &manifest
            .pairs
            .iter()
            .map(|(work, key, hash)| format!("('{work}', '{key}'::uuid, 28, decode('{hash}', 'hex'))"))
            .collect::<Vec<_>>(),
    );
    assert!(
        !write
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'<' && pair[1].is_ascii_lowercase()),
        "every placeholder is filled"
    );
    write
}

/// Run the write transaction on a dedicated connection. A refusal leaves the
/// session in an aborted transaction block, which is rolled back explicitly.
async fn try_documented_write(
    pool: &sqlx::PgPool,
    manifest: &DispositionManifest,
) -> Result<(), String> {
    let write = documented_write(pool, manifest).await;
    let mut conn = pool.acquire().await.expect("runbook connection");
    match sqlx::raw_sql(&write).execute(&mut *conn).await {
        Ok(_) => Ok(()),
        Err(error) => {
            sqlx::raw_sql("ROLLBACK")
                .execute(&mut *conn)
                .await
                .expect("roll back the refused disposition");
            Err(error.to_string())
        }
    }
}

/// Everything a disposition could touch, rendered for equality checks.
async fn disposition_snapshot(pool: &sqlx::PgPool) -> Vec<Vec<String>> {
    let mut snapshot = Vec::new();
    for sql in [
        "SELECT concat_ws('|', id, status, attempt, terminal_category, settled_at_ms, lease_id, lease_until_ms, body) FROM extension_room_observation_work ORDER BY id",
        "SELECT concat_ws('|', id, status, settled_at_ms) FROM extension_room_publications ORDER BY id",
        "SELECT concat_ws('|', plugin_id, generation, room_jid, message_key, category, recorded_at_ms) FROM extension_room_observation_receipts ORDER BY 1",
        "SELECT concat_ws('|', message_key, kind, encode(semantic_identity_hash, 'hex')) FROM ingress_effect_receipts ORDER BY 1",
        "SELECT concat_ws('|', message_key, terminal_at) FROM ingress_messages ORDER BY 1",
        "SELECT concat_ws('|', plugin_id, generation) FROM extension_room_observers ORDER BY 1",
    ] {
        snapshot.push(
            sqlx::query_scalar::<_, String>(sql)
                .fetch_all(pool)
                .await
                .unwrap_or_else(|error| panic!("snapshot {sql}: {error}")),
        );
    }
    snapshot
}

async fn work_id_for(fixture: &IngressFixture, key: MessageKey) -> String {
    fixture
        .optional_text(&format!(
            "SELECT id FROM extension_room_observation_work WHERE message_key = '{}'",
            key.to_storage()
        ))
        .await
        .expect("work id")
}

/// PostgreSQL: run the runbook's dry run and write transaction verbatim with
/// the reviewed manifest substituted for the placeholders.
async fn apply_documented_disposition(
    fixture: &IngressFixture,
    work_id: &str,
    publication_id: &str,
    unsupported: MessageKey,
    expected_hash: &[u8; 32],
) {
    let pool = sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("runbook pool");
    let manifest = documented_dry_run(&pool, &[room()]).await;
    assert_eq!(
        manifest,
        DispositionManifest {
            work: vec![ReviewedWork {
                id: work_id.to_string(),
                status: "pending".to_string(),
                attempt: 0,
                reason: "room_lost".to_string(),
            }],
            publications: vec![publication_id.to_string()],
            pairs: vec![(
                work_id.to_string(),
                unsupported.to_storage().to_string(),
                hex::encode(expected_hash),
            )],
        },
        "the documented hash expression finds the exact kind-28 pair"
    );
    try_documented_write(&pool, &manifest)
        .await
        .expect("documented disposition transaction");
    pool.close().await;
}

async fn operator_disposition_terminalizes_and_then_ages_out(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let subscription = subscription(&observer);
    let t0 = Utc::now();
    // Class B: successful settlement left a pending publication for a room
    // that can no longer be restored. Its work is completed, its source live.
    seed(&fixture, &observer, "stranded", t0).await;
    settle(&fixture, &subscription, ms(t0), completed(vec![payload()])).await;
    // Class A: pending work in that same unrestorable room.
    let unsupported = seed(&fixture, &observer, "unsupported", t0).await;
    let work_id = fixture
        .optional_text(&format!(
            "SELECT id FROM extension_room_observation_work WHERE message_key = '{}'",
            unsupported.to_storage()
        ))
        .await
        .expect("pending work id");
    let publication_id = fixture
        .optional_text("SELECT id FROM extension_room_publications WHERE status = 'pending'")
        .await
        .expect("stranded publication id");

    let receipt = crate::ingress::receipt_key(&intent(&observer)).expect("receipt key");
    if fixture.db.driver() == DatabaseDriver::Postgres {
        apply_documented_disposition(
            &fixture,
            &work_id,
            &publication_id,
            unsupported,
            &receipt.semantic_identity_hash,
        )
        .await;
    } else {
        apply_disposition(
            &fixture,
            std::slice::from_ref(&work_id),
            std::slice::from_ref(&publication_id),
            unsupported,
            ms(t0),
        )
        .await;
    }
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'completed' AND terminal_category = 'completed'")
            .await,
        1,
        "the completed callback outcome is preserved"
    );
    assert_eq!(
        fixture
            .count("extension_room_observation_receipts WHERE category = 'operator_unsupported'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(&format!(
                "extension_room_observation_receipts WHERE category = 'completed' AND message_key = '{}'",
                unsupported.to_storage()
            ))
            .await,
        0,
        "abandonment never fabricates a completed outcome"
    );

    // The exact abandonment receipt lets maintenance settle the canonical row.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let budget = MaintenanceBudget {
        grace: Duration::zero(),
        ..MaintenanceBudget::DEFAULT
    };
    run_maintenance_pass(&fixture.db, &fixture.uow, budget, None).await;
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    let mut tx = fixture.uow.begin().await.expect("receipts");
    assert!(EffectReceiptRepository::keys(&mut tx, unsupported)
        .await
        .expect("receipts")
        .contains(&receipt));
    drop(tx);
    assert_eq!(fixture.count("extension_room_observation_work").await, 2);

    // The documented disposition stamps the database clock.
    assert_eq!(
        collect_observer_history(&fixture.db, RetentionGcBudget::DEFAULT, horizon(Utc::now()))
            .await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(fixture.count("extension_room_observation_work").await, 0);
    assert_eq!(fixture.count("extension_room_publications").await, 0);
    assert_eq!(
        fixture.count("extension_room_observation_receipts").await,
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn operator_disposition_terminalizes_and_then_ages_out_sqlite() {
    operator_disposition_terminalizes_and_then_ages_out(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn operator_disposition_terminalizes_and_then_ages_out_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_disposition").await {
        operator_disposition_terminalizes_and_then_ages_out(fixture).await;
    }
}

fn healthy_room() -> BareJid {
    "healthy@conference.example.org"
        .parse()
        .expect("healthy room")
}

/// The fixture observer, scoped to the lost fixture room and a healthy room.
fn two_room_observer() -> ConfiguredRoomObserver {
    let mut observer = configured_observer(1, 'a');
    observer.scope = RoomObservationScope::Rooms(vec![room(), healthy_room()]);
    observer
}

/// Capture one new message in `target`, with its canonical row and intent.
async fn seed_in(
    fixture: &IngressFixture,
    observer: &ConfiguredRoomObserver,
    target: &BareJid,
    stanza: &str,
    at: DateTime<Utc>,
) -> MessageKey {
    let mut frozen = intent(observer);
    let IngressEffectIntent::RoomObserver {
        room: intent_room,
        sender: occupant,
        ..
    } = &mut frozen
    else {
        unreachable!("fixture intent is a room observer")
    };
    *intent_room = target.clone();
    *occupant = format!("{target}/author").parse().expect("occupant");
    let mut stanza_message = Message::new(Some(target.clone().into()));
    stanza_message.from = Some(format!("{target}/author").parse().expect("occupant"));
    stanza_message.type_ = MessageType::Groupchat;
    stanza_message.id = Some(Id(format!("wire-{stanza}")));
    stanza_message
        .bodies
        .insert(Lang::new(), "body".to_string());
    add_stanza_id(
        &mut stanza_message,
        &StanzaId::new(stanza, target.clone().into()),
    );
    add_origin_id(&mut stanza_message, &format!("origin-{stanza}"));
    let key = MessageKey::new();
    let mut tx = fixture.uow.begin().await.expect("seed");
    record_message(&mut tx, key).await;
    Repo::sync_configured(&mut tx, std::slice::from_ref(observer), ms(at))
        .await
        .expect("sync");
    capture(
        &mut tx,
        key,
        target,
        &stanza_message,
        &sender(),
        &[frozen],
        at,
    )
    .await
    .expect("capture");
    tx.commit().await.expect("seed commit");
    key
}

async fn runbook_pool(fixture: &IngressFixture) -> sqlx::PgPool {
    sqlx::PgPool::connect(fixture.db.database_url())
        .await
        .expect("runbook pool")
}

#[tokio::test]
async fn postgres_disposition_retires_only_the_unsupported_rooms() {
    let Some(fixture) = IngressFixture::postgres("observer_disposition_mixed").await else {
        return;
    };
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = two_room_observer();
    let mut lost = subscription(&observer);
    lost.room = room();
    let mut healthy = subscription(&observer);
    healthy.room = healthy_room();
    let t0 = Utc::now();
    // Lost room: a stranded publication (Class B) and pending work (Class A).
    seed_in(&fixture, &observer, &room(), "lost-stranded", t0).await;
    settle(&fixture, &lost, ms(t0), completed(vec![payload()])).await;
    let unsupported = seed_in(&fixture, &observer, &room(), "lost-pending", t0).await;
    // Healthy room: a completed result awaiting publication and pending work.
    let healthy_published = seed_in(&fixture, &observer, &healthy_room(), "ok-result", t0).await;
    settle(&fixture, &healthy, ms(t0), completed(vec![payload()])).await;
    let healthy_pending = seed_in(&fixture, &observer, &healthy_room(), "ok-pending", t0).await;

    let pool = runbook_pool(&fixture).await;
    let manifest = documented_dry_run(&pool, &[room()]).await;
    let unsupported_work = work_id_for(&fixture, unsupported).await;
    let stranded_publication = fixture
        .optional_text(&format!(
            "SELECT id FROM extension_room_publications WHERE room_jid = '{}'",
            room()
        ))
        .await
        .expect("stranded publication");
    assert_eq!(
        manifest
            .work
            .iter()
            .map(|work| &work.id)
            .collect::<Vec<_>>(),
        vec![&unsupported_work]
    );
    assert_eq!(manifest.publications, vec![stranded_publication]);
    assert_eq!(
        manifest
            .pairs
            .iter()
            .map(|(work, _, _)| work)
            .collect::<Vec<_>>(),
        vec![&unsupported_work],
        "the pair manifest names exactly the Class A rows"
    );
    let healthy_work = work_id_for(&fixture, healthy_pending).await;
    let healthy_before = fixture
        .optional_text(&format!(
            "SELECT concat_ws('|', status, attempt, body, settled_at_ms) FROM extension_room_observation_work WHERE id = '{healthy_work}'"
        ))
        .await;

    try_documented_write(&pool, &manifest)
        .await
        .expect("disposition of the unsupported set");

    assert_eq!(
        fixture
            .optional_text(&format!(
                "SELECT concat_ws('|', status, attempt, body, settled_at_ms) FROM extension_room_observation_work WHERE id = '{healthy_work}'"
            ))
            .await,
        healthy_before,
        "healthy pending work is untouched"
    );
    assert_eq!(
        fixture
            .count(&format!(
                "extension_room_publications WHERE room_jid = '{}' AND status = 'pending' AND settled_at_ms IS NULL",
                healthy_room()
            ))
            .await,
        1,
        "the healthy room's result still awaits publication"
    );
    assert_eq!(
        fixture
            .count(&format!(
                "ingress_effect_receipts WHERE message_key = '{}'",
                healthy_pending.to_storage()
            ))
            .await,
        0,
        "healthy pending intents stay unreceipted"
    );
    assert_eq!(
        fixture
            .count(&format!(
                "ingress_effect_intents WHERE message_key = '{}'",
                healthy_pending.to_storage()
            ))
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(&format!(
                "ingress_effect_receipts WHERE message_key = '{}'",
                healthy_published.to_storage()
            ))
            .await,
        1,
        "the healthy settlement receipt is unchanged"
    );
    assert_eq!(
        fixture
            .count(&format!(
                "extension_room_observation_work WHERE id = '{unsupported_work}' AND status = 'terminal' AND terminal_category = 'operator_unsupported'"
            ))
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(&format!(
                "extension_room_publications WHERE room_jid = '{}' AND status = 'stale'",
                room()
            ))
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(&format!(
                "ingress_effect_receipts WHERE message_key = '{}' AND kind = 28",
                unsupported.to_storage()
            ))
            .await,
        1
    );
    pool.close().await;
    fixture.close().await;
}

/// Seed one Class A row and return its pool. The caller arranges the state
/// the dry run reviews and the change that must make the write refuse.
async fn refusal_fixture(
    name: &str,
) -> Option<(IngressFixture, ConfiguredRoomObserver, MessageKey)> {
    let fixture = IngressFixture::postgres(name).await?;
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let key = seed(&fixture, &observer, "refused", Utc::now()).await;
    Some((fixture, observer, key))
}

async fn assert_refused_without_change(
    fixture: &IngressFixture,
    manifest: &DispositionManifest,
    reason: &str,
) {
    let pool = runbook_pool(fixture).await;
    let before = disposition_snapshot(&pool).await;
    let error = try_documented_write(&pool, manifest)
        .await
        .expect_err("the disposition must refuse");
    assert!(error.contains(reason), "unexpected refusal: {error}");
    assert_eq!(
        disposition_snapshot(&pool).await,
        before,
        "a refused disposition changes nothing"
    );
    pool.close().await;
}

#[tokio::test]
async fn postgres_disposition_refuses_unexpired_lease() {
    let Some((fixture, observer, _)) = refusal_fixture("observer_refuse_lease").await else {
        return;
    };
    // The dry run reviews leased work whose lease is still live.
    let mut tx = fixture.uow.begin().await.expect("claim");
    Repo::claim(&mut tx, &subscription(&observer), crate::time::now_ms())
        .await
        .expect("claim")
        .expect("work");
    tx.commit().await.expect("lease");
    let pool = runbook_pool(&fixture).await;
    let manifest = documented_dry_run(&pool, &[room()]).await;
    pool.close().await;
    assert_eq!(manifest.work.len(), 1);
    assert_eq!(manifest.work[0].status, "leased");
    assert_refused_without_change(&fixture, &manifest, "unexpired lease").await;
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'leased'")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_disposition_refuses_rows_changed_since_review() {
    let Some((fixture, _, key)) = refusal_fixture("observer_refuse_changed").await else {
        return;
    };
    let pool = runbook_pool(&fixture).await;
    let manifest = documented_dry_run(&pool, &[room()]).await;
    pool.close().await;
    assert_eq!(manifest.work.len(), 1);
    let work_id = work_id_for(&fixture, key).await;
    // Attempt moved on while the status still reads pending.
    fixture
        .execute(
            "UPDATE extension_room_observation_work SET attempt = attempt + 1 WHERE id = ?",
            crate::db_params![&work_id],
        )
        .await;
    assert_refused_without_change(&fixture, &manifest, "changed since review").await;
    // Status moved on (to an expired lease) while the attempt is as reviewed.
    fixture
        .execute(
            "UPDATE extension_room_observation_work SET attempt = attempt - 1, status = 'leased', lease_until_ms = 1 WHERE id = ?",
            crate::db_params![&work_id],
        )
        .await;
    assert_refused_without_change(&fixture, &manifest, "changed since review").await;
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'leased' AND attempt = 0")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_disposition_refuses_reconfigured_generation() {
    let Some((fixture, _, _)) = refusal_fixture("observer_refuse_generation").await else {
        return;
    };
    // The plugin moved to generation 2 without staling generation 1 work.
    fixture
        .execute(
            "UPDATE extension_room_observers SET generation = 2 WHERE plugin_id = 'observer-fixture'",
            (),
        )
        .await;
    let pool = runbook_pool(&fixture).await;
    let manifest = documented_dry_run(&pool, &[]).await;
    pool.close().await;
    assert_eq!(manifest.work.len(), 1);
    assert_eq!(manifest.work[0].reason, "generation_retired");
    assert_eq!(manifest.pairs.len(), 1);
    // Generation 1 is configured again before the write runs.
    fixture
        .execute(
            "UPDATE extension_room_observers SET generation = 1 WHERE plugin_id = 'observer-fixture'",
            (),
        )
        .await;
    assert_refused_without_change(&fixture, &manifest, "configured again").await;
    assert_eq!(
        fixture
            .count("extension_room_observation_work WHERE status = 'pending'")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_disposition_dry_run_reads_one_snapshot() {
    let Some(fixture) = IngressFixture::postgres("observer_dry_run_snapshot").await else {
        return;
    };
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    let observer = configured_observer(1, 'a');
    let key = seed(&fixture, &observer, "snapshot", Utc::now()).await;
    let work_id = work_id_for(&fixture, key).await;
    let pool = runbook_pool(&fixture).await;
    // Another connection settles the reviewed work, with its ingress
    // receipt, after the first manifest query and before the third.
    let settlement = async {
        settle(
            &fixture,
            &subscription(&observer),
            crate::time::now_ms(),
            failed(),
        )
        .await;
        assert_eq!(
            fixture
                .count(&format!(
                    "ingress_effect_receipts WHERE message_key = '{}'",
                    key.to_storage()
                ))
                .await,
            1,
            "the settlement committed"
        );
    };
    let manifest = documented_dry_run_observed(&pool, &[room()], settlement).await;
    assert_eq!(
        manifest
            .work
            .iter()
            .map(|work| &work.id)
            .collect::<Vec<_>>(),
        vec![&work_id]
    );
    assert_eq!(
        manifest
            .pairs
            .iter()
            .map(|(work, _, _)| work)
            .collect::<Vec<_>>(),
        vec![&work_id],
        "the pair query reads the snapshot the work query fixed"
    );
    // Outside that snapshot the settlement is visible: a fresh review is empty.
    let fresh = documented_dry_run(&pool, &[room()]).await;
    assert!(fresh.work.is_empty() && fresh.pairs.is_empty());
    pool.close().await;
    fixture.close().await;
}

/// Enough recent history, plus a little expired history, that a planner
/// with statistics prefers indexes for the protective guards.
async fn seed_planner_history(fixture: &IngressFixture) {
    let mut tx = fixture.db.begin_immediate().await.expect("planner seed");
    for index in 0..3_000_i64 {
        // Settled history is one expired cohort stamped at a single instant,
        // as V1023's backfill leaves it, across every final status, plus a
        // little active work. 1% of sources are retracted.
        let expired = index % 100 == 0;
        let settled_at = 0_i64;
        let status = if index % 100 == 1 {
            "pending"
        } else {
            ["completed", "terminal", "stale"][usize::try_from(index % 3).expect("index")]
        };
        let publication_status = if index % 2 == 0 { "published" } else { "stale" };
        let work_id = Uuid::now_v7().to_string();
        let source_key = Uuid::now_v7().to_string();
        let message_key = Uuid::now_v7().to_string();
        tx.execute(
            "INSERT INTO extension_room_observation_work (id, source_key, message_key, plugin_id, generation, identity, room_jid, revision, source_json, body, status, attempt, due_at_ms, terminal_category, settled_at_ms) VALUES (?, ?, ?, 'observer', 1, 'identity', 'room@conference.example.org', 0, '{}', '', ?, 1, 0, NULL, ?)",
            crate::db_params![&work_id, &source_key, &message_key, status, (status != "pending").then_some(settled_at)],
        )
        .await
        .expect("work");
        tx.execute(
            "INSERT INTO extension_room_publications (id, work_id, output_index, source_key, plugin_id, generation, identity, room_jid, revision, source_json, payload_json, status, settled_at_ms) VALUES (?, ?, 0, ?, 'observer', 1, 'identity', 'room@conference.example.org', 0, '{}', '{}', ?, ?)",
            crate::db_params![Uuid::now_v7().to_string(), &work_id, &source_key, publication_status, settled_at],
        )
        .await
        .expect("publication");
        tx.execute(
            "INSERT INTO extension_room_observation_receipts (plugin_id, generation, room_jid, message_key, category, recorded_at_ms) VALUES ('observer', 1, 'room@conference.example.org', ?, 'completed', ?)",
            crate::db_params![&message_key, settled_at],
        )
        .await
        .expect("receipt");
        tx.execute(
            "INSERT INTO extension_room_sources (source_key, room_jid, sender_jid, root_stanza_id, revision_stanza_id, root_origin_id, revision, source_json, retracted, captured_at_ms) VALUES (?, 'room@conference.example.org', 'author@example.org', ?, ?, NULL, 0, '{}', ?, ?)",
            // Every source is aged: live identity outlives the horizon by
            // design, so only the 1% retracted ones may be GC candidates.
            crate::db_params![&source_key, format!("stanza-{index}"), format!("stanza-{index}"), i64::from(expired), 0_i64],
        )
        .await
        .expect("source");
        tx.execute(
            "INSERT INTO extension_room_source_revisions (room_jid, room_stanza_id, source_key) VALUES ('room@conference.example.org', ?, ?)",
            crate::db_params![format!("stanza-{index}"), &source_key],
        )
        .await
        .expect("revision");
    }
    tx.execute("ANALYZE", ()).await.expect("analyze");
    tx.commit().await.expect("planner seed commit");
}

/// Every retention statement, as the driver runs it, with representative
/// parameters: a cutoff that selects the expired 1% and a full budget.
fn retention_statements(
    driver: DatabaseDriver,
) -> Vec<(&'static str, String, Vec<crate::db::Value>)> {
    use super::super::retention as statements;
    let postgres = driver == DatabaseDriver::Postgres;
    let pick = |sqlite: &'static str, pg: &'static str| if postgres { pg } else { sqlite };
    let cutoff = || {
        vec![
            crate::db::Value::from(1_000_i64),
            crate::db::Value::from(i64::from(BATCH)),
        ]
    };
    let mut source_delete: Vec<crate::db::Value> = (0..3)
        .map(|_| crate::db::Value::from(Uuid::now_v7().to_string()))
        .collect();
    source_delete.push(crate::db::Value::from(1_000_i64));
    vec![
        (
            "publications",
            pick(
                statements::PUBLICATIONS_SQLITE,
                statements::PUBLICATIONS_POSTGRES,
            )
            .to_string(),
            cutoff(),
        ),
        (
            "work",
            pick(statements::WORK_SQLITE, statements::WORK_POSTGRES).to_string(),
            cutoff(),
        ),
        (
            "receipts",
            pick(statements::RECEIPTS_SQLITE, statements::RECEIPTS_POSTGRES).to_string(),
            cutoff(),
        ),
        (
            "revisions",
            pick(statements::REVISIONS_SQLITE, statements::REVISIONS_POSTGRES).to_string(),
            cutoff(),
        ),
        (
            "source candidates",
            pick(
                statements::SOURCE_CANDIDATES_SQLITE,
                statements::SOURCE_CANDIDATES_POSTGRES,
            )
            .to_string(),
            cutoff(),
        ),
        (
            "source delete",
            statements::source_delete_sql(3),
            source_delete,
        ),
    ]
}

async fn plan_lines(
    fixture: &IngressFixture,
    explain: &str,
    sql: &str,
    params: Vec<crate::db::Value>,
) -> Vec<String> {
    let column = if fixture.db.driver() == DatabaseDriver::Postgres {
        0
    } else {
        3
    };
    let mut tx = fixture.db.begin().await.expect("plan transaction");
    if fixture.db.driver() == DatabaseDriver::Postgres {
        // At fixture scale a hash join over a sequential scan can cost less
        // than the index lookups it replaces at production scale. Disabling
        // sequential scans makes the check size-independent: a `Seq Scan`
        // that survives means no index can serve that access at all.
        tx.execute("SET LOCAL enable_seqscan = off", ())
            .await
            .expect("prefer indexes");
    }
    let mut rows = tx
        .query(&format!("{explain} {sql}"), params)
        .await
        .unwrap_or_else(|error| panic!("{explain} {sql}: {error}"));
    let mut lines = Vec::new();
    while let Some(row) = rows.next().await.expect("plan row") {
        lines.push(row.get::<String>(column).expect("plan text"));
    }
    drop(rows);
    tx.rollback().await.expect("discard plan transaction");
    lines
}

async fn retention_guards_are_index_lookups(fixture: IngressFixture) {
    initialize_room_observations(&fixture.db)
        .await
        .expect("schema");
    seed_planner_history(&fixture).await;
    let driver = fixture.db.driver();
    for (name, sql, params) in retention_statements(driver) {
        let (explain, forbidden): (&str, Vec<String>) = match driver {
            // A correlated guard must SEARCH its table, never SCAN it.
            DatabaseDriver::Sqlite => (
                "EXPLAIN QUERY PLAN",
                [
                    "w",
                    "p",
                    "v",
                    "m",
                    "s",
                    "extension_room_observation_work",
                    "extension_room_publications",
                    "extension_room_source_revisions",
                    "extension_room_sources",
                ]
                .iter()
                .map(|alias| format!("SCAN {alias}"))
                .collect(),
            ),
            // Guards are aliased; the unaliased DELETE target may be hash
            // semi-joined to its locked candidates, which is not a guard.
            DatabaseDriver::Postgres => (
                "EXPLAIN",
                [
                    "extension_room_observation_work w",
                    "extension_room_publications p",
                    "extension_room_source_revisions v",
                    // Aliased or not: aged live sources must never be walked.
                    "extension_room_sources",
                ]
                .iter()
                .map(|table| format!("Seq Scan on {table}"))
                .collect(),
            ),
        };
        let plan = plan_lines(&fixture, explain, &sql, params).await;
        if let Some((index, order_column)) = match name {
            "publications" => Some(("extension_room_publications_settled", "settled_at_ms")),
            "work" => Some(("extension_room_observation_work_settled", "settled_at_ms")),
            "receipts" => Some((
                "extension_room_observation_receipts_recorded",
                "recorded_at_ms",
            )),
            _ => None,
        } {
            // Candidates come off the index already in ORDER BY order, so a
            // whole expired cohort (V1023 stamps one instant) is never sorted
            // to delete one LIMIT's worth.
            assert!(
                plan.iter().any(|line| line.contains(index)),
                "{name} does not select candidates through {index}:\n{}",
                plan.join("\n")
            );
            let sorts: Vec<&String> = plan
                .iter()
                .filter(|line| {
                    line.contains("USE TEMP B-TREE FOR")
                        || (line.contains("Sort Key:") && line.contains(order_column))
                        || line.contains("Presorted Key:")
                })
                .collect();
            assert!(
                sorts.is_empty(),
                "{name} sorts its candidates: {sorts:?}\nfull plan:\n{}",
                plan.join("\n")
            );
        }
        if matches!(name, "revisions" | "source candidates") {
            // Live sources age past the horizon and are kept forever; only
            // the retracted-only partial index keeps them out of the walk.
            assert!(
                plan.iter()
                    .any(|line| line.contains("extension_room_sources_retracted_captured")),
                "{name} does not use the retracted-source index:\n{}",
                plan.join("\n")
            );
        }
        if name == "receipts" {
            // The active-work guard has its own partial index (V1023); the
            // generic status indexes would read every active row per receipt.
            assert!(
                plan.iter()
                    .any(|line| line.contains("extension_room_observation_work_active_guard")),
                "receipts guard does not use the active-work index:\n{}",
                plan.join("\n")
            );
        }
        let scans: Vec<&String> = plan
            .iter()
            .filter(|line| {
                forbidden.iter().any(|scan| {
                    let line = line.trim_start().trim_start_matches("->").trim_start();
                    line == scan.as_str() || line.starts_with(&format!("{scan} "))
                })
            })
            .collect();
        assert!(
            scans.is_empty(),
            "{name} scans a guarded table: {scans:?}\nfull plan:\n{}",
            plan.join("\n")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn retention_guards_are_index_lookups_sqlite() {
    retention_guards_are_index_lookups(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn retention_guards_are_index_lookups_postgres() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_plans").await {
        retention_guards_are_index_lookups(fixture).await;
    }
}
