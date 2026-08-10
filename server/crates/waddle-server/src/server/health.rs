use crate::{
    db::{
        lineage::{LineageReport, LineageStatus},
        PoolHealth,
    },
    server::AppState,
};
use axum::{
    extract::State,
    http::{header, HeaderName, Method, StatusCode},
    response::{IntoResponse, Json},
};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

const LINEAGE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
/// Per-boundary probe budget, deliberately inside the joint deadline so a
/// single slow pool reports as that store's typed `probe_timeout` (and the
/// sticky path can keep proven boundaries) rather than the whole request
/// hitting the outer deadline.
const LINEAGE_PER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1200);

/// Configure CORS layer.
///
/// If `WADDLE_CORS_ORIGINS` is set (comma-separated list of origins),
/// only those origins are allowed. Otherwise, falls back to permissive
/// CORS (suitable for development).
pub(crate) fn configure_cors() -> CorsLayer {
    let origins = std::env::var("WADDLE_CORS_ORIGINS").ok();
    build_cors(origins.as_deref())
}

pub(crate) fn build_cors(origins: Option<&str>) -> CorsLayer {
    use tower_http::cors::AllowOrigin;

    match origins {
        Some(origins) if !origins.is_empty() => {
            let allowed: Vec<_> = origins
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            if allowed.is_empty() {
                warn!(
                    "WADDLE_CORS_ORIGINS set but no valid origins parsed, falling back to permissive CORS"
                );
                CorsLayer::permissive()
            } else {
                info!(origins = ?allowed, "Configured CORS with explicit allowed origins");
                CorsLayer::new()
                    .allow_origin(AllowOrigin::list(allowed))
                    .allow_methods([
                        Method::GET,
                        Method::POST,
                        Method::PUT,
                        Method::PATCH,
                        Method::DELETE,
                        Method::OPTIONS,
                    ])
                    .allow_headers([
                        header::ACCEPT,
                        header::AUTHORIZATION,
                        header::CONTENT_TYPE,
                        header::ORIGIN,
                        // W3C Trace Context — browsers won't send these
                        // cross-origin unless explicitly allowed, which
                        // would silently break end-to-end traces from
                        // the chat frontend into the server.
                        HeaderName::from_static("traceparent"),
                        HeaderName::from_static("tracestate"),
                        HeaderName::from_static("baggage"),
                        HeaderName::from_static("x-waddle-session-id"),
                    ])
                    .allow_credentials(true)
            }
        }
        _ => CorsLayer::permissive(),
    }
}

/// Response for detailed health check
#[derive(Debug, Serialize)]
struct DetailedHealthResponse {
    status: String,
    service: String,
    version: String,
    license: String,
    database: DatabaseHealthStatus,
}

#[derive(Debug, Serialize)]
struct DatabaseHealthStatus {
    status: String,
    global_healthy: bool,
    waddle_dbs_healthy: bool,
    loaded_waddle_count: usize,
}

impl From<PoolHealth> for DatabaseHealthStatus {
    fn from(health: PoolHealth) -> Self {
        Self {
            status: if health.is_healthy() {
                "healthy"
            } else {
                "unhealthy"
            }
            .to_string(),
            global_healthy: health.global_healthy,
            waddle_dbs_healthy: health.waddle_dbs_healthy,
            loaded_waddle_count: health.loaded_waddle_count,
        }
    }
}

/// Simple health check endpoint (for load balancers)
pub(crate) async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Quick health check - just verify the global DB is accessible
    match state.db_pool.global().health_check().await {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({
                "status": "healthy",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "license": "AGPL-3.0"
            })),
        ),
        Ok(false) => {
            warn!("Health check: database unhealthy");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unhealthy",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "error": "database unhealthy"
                })),
            )
        }
        Err(e) => {
            warn!("Health check failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unhealthy",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "error": format!("database error: {}", e)
                })),
            )
        }
    }
}

async fn health_for_serving_generation<T>(
    lifecycle: &crate::clustering::NodeLifecycle,
    health_check: impl std::future::Future<Output = T>,
) -> Result<T, crate::clustering::NodeAdmissionError> {
    let permit = lifecycle.admit()?;
    let health = health_check.await;
    permit.revalidate()?;
    Ok(health)
}

