use anyhow::{Context, Result};
use tracing::info;
use waddle_ecdysis::InheritedListeners;
use waddle_server::config;
use waddle_server::config::ServerConfig;
use waddle_server::{db, server, telemetry};

/// Tokio's default 2 MiB worker stack is no longer enough for the deepest
/// protocol futures (a debug/ci-test-profile session-initiate rewrite
/// overflows it and aborts the whole process — every connected socket then
/// resets without a close handshake; first tripped on CI by the #1702
/// merge). 8 MiB matches the platform main-thread default and keeps
/// headroom; the real long-term fix is boxing the fattest handler futures.
fn main() -> Result<()> {
    // SAFETY: This is the first startup operation, before runtime/telemetry
    // threads or other descriptor owners exist. The launcher exclusively
    // transfers the LISTEN_FDS descriptors at exec; this call adopts them once.
    let inherited = unsafe { InheritedListeners::from_env() };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .context("build tokio runtime")?
        .block_on(async_main(inherited))
}

async fn async_main(inherited: Option<InheritedListeners>) -> Result<()> {
    let inherited = inherited.map(InheritedListeners::into_listener_set);
    // Install the ring crypto provider for rustls (required for XMPP TLS)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Initialize telemetry
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        telemetry::init().map_err(|e| anyhow::anyhow!("Failed to init telemetry: {}", e))?;
    } else {
        telemetry::init_local()
            .map_err(|e| anyhow::anyhow!("Failed to init local telemetry: {}", e))?;
    }

    info!("Waddle Server starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: {}", env!("CARGO_PKG_LICENSE"));

    // Report inherited listeners captured from the launcher before runtime startup.
    if inherited.is_some() {
        info!("Inherited listeners from launcher");
    }

    // Load configuration
    let server_config = ServerConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load server configuration: {}", e))?;
    server_config.log_config();

    // Initialize database (driver + DSN contract)
    let db_runtime = config::DatabaseRuntimeConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load database runtime config: {}", e))?;
    let lineage_config = config::LineageConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load database lineage configuration: {}", e))?;
    info!(
        driver = ?db_runtime.driver,
        "Using configured database runtime"
    );
    // The dedicated control-plane pool (ADR-0017 element 4/12) hosts only
    // node/claim liveness statements (the keypair-slot lease heartbeat, and —
    // from Phase 3 Slice 1 — the claims CAS), which no code path issues
    // unless clustering is actually running. Provisioning it on every
    // Postgres deployment regardless of `clustering.enabled` would open and
    // connect-validate a second pool nothing uses, holding idle connections
    // against the database for a feature never requested, and a transient
    // failure on that second connect would fail startup for that same unused
    // feature. So it is gated on BOTH the configured driver being Postgres
    // AND clustering being enabled for this run.
    let mut db_config = db::DatabaseConfig::new(db_runtime.driver, db_runtime.database_url);
    db_config.pool_size = db_runtime.pool_size;
    db_config.acquire_timeout = db_runtime.acquire_timeout;
    if db_runtime.driver == db::DatabaseDriver::Postgres && server_config.clustering.enabled {
        db_config = db_config.with_control_plane_pool(db_runtime.control_plane_pool_size);
    }

    let pool_config = db::PoolConfig;
    let db_pool = db::DatabasePool::try_new(db_config, pool_config)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize database: {}", e))?;

    // Pre-migration lineage guard (#1652): the append-only migration ledger
    // (#1651) is the most damaging write in the boot path, so the GLOBAL
    // boundary's lineage is checked BEFORE migrations run. The operator
    // action (enroll / adopt) applies here first for the same reason. A
    // database that holds a lineage row that does not verify — another
    // deployment's database, an unadopted restore — refuses migrations
    // outright. A database with NO row (fresh install, or the first rollout
    // of a lineage-aware binary) proceeds: enrollment is what creates the
    // row, and refusing here would deadlock the bootstrap. That un-enrolled
    // window is the accepted residual risk and is closed at readiness.
    db::lineage::ensure_table(db_pool.global())
        .await
        .context("Failed to bootstrap database lineage table")?;
    let global_adopt_matched = match &lineage_config.action {
        Some(db::lineage::LineageAction::Enroll) => {
            info!(store = "global", "enrolling database lineage");
            db::lineage::enroll(db_pool.global(), &lineage_config)
                .await
                .context("Failed to enroll global database lineage")?;
            None
        }
        Some(db::lineage::LineageAction::Adopt(expected)) => {
            match db::lineage::adopt_if_matched(db_pool.global(), &lineage_config, expected)
                .await
                .context("Failed to apply global database lineage adoption")?
            {
                db::lineage::AdoptOutcome::Adopted { matched, .. } => {
                    info!(store = "global", matched = %matched, "adopted database lineage");
                    Some(matched)
                }
                db::lineage::AdoptOutcome::NotMatched => None,
            }
        }
        None => None,
    };
    match db::lineage::verify(db_pool.global(), &lineage_config).await {
        Ok(_) => {}
        Err(db::DatabaseError::Lineage(db::lineage::LineageError::MissingRow)) => {
            // A clustered node starts writing dynamic control-plane rows
            // (leases, node registration) right after migrations — before
            // the full readiness attestation could hold it back. An
            // un-enrolled database is therefore acceptable only when this
            // process is the one enrolling it; otherwise a clustered node
            // mis-pointed at a fresh/foreign row-less database would
            // mutate it. Single-node deployments stay lenient: they write
            // nothing dynamic before the readiness gate.
            if server_config.clustering.enabled
                && !matches!(
                    lineage_config.action,
                    Some(db::lineage::LineageAction::Enroll)
                )
            {
                return Err(anyhow::anyhow!(
                    "refusing to start clustering against a database with no lineage row: \
                     enroll it first (WADDLE_DB_LINEAGE_ACTION=enroll on one rollout)"
                ));
            }
            info!("global database has no lineage row yet; proceeding to migrations un-enrolled");
        }
        Err(error) => {
            return Err(anyhow::anyhow!(error).context(
                "refusing to run migrations: the global database's lineage does not \
                 describe this deployment (mis-pointed DSN, unadopted restore, or \
                 missing WADDLE_DEPLOYMENT_UUID)",
            ));
        }
    }

    let migration_runner = db::MigrationRunner::single();
    migration_runner
        .run(db_pool.global())
        .await
        .context("Failed to run migrations")?;

    info!("Database initialized and migrations complete");

    // Start the server
    let metrics_flush = server::start(
        db_pool,
        server_config,
        lineage_config,
        global_adopt_matched,
        inherited,
    )
    .await?;

    telemetry::shutdown(metrics_flush);

    Ok(())
}
