mod acme;
pub(crate) mod caps_resolution;
mod config;
pub(crate) mod dual_registration;
pub(crate) mod durable_membership;
mod extension_commands;
pub mod extension_host_adapter;
mod extension_host_tools;
mod fixed_account;
mod health;
mod http;
pub(crate) mod process_metrics;
pub(crate) mod profile_publish_route;
mod room_registry_gauge;
pub(crate) mod session_janitors;
mod state;
pub(crate) mod state_inventory;
pub(crate) mod state_inventory_metrics;
pub(crate) mod state_inventory_route;
mod topology;
mod trace;
#[cfg(test)]
mod xmpp_account_state;
#[cfg(test)]
mod xmpp_app_state;
#[cfg(test)]
mod xmpp_auth_state;
mod xmpp_channels;
#[cfg(test)]
mod xmpp_permission_state;
#[cfg(test)]
mod xmpp_profile_state;
#[cfg(test)]
mod xmpp_roster_state;
#[cfg(test)]
mod xmpp_space_state;
#[cfg(test)]
mod xmpp_upload_state;
#[cfg(test)]
mod xmpp_user_storage_state;

pub(crate) mod bootstrap_membership;
pub(crate) mod managed_channel_policy;
pub(crate) mod routes;
pub mod xmpp_state;

pub use config::{XmppAcmeConfig, XmppConfig};
pub use state::{resolve_server_owner_jids, AppState, AppStateDeps};

use crate::config::ServerConfig;
use crate::db::DatabasePool;
use crate::inbox::DatabaseInboxStorage;
use crate::permissions::PermissionActor;
use crate::spaces_metadata::DatabaseSpacesMetadataStore;
use acme::start_acme_runtime;
use anyhow::Result;
use fixed_account::{ensure_fixed_test_account, fixed_test_account_enabled};
use http::{create_websocket_mam_storage, start_http_server, HttpServerDeps};
use kameo::actor::Spawn;
use std::{net::SocketAddr, sync::Arc};
use tracing::info;

