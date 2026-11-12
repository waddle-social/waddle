use anyhow::{Context, Result};
use tracing::info;
use waddle_ecdysis::InheritedListeners;
use waddle_server::config;
use waddle_server::config::ServerConfig;
use waddle_server::{db, server, telemetry};
use waddle_xmpp::telemetry::attributes::RestrictedStartupStage;
use waddle_xmpp::telemetry::authority::record_restricted_startup_refusal;

/// Tokio's default 2 MiB worker stack is no longer enough for the deepest
/// protocol futures (a debug/ci-test-profile session-initiate rewrite
/// overflows it and aborts the whole process — every connected socket then
/// resets without a close handshake; first tripped on CI by the #1702
/// merge). 8 MiB matches the platform main-thread default and keeps
/// headroom; the real long-term fix is boxing the fattest handler futures.
fn main() -> Result<()> {
    // Any argument selects an operator subcommand, which runs to completion
    // on its own runtime and exits. It is dispatched before listener
    // adoption: a subcommand owns no sockets and reads no serving
    // configuration. Without arguments the server starts.
    if std::env::args_os().len() > 1 {
        std::process::exit(waddle_server::owner_cli::run_from_args());
    }
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
    let xmpp_config = server::XmppConfig::from_env()
        .map_err(|error| anyhow::anyhow!("Failed to load XMPP configuration: {error}"))?;

    // The schema baseline, when selected, replaces the migration catalog for
    // every ledger reader in the process, so it is chosen before the
    // database is opened. Unset leaves the migration history in place.
    db::select_schema_baseline_from_env().map_err(|error| anyhow::anyhow!(error))?;

    // Initialize database (driver + DSN contract)
    let db_runtime = config::DatabaseRuntimeConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load database runtime config: {}", e))?;
    let lineage_config = config::LineageConfig::from_env()
        .map_err(|e| anyhow::anyhow!("Failed to load database lineage configuration: {}", e))?;
    info!(
        driver = ?db_runtime.driver,
        "Using configured database runtime"
    );
    let (db_pool, global_adopt_matched, owner_outbox) =
        match std::env::var_os("WADDLE_DB_RUNTIME_ATTESTATION") {
            Some(document) => {
                if db_runtime.driver != db::DatabaseDriver::Postgres {
                    anyhow::bail!("restricted runtime attestation requires PostgreSQL");
                }
                let document = document
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid runtime attestation document"))?;
                let release_git_sha = std::env::var("WADDLE_GIT_SHA").map_err(|_| {
                    db::runtime_startup::RuntimeStartupError::ReleaseIdentityMissing
                })?;
                let identity = match db::runtime_startup::RuntimeStartupIdentity::from_json(
                    &document,
                    &lineage_config,
                    &release_git_sha,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        return Err(refused_restricted_start(
                            RestrictedStartupStage::Handoff,
                            error,
                        )
                        .await);
                    }
                };
                let limits = db::bootstrap::RuntimePoolLimits {
                    main_connections: db_runtime.pool_size,
                    ingress_connections: server_config.ingress.pool_size,
                    acquire_timeout: db_runtime.acquire_timeout,
                };
                let preparation = match identity.preflight(&db_runtime.database_url, limits).await {
                    Ok(preparation) => preparation,
                    Err(error) => {
                        return Err(refused_restricted_start(
                            RestrictedStartupStage::Preflight,
                            error,
                        )
                        .await);
                    }
                };
                info!(
                    transition_id = %preparation.transition_id(),
                    "Restricted runtime pinned read-only preflight passed"
                );
                // The runtime role holds no DDL privilege, so nothing is
                // migrated or provisioned here: the owner phase prepared every
                // schema, and each store attaches to it during startup.
                // A clustered node also holds the control-plane pool that
                // carries node and claim liveness, under the same login.
                let control_plane = server_config.clustering.enabled.then_some(
                    db::bootstrap::ControlPlanePoolLimits {
                        connections: db_runtime.control_plane_pool_size,
                        acquire_timeout: db_runtime.acquire_timeout,
                    },
                );
                let pool = match identity
                    .attach_serving(preparation, &db_runtime.database_url, limits, control_plane)
                    .await
                {
                    Ok(pool) => pool,
                    Err(error) => {
                        return Err(refused_restricted_start(
                            RestrictedStartupStage::ServingPool,
                            error,
                        )
                        .await);
                    }
                };
                info!("Restricted runtime attached its attested serving pool");
                (pool, None, None)
            }
            None => {
                // Owner startup: this credential owns the schema, so it
                // migrates and provisions before serving. SQLite has no role
                // model and always starts this way.
                let mut db_config =
                    db::DatabaseConfig::new(db_runtime.driver, db_runtime.database_url);
                db_config.pool_size = db_runtime.pool_size;
                db_config.acquire_timeout = db_runtime.acquire_timeout;
                if db_runtime.driver == db::DatabaseDriver::Postgres
                    && server_config.clustering.enabled
                {
                    db_config =
                        db_config.with_control_plane_pool(db_runtime.control_plane_pool_size);
                }
                let (pool, matched, outbox) = if db_runtime.driver == db::DatabaseDriver::Postgres {
                    // The PostgreSQL baseline is the schema the restricted
                    // owner phase completes. A server that owns its schema
                    // creates some of the same tables in another shape, so
                    // it keeps the migration history.
                    if db::schema_baseline_selected() {
                        anyhow::bail!(
                            "{} applies to PostgreSQL only through `waddle-server owner-phase` \
                             and a server started with WADDLE_DB_RUNTIME_ATTESTATION",
                            db::BASELINE_ACTION_ENV
                        );
                    }
                    xmpp_config.validate_shared_owner_locations()?;
                    db::bootstrap::bootstrap_shared_owner_postgres(
                        db_config,
                        &lineage_config,
                        server_config.clustering.enabled,
                        server_config.session_key.as_bytes(),
                        db_runtime.pool_size,
                        server_config.ingress.pool_size,
                    )
                    .await
                    .map(|(pool, matched, outbox)| (pool, matched, Some(outbox)))?
                } else {
                    let pool = db::DatabasePool::try_new(db_config, db::PoolConfig).await?;
                    let matched = db::bootstrap::bootstrap_global_owner(
                        pool.global(),
                        &lineage_config,
                        server_config.clustering.enabled,
                    )
                    .await?;
                    (pool, matched, None)
                };
                info!("Database initialized and migrations complete");
                (pool, matched, outbox)
            }
        };

    // Start the server
    let restricted = std::env::var_os("WADDLE_DB_RUNTIME_ATTESTATION").is_some();
    let started = server::start_with_config_and_owner_outbox(
        db_pool,
        xmpp_config,
        server_config,
        lineage_config,
        global_adopt_matched,
        owner_outbox,
        inherited,
    )
    .await;
    let metrics_flush = match started {
        Ok(metrics_flush) => metrics_flush,
        // Under the restricted role a failed store attachment ends the
        // start here: there is no owner startup to fall back to.
        Err(error) if restricted => {
            return Err(refused_restricted_start(RestrictedStartupStage::Startup, error).await);
        }
        Err(error) => return Err(error),
    };

    telemetry::shutdown(metrics_flush);

    Ok(())
}

/// Count a start under the restricted database role that refused to serve,
/// and export the count before the process exits with `error`.
async fn refused_restricted_start(
    stage: RestrictedStartupStage,
    error: impl Into<anyhow::Error>,
) -> anyhow::Error {
    record_restricted_startup_refusal(stage);
    telemetry::shutdown(telemetry::flush_metrics_before_exit().await);
    error.into()
}
