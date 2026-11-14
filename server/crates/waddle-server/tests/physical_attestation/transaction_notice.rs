//! Real PG17 coverage that attested checkout hooks send transaction control
//! only to sessions that are inside a transaction.
//!
//! PostgreSQL answers `ROLLBACK` or `COMMIT` outside a transaction with the
//! warning "there is no transaction in progress", which SQLx logs under the
//! `sqlx::postgres::notice` target. Checkout hooks run on every pool acquire,
//! so a stray statement there logs once per database request.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::Connection;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;
use waddle_server::db::physical_attestation::{
    attested_pool, install, pinned_attested_pool, AttestedPgPool, PinnedRuntimeAuthority,
};
use waddle_server::db::DarkMigrationLedgerPins;

use super::{dark_attestor_fixture, test_url, Fixture, CATALOG_GRANT_LOCK};

const NOTICE_TARGET: &str = "sqlx::postgres::notice";

/// Records every PostgreSQL notice SQLx reports while installed.
#[derive(Clone, Default)]
struct NoticeCapture(Arc<Mutex<Vec<String>>>);

impl NoticeCapture {
    fn notices(&self) -> Vec<String> {
        self.0.lock().expect("notice capture").clone()
    }
}

struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: Subscriber> Layer<S> for NoticeCapture {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != NOTICE_TARGET {
            return;
        }
        let mut message = MessageVisitor(String::new());
        event.record(&mut message);
        self.0.lock().expect("notice capture").push(message.0);
    }
}

// Mirrors the synthetic bundle used by the pinned hook tests: it exercises
// checkout only and does not claim to prove the ledger or guard manifest.
fn pinned_pool(
    fixture: &Fixture,
    config: &waddle_server::db::physical_attestation::PhysicalAttestationConfig,
    attestor: waddle_server::db::physical_attestation::DarkPhysicalAttestationPins,
) -> AttestedPgPool {
    let schema_oid = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity")
        .schema
        .oid;
    let base = 3_000_000_100;
    let ledger = DarkMigrationLedgerPins {
        schema_oid,
        schema_owner_role_oid: attestor.schema_owner_role_oid,
        runtime_role_oid: attestor.runtime_role_oid,
        ledger_relation_oid: base,
        ledger_primary_index_oid: base + 1,
    };
    let pins = PinnedRuntimeAuthority::new(config, attestor, ledger, base + 2, base + 3, base + 4)
        .expect("valid independent test bundle");
    pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        pins,
        1,
        Duration::from_secs(5),
    )
    .expect("pinned pool")
}

/// Drive the request shapes the database actor issues through a single-slot
/// pool, so each step checks out the same session through the hook: plain
/// reads, a committed and a rolled-back SQLx transaction, a borrower that
/// leaks an open raw transaction for the hook to end, and one that leaks a
/// failed raw transaction for the hook to discard.
async fn drive_typical_checkouts(pool: &AttestedPgPool) {
    for _ in 0..3 {
        let mut member = pool.acquire_for_test().await.expect("checkout");
        sqlx::query("SELECT 1")
            .execute(&mut *member)
            .await
            .expect("plain read");
    }

    let mut member = pool.acquire_for_test().await.expect("checkout");
    let mut committed = member.begin().await.expect("begin");
    sqlx::query("SELECT 1")
        .execute(&mut *committed)
        .await
        .expect("read in transaction");
    committed.commit().await.expect("commit");
    let rolled_back = member.begin().await.expect("begin");
    rolled_back.rollback().await.expect("rollback");
    drop(member);

    let mut member = pool.acquire_for_test().await.expect("checkout");
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *member)
        .await
        .expect("raw begin");
    drop(member);
    let mut member = pool
        .acquire_for_test()
        .await
        .expect("checkout after an inherited open transaction");
    // Had checkout left the inherited transaction open, PostgreSQL would
    // answer this BEGIN with "there is already a transaction in progress".
    sqlx::query("BEGIN")
        .execute(&mut *member)
        .await
        .expect("raw begin");
    let failed_pid = backend_pid(&mut member).await;
    sqlx::query("SELECT 1 / 0")
        .execute(&mut *member)
        .await
        .expect_err("abort the raw transaction");
    drop(member);
    let mut member = pool
        .acquire_for_test()
        .await
        .expect("checkout after an inherited failed transaction");
    assert_ne!(
        backend_pid(&mut member).await,
        failed_pid,
        "a session with a failed inherited transaction is discarded"
    );
}

async fn backend_pid(connection: &mut sqlx::PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(connection)
        .await
        .expect("backend PID")
}

fn assert_no_notices(capture: &NoticeCapture) {
    let notices = capture.notices();
    assert!(
        notices.is_empty(),
        "checkout hooks produced PostgreSQL notices: {notices:?}"
    );
}

// Each test uses a current-thread runtime so the thread-local subscriber
// observes the pool's connection I/O, including spawned release tasks.
#[tokio::test]
async fn pinned_checkout_sends_no_transaction_control_outside_a_transaction() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = pinned_pool(&fixture, &config, attestor);

    let capture = NoticeCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    drive_typical_checkouts(&pool).await;
    pool.close().await;
    drop(guard);

    assert_no_notices(&capture);
    fixture.cleanup().await;
}

#[tokio::test]
async fn attested_checkout_sends_no_transaction_control_outside_a_transaction() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install attestation");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_secs(5),
    )
    .expect("attested pool");

    let capture = NoticeCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    drive_typical_checkouts(&pool).await;
    pool.close().await;
    drop(guard);

    assert_no_notices(&capture);
    fixture.cleanup().await;
}