/// A node held out of `Serving` — including one held there by a failed
/// startup lineage attestation — must still tell the operator *why*: the
/// lifecycle admission state alone reads as a generic "Starting", so the
/// live per-store lineage statuses are attached whenever they are not clean.
fn lifecycle_not_ready_response(
    lifecycle: &crate::clustering::NodeLifecycle,
    error: &crate::clustering::NodeAdmissionError,
    lineage: Result<LineageReport, LineageStatus>,
) -> (StatusCode, Json<serde_json::Value>) {
    let admission = lifecycle.admission();
    warn!(?admission, %error, "Readiness check: node is not admitting clients");
    let lineage_value = lineage_json(lineage);
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "status": "not_ready",
            "service": "waddle-server",
            "version": env!("CARGO_PKG_VERSION"),
            "admission": format!("{admission:?}"),
            "lineage": lineage_value
        })),
    )
}

/// Readiness check endpoint (for orchestrators).
///
/// Readiness is stricter than liveness and validates overall DB pool health
/// **and** the ADR-0017 Phase 3 Slice 2 clustering readiness signal: a node
/// that has self-fenced its entity-ownership claims (node-lease heartbeat
/// lost, or Postgres unreachable past the lease deadline) must not stay in
/// the client Service/Ingress endpoint set — clients whose sockets it just
/// closed would otherwise be routed straight back to the still-refusing
/// node. The same lifecycle also fails closed when a critical local actor
/// terminates in a single-node deployment.
pub(crate) async fn readiness_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // One deadline bounds the WHOLE readiness body — pool health and lineage
    // probes run concurrently inside it — so a stalled pool acquisition
    // cannot eat the kubelet's 2s budget and surface as an untyped
    // client-side probe timeout.
    let readiness = async {
        tokio::time::timeout(LINEAGE_PROBE_TIMEOUT, async {
            tokio::join!(state.db_pool.health_check(), lineage_readiness(&state))
        })
        .await
    };
    match health_for_serving_generation(&state.node_lifecycle, readiness).await {
        Err(error) => {
            // A not-Serving node reports the stored LATCHING report (live
            // drift or startup-definitive failure) when one exists, else the
            // startup attestation outcome: free, cannot double the request's
            // deadline, and names the failure that actually took the node
            // down.
            let lineage = state
                .lineage_latched
                .get()
                .or_else(|| state.lineage_startup.get())
                .cloned()
                .ok_or(LineageStatus::Initializing);
            lifecycle_not_ready_response(&state.node_lifecycle, &error, lineage)
        }
        Ok(Err(_)) => {
            warn!("Readiness check: deadline exceeded before health/lineage completed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "database": { "status": "timeout" },
                    "lineage": { "status": LineageStatus::ProbeTimeout.as_str() }
                })),
            )
        }
        Ok(Ok((Ok(health), Ok(report)))) if health.is_healthy() && report.is_attested() => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "database": "ready",
                "lineage": "attested"
            })),
        ),
        Ok(Ok((Ok(health), Ok(report)))) if health.is_healthy() => {
            lineage_not_ready_response(report)
        }
        Ok(Ok((Ok(_), Err(status)))) => lineage_status_response(status),
        Ok(Ok((Ok(health), lineage))) => {
            warn!(
                global_healthy = health.global_healthy,
                waddle_dbs_healthy = health.waddle_dbs_healthy,
                loaded_waddle_count = health.loaded_waddle_count,
                "Readiness check: database pool not fully ready"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "database": {
                        "status": "not_ready",
                        "global_healthy": health.global_healthy,
                        "waddle_dbs_healthy": health.waddle_dbs_healthy,
                        "loaded_waddle_count": health.loaded_waddle_count
                    },
                    "lineage": lineage_json(lineage)
                })),
            )
        }
        Ok(Ok((Err(e), lineage))) => {
            warn!(error = %e, "Readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "database": {
                        "status": format!("error: {}", e)
                    },
                    "lineage": lineage_json(lineage)
                })),
            )
        }
    }
}