/// Start both HTTP and XMPP servers with Ecdysis graceful restart support.
///
/// On SIGTERM: graceful drain and exit.
/// On SIGQUIT: re-exec with fd passing, then drain and exit.
pub async fn start(
    db_pool: DatabasePool,
    server_config: ServerConfig,
    lineage_config: crate::config::LineageConfig,
    global_adopt_matched: Option<crate::db::lineage::LineageUuid>,
    inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<crate::telemetry::MetricsFlush> {
    let xmpp_config = XmppConfig::from_env()
        .map_err(|error| anyhow::anyhow!("Failed to load XMPP configuration: {}", error))?;

    start_with_config(
        db_pool,
        xmpp_config,
        server_config,
        lineage_config,
        global_adopt_matched,
        inherited,
    )
    .await
}

/// Start both HTTP and XMPP servers with explicit configuration.
///
/// On graceful exit, returns the outcome of the pre-exit metrics flush
/// so `main` can pass it to `telemetry::shutdown`.
pub async fn start_with_config(
    db_pool: DatabasePool,
    xmpp_config: XmppConfig,
    server_config: ServerConfig,
    lineage_config: crate::config::LineageConfig,
    global_adopt_matched: Option<crate::db::lineage::LineageUuid>,
    mut inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<crate::telemetry::MetricsFlush> {
    // Set up Ecdysis graceful shutdown coordinator
    let shutdown = waddle_ecdysis::GracefulShutdown::from_env();
    let stop_token = shutdown.stop_token();

    // Acquire listeners: inherited from parent process, or bind fresh.
    // Two explicit paths — no silent fallback.
    let http_listener = if let Some(ref mut set) = inherited {
        // Ecdysis restart path: all listeners MUST be inherited
        set.take("http")
    } else {
        // Cold start path: bind listeners fresh
        let http_addr: SocketAddr = std::env::var("WADDLE_HTTP_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:3000".parse().expect("Valid fallback HTTP address"));
        let http = tokio::net::TcpListener::bind(http_addr).await?;
        if let Ok(addr) = http.local_addr() {
            info!(addr = %addr, "Bound HTTP listener");
        } else {
            info!(addr = %http_addr, "Bound HTTP listener");
        }

        http
    };

    // If we inherited, verify we consumed everything
    if let Some(set) = inherited {
        set.assert_empty();
    }

    // Wrap db_pool in Arc for shared ownership between HTTP and XMPP states
    let db_pool = Arc::new(db_pool);
    ensure_fixed_test_account(&db_pool, &xmpp_config).await?;
    let global_db = Arc::new(db_pool.global().clone());
    let permission_actor_impl = if server_config.spicedb.is_none() && fixed_test_account_enabled() {
        PermissionActor::new_for_tests(Arc::clone(&global_db))
    } else {
        PermissionActor::from_server_config(&server_config).await?
    };
    info!(
        backend = permission_actor_impl.backend_name(),
        "Permission backend configured"
    );
    let permission_actor = crate::permissions::PermissionActor::spawn(permission_actor_impl);
    permission_actor
        .ask(crate::permissions::EnsureSchema)
        .await
        .map_err(|error| anyhow::anyhow!("Failed to ensure permission schema: {}", error))?;
    bootstrap_membership::reconcile_existing_accounts_or_warn(
        db_pool.global_actor(),
        &permission_actor,
        &bootstrap_membership::BootstrapMembershipConfig::from_env(),
    )
    .await;
    let inbox_database_storage = Arc::new(
        DatabaseInboxStorage::open(xmpp_config.inbox_database_url.as_deref())
            .await
            .map_err(|error| anyhow::anyhow!("Failed to initialize inbox storage: {}", error))?,
    );
    let inbox_storage: Arc<dyn waddle_xmpp::inbox::storage::InboxStorage> =
        inbox_database_storage.clone();
    let spaces_metadata_database_store = Arc::new(
        DatabaseSpacesMetadataStore::open(xmpp_config.spaces_metadata_database_url.as_deref())
            .await
            .map_err(|error| {
                anyhow::anyhow!("Failed to initialize spaces metadata storage: {}", error)
            })?,
    );
    let spaces_metadata_store: Arc<dyn crate::spaces_metadata::SpacesMetadataStore> =
        spaces_metadata_database_store.clone();
    let channel_space_link_database_store = Arc::new(
        crate::channel_space_links::DatabaseChannelSpaceLinkStore::open(
            xmpp_config.channel_space_links_database_url.as_deref(),
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("Failed to initialize channel-space link storage: {}", error)
        })?,
    );
    let channel_space_link_store: Arc<dyn crate::channel_space_links::ChannelSpaceLinkStore> =
        channel_space_link_database_store.clone();
    // Build the shared XEP-0060 PubSub/PEP storage and the MUC
    // (XEP-0045) room registry here so the resulting handles live on
    // `AppState`. Admin V2 handlers reach them via `state.*` instead of
    // threading per-connection `ProtocolServices` references through
    // every site that mutates rooms or pubsub nodes. The concrete
    // `DatabasePubSubStorage` handle is also passed onward into the
    // HTTP server so the notification-settings projection can read the
    // same database without re-opening it.
    let pubsub_database_storage = crate::pubsub::build_database_pubsub_storage(
        xmpp_config.pubsub_database_url.clone_resolved_url(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("Failed to initialize PubSub storage: {}", error))?;
    let pubsub_storage: Arc<dyn waddle_xmpp::pubsub::PubSubStorage> =
        pubsub_database_storage.clone();
    // #807: spawn the registry behind the instrumented, explicitly-bounded
    // handle (named bounded mailbox + per-request reply timeout + typed errors),
    // and start the periodic mailbox-depth gauge. The shared graph still stores
    // the underlying `ActorRef`, so existing call sites are unchanged.
    // #1135: hydrate every freshly spawned RoomActor's durable inbox
    // recipient set from the permission tuples, so offline channel
    // members keep receiving inbox rows / notification candidates
    // across deploys and actor respawns.
    let durable_membership_source: Arc<dyn waddle_xmpp::muc::affiliation::DurableMembershipSource> =
        Arc::new(durable_membership::PermissionDurableMembershipSource::new(
            permission_actor.clone(),
        ));
    let room_registry_handle = waddle_xmpp::muc::RoomRegistry::spawn(
        xmpp_config.muc_domain.to_string(),
        server_config.occupant_id_secret.clone(),
        Some(durable_membership_source),
    );
    room_registry_gauge::spawn(room_registry_handle.clone(), stop_token.clone());
    let room_registry = room_registry_handle.actor_ref().clone();

    // ADR-0017 Phase 2/3: conditionally start the owned libp2p swarm
    // subsystem (node discovery only) plus the Phase 3 Slice 2 node-lease/
    // self-fencing loop, gated behind `clustering.enabled` + the
    // `clustering` build feature + the Postgres control plane. A no-op when
    // disabled — the default single-replica path is byte-for-byte unchanged,
    // and the typed node lifecycle simply stays serving after startup.
    let node_lifecycle = crate::clustering::NodeLifecycle::starting();
    let lifecycle_on_shutdown = node_lifecycle.clone();
    let stop_token_for_lifecycle = stop_token.clone();
    tokio::spawn(async move {
        stop_token_for_lifecycle.cancelled().await;
        // A critical-service failure has already latched `Failed`; ordinary
        // Ecdysis shutdowns become `Draining` and are intentionally nonfatal.
        lifecycle_on_shutdown.begin_drain();
    });
    // ADR-0017 Phase 3 Slice 10: `clustering_shutdown` is awaited after the
    // HTTP server exits below — see that await's own comment for why this
    // node's graceful per-entity claim drain must complete before process
    // exit, not merely race it in the background.
    let (clustering_handles, clustering_shutdown) = crate::clustering::start_if_enabled(
        &server_config.clustering,
        db_pool.global(),
        &stop_token,
        node_lifecycle.clone(),
    )
    .await?;

    // ADR-0017 Phase 3 Slice 7: wire the real, clustering-backed claim
    // store/identity/durable store into the room registry spawned above —
    // construction-order note: the registry is spawned before
    // `start_if_enabled` runs (it needs no clustering handles to exist),
    // mirroring `local_claims`/`resume_bridge`'s identical fill-in-later
    // cell pattern. A `None` claim pair (clustering disabled, non-Postgres,
    // or a build without the `clustering` feature) leaves the registry's
    // single-node defaults (`InProcessClaimStore`, no durable store)
    // untouched — today's behavior, unchanged.
    #[cfg(feature = "clustering")]
    if let Some((claim_store, node_identity)) = clustering_handles.claim_pair() {
        // ADR-0017 Phase 3 Slice 10: rollout-aware acquire placement for
        // the room re-election path — `None` (no backoff) whenever the
        // node-lease handle itself is unavailable, mirroring every other
        // `clustering_handles.*` optional-field fallback in this block.
        let rollout_backoff = clustering_handles.node_lease.clone().map(|node_lease| {
            Arc::new(crate::clustering::drain::PostgresRolloutBackoff::new(
                node_lease,
                clustering_handles.pod_template_hash.clone(),
            )) as Arc<dyn waddle_xmpp::ownership::RolloutBackoff>
        });
        room_registry_handle
            .wire_clustering_claims(
                claim_store,
                node_identity,
                clustering_handles.muc_durable_store.clone(),
                rollout_backoff,
            )
            .await;
        if let Some(room_local_claims) = &clustering_handles.room_local_claims {
            room_local_claims.wire(room_registry_handle.clone());
        }
    }

    // Create HTTP state (shares db_pool via Arc)
    let blob_storage = crate::storage::build_blob_storage()
        .map_err(|e| anyhow::anyhow!("Failed to initialize blob storage: {}", e))?;
    let server_owner_jids = resolve_server_owner_jids(
        &bootstrap_membership::BootstrapMembershipConfig::from_env(),
        &xmpp_config.domain,
    );
    let state = Arc::new(AppState::new_with_deps(AppStateDeps {
        db_pool: Arc::clone(&db_pool),
        blob_storage,
        inbox_storage: Arc::clone(&inbox_storage),
        spaces_metadata_store: Arc::clone(&spaces_metadata_store),
        channel_space_link_store: Arc::clone(&channel_space_link_store),
        pubsub_storage: Arc::clone(&pubsub_storage),
        room_registry: room_registry.clone(),
        spaces_jid: xmpp_config.spaces_jid.clone(),
        muc_domain: xmpp_config.muc_domain.clone(),
        occupant_id_secret: server_config.occupant_id_secret.clone(),
        permission_actor: permission_actor.clone(),
        server_owner_jids,
        node_lifecycle: node_lifecycle.clone(),
        clustering_claims: clustering_handles,
        lineage_config: lineage_config.clone(),
        clustering_enabled: server_config.clustering.enabled,
    }));
    if let Some(matched) = global_adopt_matched {
        state.note_lineage_adopt_match(matched);
    }
    bootstrap_store_lineage(
        &state,
        crate::db::lineage::DurableStore::Global,
        db_pool.global().clone(),
    )
    .await?;
    state.register_lineage_database(
        crate::db::lineage::DurableStore::Global,
        db_pool.global().clone(),
    );
    if db_pool.global().has_control_plane_pool() {
        state.register_lineage_control_plane(
            crate::db::lineage::DurableStore::ControlPlane,
            db_pool.global().clone(),
        );
    }
    bootstrap_and_register_store_lineage(
        &state,
        crate::db::lineage::DurableStore::Inbox,
        inbox_database_storage.database(),
    )
    .await?;
    bootstrap_and_register_store_lineage(
        &state,
        crate::db::lineage::DurableStore::SpacesMetadata,
        spaces_metadata_database_store.database(),
    )
    .await?;
    bootstrap_and_register_store_lineage(
        &state,
        crate::db::lineage::DurableStore::ChannelSpaceLinks,
        channel_space_link_database_store.database(),
    )
    .await?;
    bootstrap_and_register_store_lineage(
        &state,
        crate::db::lineage::DurableStore::Pubsub,
        pubsub_database_storage.database(),
    )
    .await?;
    // ADR-0017 Phase 3 Slice 7 FIX 1: co-location-check MAM storage against
    // the clustering global database and enable fenced groupchat-archive
    // writes when clustering is live, mirroring the
    // `sm_persistence`/`pending_delivery` `open_for_cluster_mode` gating
    // already established for their own durability stores.
    let websocket_mam_storage = create_websocket_mam_storage(
        xmpp_config.mam_database_url.clone_resolved_url(),
        server_config.clustering.enabled,
        state.clustering_claims.claim_pair().is_some(),
        db_pool.global(),
    )
    .await?;
    bootstrap_mam_lineage(&state, &websocket_mam_storage).await?;
    let acme_runtime = start_acme_runtime(&xmpp_config, stop_token.clone());

    // Start HTTP server
    let http_state = state.clone();
    let http_mam_storage = websocket_mam_storage.clone();
    let http_server_config = server_config.clone();
    let http_xmpp_config = xmpp_config.clone();
    let http_shutdown_handle = shutdown.handle();
    let acme_http01_challenge_service = acme_runtime
        .as_ref()
        .map(|runtime| runtime.http01_challenge_service.clone());
    // Coordinates the Q6 SM-drain task's completion back to the HTTP
    // server's graceful_shutdown closure. The drain task notifies on
    // exit (RAII guard); the HTTP graceful_shutdown awaits it after
    // stop_token cancels so axum doesn't tear down mid-drain.
    let drain_complete = Arc::new(tokio::sync::Notify::new());
    let http_drain_complete = Arc::clone(&drain_complete);
    let http_pubsub_database_storage = Arc::clone(&pubsub_database_storage);
    let http_handle = tokio::spawn(async move {
        start_http_server(HttpServerDeps {
            state: http_state,
            server_config: http_server_config,
            xmpp_config: http_xmpp_config,
            mam_storage: http_mam_storage,
            pubsub_database_storage: http_pubsub_database_storage,
            acme_http01_challenge_service,
            listener: http_listener,
            shutdown_handle: http_shutdown_handle,
            drain_complete: http_drain_complete,
        })
        .await
    });

    info!("TCP XMPP listener disabled; serving WebSocket C2S only");

    // Run the Ecdysis shutdown lifecycle
    let shutdown_handle = tokio::spawn(async move {
        let signal = shutdown
            .run(|| async {
                // SIGQUIT restart: the new process will read the same binary
                // and bind fresh listeners. The old process drains gracefully.
                // True fd-passing requires stopping accept loops first to
                // extract listeners — implemented here as a clean restart.
                info!("SIGQUIT received — new process will start, old process draining");
                // In the future, we could extract listeners from the tasks
                // and call waddle_ecdysis::restart() for zero-gap fd passing.
                // For now, the new process binds fresh (brief listen gap).
            })
            .await;

        info!(signal = ?signal, "Shutdown lifecycle complete");
    });

    // Wait for the HTTP server to fully exit. axum's
    // `graceful_shutdown` closure (in `start_http_server`) is what
    // waits on the SM Q6 drain via `drain_complete.notified()`, so
    // letting `http_handle` drive the exit guarantees the runtime
    // doesn't tear down mid-drain.
    let result = match http_handle.await {
        Ok(Ok(())) => {
            info!("HTTP server stopped (graceful drain complete)");
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(e) => Err(anyhow::anyhow!("HTTP server task failed: {}", e)),
    };
    // ADR-0017 Phase 3 Slice 10: the clustering node-lease loop runs its
    // own per-entity graceful drain (mark draining, seal + batch-release
    // owned `RoomActor` claims) on this same `stop_token` firing, in
    // parallel with the HTTP/SM drain just awaited above — but element 4's
    // drain sequence requires it "completing before process exit," not
    // merely racing shutdown in the background. A small fixed margin atop
    // the configured budget covers this await's own task-exit/logging
    // overhead beyond the drain loop's own internal budget bound; a no-op
    // (returns immediately) when clustering is disabled.
    const CLUSTERING_DRAIN_AWAIT_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);
    clustering_shutdown
        .await_drain(
            server_config.clustering.node_lease.claim_release_budget
                + CLUSTERING_DRAIN_AWAIT_MARGIN,
        )
        .await;

    // Issue #1388: both drains just awaited above (SM/Q6 via `http_handle`,
    // clustering per-entity via `clustering_shutdown`) increment counters and
    // histograms — including the tail-end-only `xmpp.sm.drain_timeout` and
    // `waddle.clustering.drain_duration_ms` — right up to the moment they
    // return. Those increments have no guarantee of a periodic OTLP export
    // tick before process exit, so force-flush the meter provider here, now
    // that every end-of-drain increment has already happened and before
    // anything below can shorten the remaining time budget.
    let metrics_flush = crate::telemetry::flush_metrics_before_exit().await;

    // Tear down the shutdown lifecycle task so we don't dangle.
    // If HTTP exited on its own (error path) before any signal
    // arrived, `shutdown_handle.await` would block on
    // `shutdown.run()` indefinitely.
    shutdown_handle.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), shutdown_handle).await;
    info!("Graceful shutdown complete");
    match node_lifecycle.critical_failure() {
        Some(failure) => Err(anyhow::anyhow!(
            "critical actor service terminated; node fenced and drained: {failure:?}"
        )),
        None => result.map(|()| metrics_flush),
    }
}

