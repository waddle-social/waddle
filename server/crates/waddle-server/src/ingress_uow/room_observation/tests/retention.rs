//! Bounded retention of settled observer history (#1901).

use super::*;
use crate::db::DatabaseDriver;
use crate::ingress::gc::RetentionGcBudget;
use crate::ingress::maintenance::{
    collect_observer_history, run_maintenance_pass, MaintenanceBudget, MaintenanceOutcome,
};
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

/// One SQL block of the runbook, delimited by `<!-- {marker}:begin|end -->`.
/// Never skips on a missing file: the nix test lanes copy the runbook.
fn runbook_sql_block(marker: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/operations/ingress-authority.md");
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "read ingress-authority runbook at {} (is the flake.nix postUnpack copy intact?): {error}",
            path.display()
        )
    });
    let begin = format!("<!-- {marker}:begin -->");
    let end = format!("<!-- {marker}:end -->");
    let start = doc.find(&begin).expect("runbook begin marker") + begin.len();
    let stop = start + doc[start..].find(&end).expect("runbook end marker");
    let block = &doc[start..stop];
    let sql_start = block.find("```sql").expect("sql fence") + "```sql".len();
    let sql_end = sql_start + block[sql_start..].find("```").expect("closed sql fence");
    block[sql_start..sql_end].to_string()
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
    let dry_run = runbook_sql_block("observer-disposition-dry-run")
        .replace("<permanently-lost-room>", &room().to_string());
    let statements: Vec<String> = dry_run
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .split(';')
        .map(str::trim)
        .filter(|statement| statement.starts_with("SELECT"))
        .map(str::to_string)
        .collect();
    assert_eq!(statements.len(), 3, "dry run has three manifest queries");
    let mut reviewed = Vec::new();
    for statement in &statements {
        reviewed.push(
            sqlx::query(statement)
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|error| panic!("dry run {statement}: {error}")),
        );
    }
    use sqlx::Row as _;
    let work: Vec<(String, String, i64, String)> = reviewed[0]
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("id"),
                row.get::<String, _>("status"),
                row.get::<i64, _>("attempt"),
                row.get::<String, _>("reason"),
            )
        })
        .collect();
    assert_eq!(
        work,
        vec![(
            work_id.to_string(),
            "pending".to_string(),
            0,
            "room_lost".to_string()
        )]
    );
    let publications: Vec<String> = reviewed[1]
        .iter()
        .map(|row| row.get::<String, _>("id"))
        .collect();
    assert_eq!(publications, vec![publication_id.to_string()]);
    let pairs: Vec<(String, i32, String)> = reviewed[2]
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("work_id"),
                row.get::<i32, _>("kind"),
                row.get::<String, _>("semantic_identity_hash"),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![(work_id.to_string(), 28, hex::encode(expected_hash))],
        "the documented hash expression finds the exact kind-28 pair"
    );
    let epoch: String =
        sqlx::query_scalar("SELECT epoch::text FROM ingress_protocol_epoch WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("live epoch");
    let write = runbook_sql_block("observer-disposition-write")
        .replace("\\set ON_ERROR_STOP on", "")
        .replace("<current epoch>", &epoch)
        .replace("<reviewed-work-id>", work_id)
        .replace("<reviewed-status>", "pending")
        .replace("<reviewed-attempt>", "0")
        .replace("<generation_retired|room_lost>", "room_lost")
        .replace("<reviewed-publication-id>", publication_id)
        .replace(
            "<reviewed-message-key>",
            &unsupported.to_storage().to_string(),
        )
        .replace("<reviewed-hex-hash>", &hex::encode(expected_hash));
    assert!(
        !write
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'<' && pair[1].is_ascii_lowercase()),
        "every placeholder is filled"
    );
    sqlx::raw_sql(&write)
        .execute(&pool)
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