async fn lineage_readiness(state: &AppState) -> Result<LineageReport, LineageStatus> {
    let Some(registry) = state.lineage_registry.get() else {
        return Err(LineageStatus::Initializing);
    };
    let report = registry
        .attest(
            &state.lineage_config,
            state.clustering_enabled,
            LINEAGE_PER_PROBE_TIMEOUT,
        )
        .await;
    // Live drift fails CLOSED, not just unready: a definitive lineage
    // failure appearing while this node is Serving (database restored or
    // replaced under the running process) latches the lifecycle — demoting
    // the node and revoking its serving generation so existing WebSocket
    // sessions and direct upgrades stop writing through the wrong database.
    // Transport-only failures never latch (sticky-success already absorbs
    // them on proven boundaries).
    // Keyed on "startup attested" rather than "currently Serving": a lease
    // self-fence can win the race mid-probe, and a definitive mismatch
    // observed on a fenced-recovering (or draining) node must still latch —
    // otherwise recovery would re-serve against the drifted database until
    // the next probe.
    if !report.is_attested()
        && !report.is_transient_only()
        && !state.node_lifecycle.startup_blocked()
        && state
            .lineage_startup
            .get()
            .is_some_and(LineageReport::is_attested)
    {
        for (store, status) in report.failures() {
            tracing::error!(
                store = %store,
                status = status.as_str(),
                "definitive lineage failure on a serving node; latching and revoking admission"
            );
        }
        let _ = state.lineage_latched.set(report.clone());
        state.node_lifecycle.latch_startup_block();
        // The latch cut client admission, but the janitor fleet (not all of
        // which consults the lifecycle) would keep mutating the replaced
        // database. A database swapped under a running node is a controlled
        // crash: give the 503 response and logs a moment to flush, then
        // exit — the next boot's pre-migration guard refuses the foreign
        // database with zero writes. Compiled out of unit-test builds
        // (which run the handler in-process); the spawned-binary
        // integration tests exercise the real exit.
        #[cfg(not(test))]
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            tracing::error!(
                "exiting: definitive lineage drift detected on a serving node (see prior errors)"
            );
            std::process::exit(1);
        });
    }
    Ok(report)
}

fn lineage_json(lineage: Result<LineageReport, LineageStatus>) -> serde_json::Value {
    match lineage {
        Ok(report) if report.is_attested() => serde_json::Value::String("attested".to_string()),
        Ok(report) => serde_json::Value::Object(
            report
                .failures()
                .iter()
                .map(|(store, status)| {
                    (
                        store.to_string(),
                        serde_json::Value::String(status.as_str().to_string()),
                    )
                })
                .collect(),
        ),
        Err(status) => json!({ "status": status.as_str() }),
    }
}

fn lineage_not_ready_response(report: LineageReport) -> (StatusCode, Json<serde_json::Value>) {
    let statuses = report
        .failures()
        .iter()
        .map(|(store, status)| {
            (
                store.to_string(),
                serde_json::Value::String(status.as_str().to_string()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "status": "not_ready",
            "service": "waddle-server",
            "version": env!("CARGO_PKG_VERSION"),
            "lineage": statuses
        })),
    )
}

fn lineage_status_response(status: LineageStatus) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "status": "not_ready",
            "service": "waddle-server",
            "version": env!("CARGO_PKG_VERSION"),
            "lineage": { "status": status.as_str() }
        })),
    )
}

/// Prometheus metrics endpoint.
pub(crate) async fn metrics_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        waddle_xmpp::prometheus::render_metrics(),
    )
}

/// Detailed health check endpoint (for monitoring)
pub(crate) async fn detailed_health_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.db_pool.health_check().await {
        Ok(health) => {
            let status = if health.is_healthy() {
                "healthy"
            } else {
                "degraded"
            };
            let status_code = if health.is_healthy() {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };

            (
                status_code,
                Json(DetailedHealthResponse {
                    status: status.to_string(),
                    service: "waddle-server".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    license: "AGPL-3.0".to_string(),
                    database: health.into(),
                }),
            )
        }
        Err(e) => {
            warn!("Detailed health check failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(DetailedHealthResponse {
                    status: "unhealthy".to_string(),
                    service: "waddle-server".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    license: "AGPL-3.0".to_string(),
                    database: DatabaseHealthStatus {
                        status: format!("error: {}", e),
                        global_healthy: false,
                        waddle_dbs_healthy: false,
                        loaded_waddle_count: 0,
                    },
                }),
            )
        }
    }
}