/// Bootstrap DDL plus the one-shot operator action for one durable boundary.
///
/// `Adopt` uses the matched-or-skip engine call: several stores usually share
/// one database (one `_lineage` row), so only the boundary whose row holds a
/// listed UUID rotates; everywhere else this is read-only. Matches are
/// recorded on `AppState` so the startup gate can fail loudly when an
/// `adopt=` entry matched no boundary at all.
pub(crate) async fn bootstrap_store_lineage(
    state: &AppState,
    store: crate::db::lineage::DurableStore,
    database: crate::db::Database,
) -> Result<()> {
    let config = &state.lineage_config;
    crate::db::lineage::ensure_table(&database).await?;
    match &config.action {
        Some(crate::db::lineage::LineageAction::Enroll) => {
            info!(store = %store, "enrolling database lineage");
            crate::db::lineage::enroll(&database, config).await?;
        }
        Some(crate::db::lineage::LineageAction::Adopt(expected)) => {
            match crate::db::lineage::adopt_if_matched(&database, config, expected).await? {
                crate::db::lineage::AdoptOutcome::Adopted { matched, .. } => {
                    info!(store = %store, matched = %matched, "adopted database lineage");
                    state.note_lineage_adopt_match(matched);
                }
                crate::db::lineage::AdoptOutcome::NotMatched => {
                    info!(
                        store = %store,
                        "lineage adopt action did not name this boundary; leaving it untouched"
                    );
                }
            }
        }
        None => {}
    }
    Ok(())
}