#[cfg(test)]
mod readiness_generation_tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{body::Body, response::Response};
    use http_body_util::BodyExt;
    use kameo::actor::Spawn;
    use std::{str::FromStr, sync::Arc};

    use crate::{
        config::LineageConfig,
        db::{
            lineage::{
                enroll, ensure_table, AttestedLineage, DatabaseLineageAttestor, DeploymentUuid,
                DurableStore, LineageAttestor, LineageRegistryBuilder,
            },
            Database, DatabaseConfig, DatabaseDriver, DatabasePool, PoolConfig,
        },
        permissions::PermissionActor,
        server::{AppState, AppStateDeps},
    };
    use waddle_xmpp::{pubsub::InMemoryPubSubStorage, xep::xep0421::OccupantIdSecret};

    fn configured_lineage() -> LineageConfig {
        LineageConfig {
            deployment_uuid: Some(
                DeploymentUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90001")
                    .expect("valid deployment UUID"),
            ),
            action: None,
        }
    }

    async fn response_json(response: Response<Body>) -> serde_json::Value {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        serde_json::from_slice(&body).expect("response JSON")
    }

    async fn sqlite_pool(name: &str) -> Arc<DatabasePool> {
        let config = DatabaseConfig::new(
            DatabaseDriver::Sqlite,
            format!("sqlite:file:{name}?mode=memory&cache=shared"),
        );
        Arc::new(
            DatabasePool::new(config, PoolConfig)
                .await
                .expect("sqlite pool"),
        )
    }

    struct PostgresFixture {
        db: Database,
        admin: sqlx::PgPool,
        schema: String,
    }

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse postgres url");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }

    async fn postgres_fixture(prefix: &str) -> Option<PostgresFixture> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (readiness PostgreSQL colocation test)"
            );
            return None;
        };
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        let schema = format!(
            "waddle_test_readiness_{prefix}_{}",
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");
        let db = Database::from_config(
            "readiness-postgres-test",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                postgres_url_with_search_path(&database_url, &schema),
            ),
        )
        .await
        .expect("open isolated postgres database");
        Some(PostgresFixture { db, admin, schema })
    }

    async fn drop_postgres_fixture(fixture: PostgresFixture) {
        let PostgresFixture { db, admin, schema } = fixture;
        drop(db);
        sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated postgres schema");
    }

    fn test_state(db_pool: Arc<DatabasePool>, clustering_enabled: bool) -> AppState {
        use jid::{BareJid, DomainPart};

        let blob_storage = crate::storage::build_blob_storage()
            .unwrap_or_else(|error| panic!("failed to initialize blob storage: {error}"));
        let permission_actor = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
            db_pool.global().clone(),
        )));
        let muc_domain =
            DomainPart::from_str("muc.localhost").expect("test MUC domain parses as DomainPart");
        let occupant_id_secret =
            OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
                .expect("test occupant-id secret meets length floor");
        let room_registry = waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::spawn(
            waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::new(
                muc_domain.to_string(),
                occupant_id_secret.clone(),
            ),
        );
        let spaces_jid: BareJid = "spaces.localhost"
            .parse()
            .expect("test spaces JID parses as BareJid");

        AppState::new_with_deps(AppStateDeps {
            db_pool,
            blob_storage,
            inbox_storage: Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
            spaces_metadata_store: Arc::new(
                crate::spaces_metadata::InMemorySpacesMetadataStore::new(),
            ),
            channel_space_link_store: Arc::new(
                crate::channel_space_links::InMemoryChannelSpaceLinkStore::new(),
            ),
            pubsub_storage: Arc::new(InMemoryPubSubStorage::new()),
            room_registry,
            spaces_jid,
            muc_domain,
            occupant_id_secret,
            permission_actor,
            server_owner_jids: Arc::from(Vec::new()),
            node_lifecycle: crate::clustering::NodeLifecycle::new(),
            clustering_claims: crate::clustering::ClusteringHandles::default(),
            lineage_config: configured_lineage(),
            clustering_enabled,
        })
    }

    struct GateAttestor {
        started_tx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        result: AttestedLineage,
    }

    impl GateAttestor {
        fn success(
            started_tx: tokio::sync::oneshot::Sender<()>,
            release_rx: tokio::sync::oneshot::Receiver<()>,
        ) -> Self {
            Self {
                started_tx: tokio::sync::Mutex::new(Some(started_tx)),
                release_rx: tokio::sync::Mutex::new(Some(release_rx)),
                result: AttestedLineage {
                    lineage_uuid: crate::db::lineage::LineageUuid::new(),
                    deployment_uuid: configured_lineage()
                        .deployment_uuid
                        .expect("configured deployment UUID"),
                    postgres_identity: None,
                },
            }
        }
    }

    #[async_trait]
    impl LineageAttestor for GateAttestor {
        async fn attest(
            &self,
            _config: &LineageConfig,
        ) -> Result<AttestedLineage, crate::db::DatabaseError> {
            if let Some(started_tx) = self.started_tx.lock().await.take() {
                let _ = started_tx.send(());
            }
            if let Some(release_rx) = self.release_rx.lock().await.take() {
                let _ = release_rx.await;
            }
            Ok(self.result.clone())
        }

        fn driver(&self) -> DatabaseDriver {
            DatabaseDriver::Sqlite
        }
    }

    async fn assert_transition_during_health_returns_unavailable(
        transition: impl FnOnce(&crate::clustering::NodeLifecycle),
    ) -> crate::clustering::NodeAdmissionError {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let worker_lifecycle = lifecycle.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            health_for_serving_generation(&worker_lifecycle, async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                true
            })
            .await
        });

        started_rx.await.expect("health check started");
        transition(&lifecycle);
        let _ = release_tx.send(());
        let error = worker
            .await
            .expect("health race task")
            .expect_err("changed serving generation must not become ready");

        assert_eq!(
            lifecycle_not_ready_response(&lifecycle, &error, Err(LineageStatus::Initializing))
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        error
    }

    #[tokio::test]
    async fn fence_during_database_health_cannot_return_ready() {
        let _ = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.begin_fenced_recovery();
        })
        .await;
    }

    #[tokio::test]
    async fn critical_failure_during_database_health_cannot_return_ready() {
        let _ = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
        })
        .await;
    }

    #[tokio::test]
    async fn fence_and_recovery_during_database_health_revokes_old_readiness_probe() {
        let error = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.begin_fenced_recovery();
            lifecycle.serve();
            assert_eq!(
                lifecycle.admission(),
                crate::clustering::NodeAdmission::Serving
            );
        })
        .await;
        assert_eq!(error, crate::clustering::NodeAdmissionError::Revoked);
    }

    #[tokio::test]
    async fn healthy_database_in_same_serving_generation_is_ready() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        assert!(health_for_serving_generation(&lifecycle, async { true })
            .await
            .expect("serving generation"));
    }

    #[tokio::test]
    async fn readiness_returns_initializing_before_lineage_registry_is_sealed() {
        let state = Arc::new(test_state(
            sqlite_pool("readiness-initializing").await,
            false,
        ));

        let response = readiness_handler(State(state)).await.into_response();
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["status"], "initializing");
    }

    #[tokio::test]
    async fn readiness_returns_attested_when_database_and_lineage_are_ready() {
        let db = Database::in_memory("readiness-attested")
            .await
            .expect("open sqlite database");
        enroll(&db, &configured_lineage())
            .await
            .expect("enroll lineage");
        let pool = sqlite_pool("readiness-attested-global").await;
        let state = test_state(pool, false);
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Global,
            Arc::new(DatabaseLineageAttestor::new(db)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let json = response_json(response).await;

        assert_eq!(json["status"], "ready");
        assert_eq!(json["lineage"], "attested");
    }

    #[tokio::test]
    async fn readiness_reports_missing_lineage_per_store() {
        let db = Database::in_memory("readiness-missing-lineage")
            .await
            .expect("open sqlite database");
        ensure_table(&db).await.expect("bootstrap lineage table");
        let pool = sqlite_pool("readiness-missing-lineage-global").await;
        let state = test_state(pool, false);
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Inbox,
            Arc::new(DatabaseLineageAttestor::new(db)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["inbox"], "missing_lineage");
    }

    #[tokio::test]
    async fn definitive_live_failure_latches_and_demotes_a_serving_node() {
        let db = Database::in_memory("readiness-live-drift")
            .await
            .expect("open sqlite database");
        ensure_table(&db).await.expect("bootstrap lineage table");
        let pool = sqlite_pool("readiness-live-drift-global").await;
        let state = Arc::new(test_state(pool, false));
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Inbox,
            Arc::new(DatabaseLineageAttestor::new(db)),
        );
        let _ = state.lineage_registry.set(registry.seal());
        // Production invariant the latch keys on: a serving node's startup
        // attestation passed.
        let _ = state.lineage_startup.set(LineageReport::default());
        assert!(state.node_lifecycle.is_ready());

        let response = readiness_handler(State(Arc::clone(&state)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // The definitive failure latched the lifecycle: the node is demoted
        // out of Serving (sockets revoked) and can never be re-promoted.
        assert!(!state.node_lifecycle.is_ready());
        assert!(state.node_lifecycle.startup_blocked());
        state.node_lifecycle.serve();
        assert!(!state.node_lifecycle.is_ready());

        // Subsequent probes keep reporting the latching failure.
        let response = readiness_handler(State(Arc::clone(&state)))
            .await
            .into_response();
        let json = response_json(response).await;
        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["inbox"], "missing_lineage");
    }

    #[tokio::test]
    async fn readiness_rejects_clustered_sqlite_store() {
        let db = Database::in_memory("readiness-clustered-sqlite")
            .await
            .expect("open sqlite database");
        enroll(&db, &configured_lineage())
            .await
            .expect("enroll lineage");
        let pool = sqlite_pool("readiness-clustered-sqlite-global").await;
        let state = test_state(pool, true);
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Mam,
            Arc::new(DatabaseLineageAttestor::new(db)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["mam"], "clustered_sqlite");
    }

    #[tokio::test]
    async fn readiness_rejects_deployment_uuid_mismatch() {
        let db = Database::in_memory("readiness-deployment-mismatch")
            .await
            .expect("open sqlite database");
        enroll(&db, &configured_lineage())
            .await
            .expect("enroll lineage");
        let pool = sqlite_pool("readiness-deployment-mismatch-global").await;
        let mut state = test_state(pool, false);
        state.lineage_config = LineageConfig {
            deployment_uuid: Some(
                DeploymentUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90002")
                    .expect("valid mismatch UUID"),
            ),
            action: None,
        };
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Global,
            Arc::new(DatabaseLineageAttestor::new(db)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["global"], "deployment_uuid_mismatch");
    }

    #[tokio::test]
    async fn readiness_returns_probe_timeout_when_attestor_is_slow() {
        let pool = sqlite_pool("readiness-probe-timeout").await;
        let state = test_state(pool, false);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Global,
            Arc::new(GateAttestor::success(started_tx, release_rx)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        let _ = started_rx.await;
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["global"], "probe_timeout");
    }

    #[tokio::test]
    async fn readiness_revokes_probe_when_lifecycle_changes_during_lineage_attestation() {
        let pool = sqlite_pool("readiness-lifecycle-revoked").await;
        let state = Arc::new(test_state(pool, false));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut registry = LineageRegistryBuilder::default();
        registry.register_probe(
            DurableStore::Global,
            Arc::new(GateAttestor::success(started_tx, release_rx)),
        );
        let _ = state.lineage_registry.set(registry.seal());

        let worker_state = Arc::clone(&state);
        let readiness =
            tokio::spawn(
                async move { readiness_handler(State(worker_state)).await.into_response() },
            );

        started_rx.await.expect("lineage probe started");
        state.node_lifecycle.begin_fenced_recovery();
        let _ = release_tx.send(());
        let response = readiness.await.expect("readiness task");
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["admission"], "FencedRecovering");
    }

    #[tokio::test]
    async fn readiness_reports_colocation_mismatch_for_distinct_postgres_schemas() {
        let Some(global) = postgres_fixture("global").await else {
            return;
        };
        let Some(mam) = postgres_fixture("mam").await else {
            drop_postgres_fixture(global).await;
            return;
        };
        enroll(&global.db, &configured_lineage())
            .await
            .expect("enroll global lineage");
        enroll(&mam.db, &configured_lineage())
            .await
            .expect("enroll MAM lineage");

        let pool = sqlite_pool("readiness-colocation-mismatch").await;
        let state = test_state(pool, true);
        let mut registry = LineageRegistryBuilder::default();
        registry.register_database(DurableStore::Global, global.db.clone());
        registry.register_database(DurableStore::Mam, mam.db.clone());
        let _ = state.lineage_registry.set(registry.seal());

        let response = readiness_handler(State(Arc::new(state)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = response_json(response).await;

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["lineage"]["mam"], "colocation_mismatch");

        drop_postgres_fixture(mam).await;
        drop_postgres_fixture(global).await;
    }
}