async fn bootstrap_and_register_store_lineage(
    state: &AppState,
    store: crate::db::lineage::DurableStore,
    database: crate::db::Database,
) -> Result<()> {
    // Ephemeral classification must follow the opened handle's real backend,
    // not env-var presence: a missing per-store URL may still resolve to the
    // durable global `WADDLE_DATABASE_URL`.
    if database.is_in_memory_sqlite() {
        state.register_lineage_ephemeral(store);
    } else {
        bootstrap_store_lineage(state, store, database.clone()).await?;
        state.register_lineage_database(store, database);
    }
    Ok(())
}

async fn bootstrap_mam_lineage(
    state: &AppState,
    storage: &waddle_xmpp::mam::SqlxMamStorage,
) -> Result<()> {
    if let Some(pool) = storage.sqlite_pool() {
        let in_memory = crate::db::lineage::sqlite_pool_is_in_memory(pool).await?;
        let database = crate::db::Database::from_sqlite_pool("mam", pool.clone(), in_memory);
        if in_memory {
            state.register_lineage_ephemeral(crate::db::lineage::DurableStore::Mam);
        } else {
            bootstrap_store_lineage(state, crate::db::lineage::DurableStore::Mam, database).await?;
            state.register_lineage_probe(
                crate::db::lineage::DurableStore::Mam,
                Arc::new(crate::db::lineage::SqlxLineageAttestor::from_sqlite(
                    pool.clone(),
                )),
            );
        }
        return Ok(());
    }
    if let Some(pool) = storage.postgres_pool() {
        let database = crate::db::Database::from_postgres_pool("mam", pool.clone());
        bootstrap_store_lineage(state, crate::db::lineage::DurableStore::Mam, database).await?;
        state.register_lineage_probe(
            crate::db::lineage::DurableStore::Mam,
            Arc::new(crate::db::lineage::SqlxLineageAttestor::from_postgres(
                pool.clone(),
            )),
        );
        return Ok(());
    }
    anyhow::bail!("MAM storage did not expose a lineage backend")
}

#[cfg(test)]
mod tests;
