//! Implementation behind the parent module facade.

use super::bootstrap_membership;
use super::durable_membership;
use super::http;
use super::info;
use super::room_registry_gauge;
use super::session_janitors;
use super::start_acme_runtime;
use super::Arc;
use super::DatabasePool;
use super::DatabaseSpacesMetadataStore;
use super::PermissionActor;
use super::ResolvedXmppDatabaseUrl;
use super::Result;
use super::ServerConfig;
use super::SocketAddr;
use super::Spawn;
use super::XmppConfig;
use super::{ensure_fixed_test_account, fixed_test_account_enabled};
use super::{resolve_server_owner_jids, AppState, AppStateDeps};
use super::{select_websocket_mam_storage, start_http_server, HttpServerDeps, MamStartupStorage};

/// Longest startup waits for the first load of the host-grant enrolled set.
/// The load is retried every second meanwhile; a replica that cannot load
/// it exits, like one whose database is unreachable, so the orchestrator's
/// restart backoff retries a full startup instead of serving rooms whose
/// management it cannot tell.
const HOST_GRANT_ENROLLED_SET_STARTUP_WAIT: std::time::Duration =
    std::time::Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum RestrictedStartupError {
    #[error("the restricted runtime attaches its own notification outbox; an owner-prepared handle was supplied")]
    OwnerOutboxSupplied,
}

/// Start HTTP and XMPP services with inherited listeners and graceful shutdown.
///
/// On SIGTERM: graceful drain and exit.
/// On SIGQUIT: record restart intent, then drain and exit.
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
    inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<crate::telemetry::MetricsFlush> {
    start_with_config_and_owner_outbox(
        db_pool,
        xmpp_config,
        server_config,
        lineage_config,
        global_adopt_matched,
        None,
        inherited,
    )
    .await
}

/// Start with the outbox prepared on this exact owner pool before lane seal.
pub async fn start_with_config_and_owner_outbox(
    db_pool: DatabasePool,
    xmpp_config: XmppConfig,
    server_config: ServerConfig,
    lineage_config: crate::config::LineageConfig,
    global_adopt_matched: Option<crate::db::lineage::LineageUuid>,
    owner_outbox: Option<crate::notification_outbox::NotificationOutboxStore>,
    mut inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<crate::telemetry::MetricsFlush> {
    if db_pool.global().uses_attested_postgres_pool() {
        // The restricted runtime role holds no DDL privilege. Every store
        // below, including the clustering stores, attaches to relations the
        // owner phase prepared.
        //
        // An owner-prepared outbox handle belongs to an owner pool. This
        // role attaches the outbox itself on its attested main lane.
        if owner_outbox.is_some() {
            return Err(RestrictedStartupError::OwnerOutboxSupplied.into());
        }
    } else {
        let shared_owner = db_pool.global().ingress_pool_size().is_some();
        let matching_outbox = owner_outbox
            .as_ref()
            .is_some_and(|outbox| outbox.belongs_to_shared_owner_main(db_pool.global()));
        if (shared_owner && !matching_outbox) || (!shared_owner && owner_outbox.is_some()) {
            anyhow::bail!(
                "shared owner startup requires its prepared outbox on the exact main pool"
            );
        }
    }
    // Set up Ecdysis graceful shutdown coordinator
    let shutdown = waddle_ecdysis::GracefulShutdown::from_env();
    let stop_token = shutdown.stop_token();
    let sm_drain_complete = tokio_util::sync::CancellationToken::new();
    let connections_drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // #2046 W1.10: one enclosing shutdown deadline, started by the first
    // observer of SIGTERM/SIGQUIT. HTTP/Q6 and normal process-triggered
    // clustering shutdown derive their limits from it. The outer clustering
    // await also uses it when a terminal fence was already pending before
    // process stop. The chart's grace-period floor adds the same terms.
    let shutdown_deadline = crate::shutdown_deadline::ShutdownDeadline::new(
        crate::shutdown_deadline::ShutdownBudget::new(
            session_janitors::max_drain_duration_from_env(),
            server_config.clustering.node_lease.claim_release_budget,
        ),
    );

    // S2S is off unless configured. Its snapshot, with every credential,
    // trust profile and limit, is loaded and validated before any S2S
    // socket exists; while it is off nothing S2S is read, bound or dialled.
    let s2s_config = crate::s2s::conn::load_enabled_config(&xmpp_config).await?;
    if let Some(config) = &s2s_config {
        check_trusted_homes(config, &xmpp_config)?;
    }

    // Acquire listeners: inherited from parent process, or bind fresh.
    // Two explicit paths — no silent fallback.
    let mut s2s_listener = None;
    let http_listener = if let Some(ref mut set) = inherited {
        // Launcher inheritance path: all listeners MUST be inherited. The
        // `s2s` listener is taken only when S2S listens, and only when it
        // is bound to the configured `listen` address, which must be
        // loopback unless the peer scope is public; an inherited `s2s`
        // descriptor without `listen` is left over and fails startup below.
        let s2s_listen = s2s_config
            .as_ref()
            .and_then(|config| config.listen().map(|address| (config, address)));
        if let Some((config, address)) = s2s_listen {
            let listener = set.take(crate::s2s::conn::S2S_LISTENER_NAME);
            crate::s2s::conn::check_inherited_listener_in(
                address,
                listener.local_addr()?,
                config.admission().scope(),
            )?;
            s2s_listener = Some(listener);
        }
        set.take("http")
    } else {
        if let Some(address) = s2s_config.as_ref().and_then(|config| config.listen()) {
            let listener = tokio::net::TcpListener::bind(address).await?;
            info!(addr = ?listener.local_addr().ok(), "Bound S2S listener");
            s2s_listener = Some(listener);
        }
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
    if db_pool.global().uses_attested_postgres_pool() {
        crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore::attach_existing(
            db_pool.global().clone(),
        )
        .await
        .inspect_err(|_| {
            waddle_xmpp::telemetry::authority::record_restricted_attach_failure(
                waddle_xmpp::telemetry::attributes::RestrictedStore::MucDestroyOutbox,
            )
        })
        .map_err(|error| {
            anyhow::anyhow!("failed to attach MUC destroy completion outbox: {error}")
        })?;
    } else {
        crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore::try_new(
            db_pool.global().clone(),
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to initialize MUC destroy completion outbox: {error}")
        })?;
    }
    ensure_fixed_test_account(&db_pool, &xmpp_config).await?;
    let global_db = Arc::new(db_pool.global().clone());
    let permission_actor_impl = if server_config.spicedb.is_none() && fixed_test_account_enabled() {
        PermissionActor::new_for_tests(Arc::clone(&global_db))
    } else {
        PermissionActor::from_server_config(&server_config).await?
    };
    // PostgreSQL holds the canonical grants of enrolled host-grant scopes.
    // Legacy tuple writes to their objects are refused from that store, so
    // the refusal survives a restart; SQLite holds no such scope. Legacy
    // reads and the host-grant gate decide whether an object is enrolled
    // from one in-memory enrolled set, which the replica loads before it
    // serves and refreshes in the background.
    let host_grant_store =
        crate::permissions::host_grant::HostGrantStore::open((*global_db).clone()).ok();
    let host_grants = match &host_grant_store {
        Some(store) => {
            let enrolled = crate::permissions::host_grant::EnrolledScopes::watch(store.clone());
            if !enrolled
                .wait_loaded(HOST_GRANT_ENROLLED_SET_STARTUP_WAIT)
                .await
            {
                return Err(anyhow::anyhow!(
                    "the host-grant enrolled set could not be loaded; refusing to serve until it is"
                ));
            }
            crate::permissions::host_grant::HostGrantGate::with_enrolled_scopes(
                store.clone(),
                enrolled,
            )
        }
        None => crate::permissions::host_grant::HostGrantGate::default(),
    };
    let permission_actor_impl = match &host_grant_store {
        Some(store) => permission_actor_impl
            .with_host_grant_authority(store.clone(), host_grants.enrolled().clone()),
        None => permission_actor_impl,
    };
    // The privileged relations no writer names (the channel manager and
    // moderator, the Space moderator and the role manager) are written only
    // for accounts of the served domain and of the trusted homes of the S2S
    // snapshot this replica runs (ADR-0020 D8).
    let permission_actor_impl = permission_actor_impl.with_privileged_issuance(
        replica_privileged_issuance(&xmpp_config, s2s_config.as_deref()),
    );
    info!(
        backend = permission_actor_impl.backend_name(),
        "Permission backend configured"
    );
    let permission_actor = crate::permissions::PermissionActor::spawn(permission_actor_impl);
    permission_actor
        .ask(crate::permissions::EnsureSchema)
        .await
        .map_err(|error| anyhow::anyhow!("Failed to ensure permission schema: {}", error))?;
    // Refuse to serve an authorization store written by another ID encoding;
    // see `permissions::authz_incarnation`. The error names no identifiers.
    permission_actor
        .ask(crate::permissions::EnsureAuthzCodecIncarnation)
        .await
        .map_err(|error| {
            anyhow::anyhow!("Authorization store is not usable with this server: {error}")
        })?;
    // Record the schema version only in a store the codec guard accepted, so
    // a refused store gains no version marker; see
    // `permissions::authz_schema_version`.
    permission_actor
        .ask(crate::permissions::RecordAuthzSchemaVersion)
        .await
        .map_err(|error| {
            anyhow::anyhow!("Failed to record the permission schema version: {error}")
        })?;
    // The host-grant restore fence runs when the ingress authority attaches
    // to the grant store (`http::attach_host_grant_store`), so the binding
    // the attachment keeps is the one the fence verified. A refusal leaves
    // managed rooms unavailable on this replica and does not stop it.
    // Membership reconciliation (reads every user row and provisions
    // permission tuples) moved behind the startup lineage attestation gate
    // in `create_websocket_state` — it mutates application state and must
    // not run against a database this node cannot vouch for.
    // The inbox projection is written inside the ingress transaction, so its
    // rows must live in the global database: SQLite shares the global pool
    // outright; PostgreSQL must be pointed at the same database.
    let inbox_startup =
        http::create_ingress_inbox_storage(&xmpp_config.inbox_database_url, &global_db).await?;
    let inbox_database_storage = inbox_startup.storage().clone();
    let inbox_storage: Arc<dyn waddle_xmpp::inbox::storage::InboxStorage> =
        inbox_database_storage.clone();
    let (spaces_metadata_database_store, channel_space_link_database_store, space_stores_attached) =
        create_space_stores(
            db_pool.global(),
            &xmpp_config.spaces_metadata_database_url,
            &xmpp_config.channel_space_links_database_url,
        )
        .await?;
    let spaces_metadata_database_store = Arc::new(spaces_metadata_database_store);
    let spaces_metadata_store: Arc<dyn crate::spaces_metadata::SpacesMetadataStore> =
        spaces_metadata_database_store.clone();
    let channel_space_link_database_store = Arc::new(channel_space_link_database_store);
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
    let (pubsub_database_storage, pubsub_attached_global) =
        create_pubsub_storage(db_pool.global(), &xmpp_config.pubsub_database_url).await?;
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
    // Rooms that enrolled host-grant scopes manage hydrate from their
    // canonical grants instead, and their actors keep a members-only
    // configuration. The gate is the one the server state holds, so the
    // attachment verified at startup reaches both.
    let durable_membership_source: Arc<dyn waddle_xmpp::muc::affiliation::DurableMembershipSource> =
        Arc::new(
            durable_membership::PermissionDurableMembershipSource::new(permission_actor.clone())
                .with_host_grants(host_grants.clone()),
        );
    let server_hats = waddle_xmpp::xep::xep0317::ServerHats::default();
    let room_registry_handle = waddle_xmpp::muc::RoomRegistry::spawn(
        xmpp_config.muc_domain.to_string(),
        server_config.occupant_id_secret.clone(),
        server_hats.clone(),
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
    let deadline_on_shutdown = shutdown_deadline.clone();
    tokio::spawn(async move {
        stop_token_for_lifecycle.cancelled().await;
        deadline_on_shutdown.start();
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
        crate::clustering::ShutdownSmDrain {
            process_stop: stop_token.clone(),
            complete: sm_drain_complete.clone(),
            connections_drained: connections_drained.clone(),
            connections: shutdown.handle(),
            deadline: shutdown_deadline.clone(),
        },
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
    // The registry's single-node default is already an
    // ObservedClaimStore-wrapped InProcessClaimStore (#1648), so no
    // unconditional re-wiring: re-wiring a spawned registry races its
    // hydration and is only warranted when a real clustered pair exists.
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
    // Owner entries outside the identity profile fail configuration load
    // here, before any membership is provisioned from them.
    let owner_config = bootstrap_membership::BootstrapMembershipConfig::try_from_env()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let server_owner_jids = resolve_server_owner_jids(&owner_config, &xmpp_config.domain);
    let mut state = AppState::new_with_deps(AppStateDeps {
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
        server_hats,
        permission_actor: permission_actor.clone(),
        server_owner_jids,
        node_lifecycle: node_lifecycle.clone(),
        max_websocket_connections: server_config.max_websocket_connections,
        clustering_claims: clustering_handles,
        lineage_config: lineage_config.clone(),
        clustering_enabled: server_config.clustering.enabled,
    });
    state.host_grants = host_grants;
    // Host-grant decisions about remote principals read the snapshot this
    // replica runs; while S2S is off they admit no remote domain.
    state.s2s_federation = s2s_config
        .as_deref()
        .map(crate::permissions::host_grant::FederationSnapshot::of);
    let state = Arc::new(state);
    if let Some(matched) = global_adopt_matched {
        state.note_lineage_adopt_match(matched);
    }
    // The global store follows the same ephemeral classification as every
    // other boundary: an in-memory SQLite global (bare dev runs, the WS
    // integration harness) is non-durable and exempt from attestation.
    let global = db_pool.global();
    if let Some(capability) = global.attested_pg_writer_capability() {
        let expected =
            capability.attested_lineage_for_deployment(state.lineage_config.deployment_uuid)?;
        crate::db::bootstrap::verify_global_runtime(global, &state.lineage_config, &expected)
            .await?;
        state.register_lineage_database(crate::db::lineage::DurableStore::Global, global.clone());
    } else {
        super::bootstrap_and_register_store_lineage(
            &state,
            crate::db::lineage::DurableStore::Global,
            global.clone(),
        )
        .await?;
    }
    if db_pool.global().has_control_plane_pool() {
        state.register_lineage_control_plane(
            crate::db::lineage::DurableStore::ControlPlane,
            db_pool.global().clone(),
        );
    }
    register_inbox_lineage(&state, &inbox_startup).await?;
    if space_stores_attached {
        state.register_lineage_alias(
            crate::db::lineage::DurableStore::SpacesMetadata,
            crate::db::lineage::DurableStore::Global,
        );
        state.register_lineage_alias(
            crate::db::lineage::DurableStore::ChannelSpaceLinks,
            crate::db::lineage::DurableStore::Global,
        );
    } else {
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
    }
    if pubsub_attached_global {
        state.register_lineage_alias(
            crate::db::lineage::DurableStore::Pubsub,
            crate::db::lineage::DurableStore::Global,
        );
    } else {
        bootstrap_and_register_store_lineage(
            &state,
            crate::db::lineage::DurableStore::Pubsub,
            pubsub_database_storage.database(),
        )
        .await?;
    }
    // ADR-0017 Phase 3 Slice 7 FIX 1: co-location-check MAM storage against
    // the clustering global database and enable fenced groupchat-archive
    // writes when clustering is live, mirroring the
    // `sm_persistence`/`pending_delivery` `open_for_cluster_mode` gating
    // already established for their own durability stores.
    let websocket_mam_startup = select_websocket_mam_storage(
        &xmpp_config.mam_database_url,
        xmpp_config.mam_owner_claim_handoff.as_ref(),
        server_config.clustering.enabled,
        state.clustering_claims.claim_pair().is_some(),
        db_pool.global(),
    )
    .await?;
    super::bootstrap_mam_lineage(&state, &websocket_mam_startup).await?;
    let acme_runtime = start_acme_runtime(&xmpp_config, stop_token.clone());

    // Start HTTP server
    let http_state = state.clone();
    let http_mam_storage = websocket_mam_startup.as_mam_storage();
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
    let http_shutdown_deadline = shutdown_deadline.clone();
    // With S2S enabled, the router is the remote route of local entities
    // and the stanza sink of inbound streams. Its pending IQ correlations
    // live in the global database, which every replica shares.
    let s2s_router = match s2s_config.as_ref() {
        Some(config) => Some(start_s2s_router(
            Arc::clone(config),
            &xmpp_config,
            db_pool.global().clone(),
            stop_token.clone(),
        )?),
        None => None,
    };
    let http_s2s_router = s2s_router.clone();
    let http_handle = tokio::spawn(async move {
        start_http_server(HttpServerDeps {
            state: http_state,
            server_config: http_server_config,
            xmpp_config: http_xmpp_config,
            mam_storage: http_mam_storage,
            pubsub_database_storage: http_pubsub_database_storage,
            owner_outbox,
            acme_http01_challenge_service,
            listener: http_listener,
            shutdown_handle: http_shutdown_handle,
            drain_complete: http_drain_complete,
            sm_drain_complete,
            shutdown_deadline: http_shutdown_deadline,
            connections_drained,
            s2s_router: http_s2s_router,
        })
        .await
    });

    info!("TCP XMPP listener disabled; serving WebSocket C2S only");

    // The S2S service starts draining when the process stop token fires,
    // after the router's shutdown notices where a router runs, and is
    // joined below. It shares no lock, permit or task with HTTP.
    // Configured endpoints, and DNS discovery through the system's name
    // servers when the configuration enables it (ADR-0020 D3).
    let s2s_resolver = s2s_config
        .as_ref()
        .map(|config| {
            crate::s2s::discovery::PeerEndpoints::for_config(std::sync::Arc::clone(config))
        })
        .transpose()?;
    let s2s_service = s2s_config.map(|config| {
        // With a router, the streams drain once the router has told the
        // remote occupants of this replica's rooms that it is stopping,
        // which it does when the process stop token fires; the notices
        // need the streams. Without one they drain on that token itself.
        let drain = crate::s2s::route::S2sRouter::service_stop(s2s_router.as_ref(), &stop_token);
        let mut options = crate::s2s::conn::ServiceOptions {
            stop: Some(drain),
            resolver: s2s_resolver.map(|resolver| {
                std::sync::Arc::new(resolver)
                    as std::sync::Arc<dyn crate::s2s::conn::EndpointResolver>
            }),
            ..Default::default()
        };
        if let Some(router) = &s2s_router {
            options.sink = Arc::new(router.clone());
        }
        let service = crate::s2s::conn::S2sService::start(config, s2s_listener, options);
        if let Some(router) = &s2s_router {
            router.attach_transport(service.handle());
        }
        service
    });

    // Run the Ecdysis shutdown lifecycle
    let shutdown_handle = tokio::spawn(async move {
        let signal = shutdown
            .run(|| async {
                // Replacement startup is managed by the supervisor/operator.
                info!("SIGQUIT received — draining for supervisor-managed restart");
            })
            .await;

        info!(signal = ?signal, "Shutdown lifecycle complete");
    });

    // Await the HTTP serving task, whose graceful-shutdown closure waits
    // for the SM Q6 drain notification. Its forced deadline can end this
    // wait without proving that every connection or drain task completed.
    let result = match http_handle.await {
        Ok(Ok(())) => {
            info!("HTTP server stopped (graceful drain complete)");
            Ok(())
        }
        Ok(Err(e)) => {
            // Startup failed (e.g. the lineage attestation gate exiting on
            // an unreachable database) rather than a signal firing: cancel
            // the stop token so the clustering drain below returns
            // immediately instead of burning its full budget and logging a
            // spurious drain-timeout on every crash-looping boot.
            stop_token.cancel();
            Err(e)
        }
        Err(e) => {
            stop_token.cancel();
            Err(anyhow::anyhow!("HTTP server task failed: {}", e))
        }
    };
    // ADR-0017 Phase 3 Slice 10: the clustering node-lease loop runs its
    // own per-entity graceful drain (mark draining, seal + batch-release
    // owned `RoomActor` claims) on this same `stop_token` firing, in
    // parallel with the HTTP/SM drain just awaited above, then attempts
    // retirement and the terminal sweep. #2046 W1.10: wait for completion
    // only until the shared deadline, not for a fresh budget after HTTP
    // exit. A terminal wait entered before process stop can still be pending;
    // timing out this await neither proves demotion completed nor cancels
    // the task. It remains detached until completion or runtime teardown,
    // with unfinished ownership left to lease expiry. Telemetry shutdown
    // and runtime teardown follow this wait. A no-op when clustering is
    // disabled.
    clustering_shutdown
        .await_drain_until(shutdown_deadline.start().deadline())
        .await;
    // Join every S2S connection task within what remains of the shutdown
    // budget; the service has been draining since the stop token fired.
    if let Some(service) = s2s_service {
        service
            .shutdown(shutdown_deadline.start().remaining())
            .await;
    }
    // The router's workers stopped taking stanzas once its shutdown
    // notices were written or had their time; join them within what
    // remains.
    if let Some(router) = s2s_router {
        router.shutdown(shutdown_deadline.start().remaining()).await;
    }

    // Issue #1388: both drains just awaited above (SM/Q6 via `http_handle`,
    // clustering per-entity via `clustering_shutdown`) increment counters and
    // histograms — including the tail-end-only `xmpp.sm.drain_timeout` and
    // `waddle.clustering.drain_duration_ms` — right up to the moment they
    // return. Flush the observations available after the bounded waits:
    // there may be no periodic export before exit, and a detached task
    // that outlasted its wait may still emit later observations.
    let metrics_flush = crate::telemetry::flush_metrics_before_exit().await;

    // Tear down the shutdown lifecycle task so we don't dangle.
    // If HTTP exited on its own (error path) before any signal
    // arrived, `shutdown_handle.await` would block on
    // `shutdown.run()` indefinitely.
    shutdown_handle.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), shutdown_handle).await;
    validate_node_shutdown(&node_lifecycle)?;
    info!("Shutdown waits finished");
    result.map(|()| metrics_flush)
}

/// An owner can signal the fatal fence before it obtains the lifecycle write
/// lock. Do not report successful shutdown while that latch is still pending.
pub(super) fn validate_node_shutdown(lifecycle: &crate::clustering::NodeLifecycle) -> Result<()> {
    let fatal = lifecycle.fatal_fence_token();
    if let Some(failure) = lifecycle.pending_websocket_owner_failure() {
        anyhow::bail!("critical node failure; cleanup completion is unconfirmed: {failure:?}");
    }
    if let Some(failure) = lifecycle.critical_failure() {
        anyhow::bail!("critical node failure; cleanup completion is unconfirmed: {failure:?}");
    }
    if let Some(failure) = lifecycle.pending_websocket_owner_failure() {
        anyhow::bail!("critical node failure; cleanup completion is unconfirmed: {failure:?}");
    }
    if fatal.is_cancelled() {
        anyhow::bail!("fatal node fence signalled; cleanup completion is unconfirmed");
    }
    Ok(())
}

/// Preserve the configured location's provenance before constructing PubSub.
/// Restricted runtime attachment must share the exact attested main pool;
/// opening an equal-looking URL would create another authority domain.
pub async fn create_pubsub_storage(
    global: &crate::db::Database,
    location: &ResolvedXmppDatabaseUrl,
) -> Result<(Arc<crate::pubsub::DatabasePubSubStorage>, bool)> {
    if global.uses_attested_postgres_pool() {
        if !matches!(location, ResolvedXmppDatabaseUrl::InheritedGlobal(_)) {
            anyhow::bail!("restricted PubSub storage must inherit the global database location");
        }
        let storage = crate::pubsub::DatabasePubSubStorage::attach_existing(global.clone())
            .await
            .inspect_err(|_| {
                waddle_xmpp::telemetry::authority::record_restricted_attach_failure(
                    waddle_xmpp::telemetry::attributes::RestrictedStore::PubSub,
                )
            })
            .map_err(|error| anyhow::anyhow!("failed to attach PubSub storage: {error}"))?;
        Ok((Arc::new(storage), true))
    } else if global.ingress_pool_size().is_some() {
        if !matches!(location, ResolvedXmppDatabaseUrl::InheritedGlobal(_)) {
            anyhow::bail!("shared owner PubSub storage must inherit the global database location");
        }
        let storage = crate::pubsub::DatabasePubSubStorage::initialize_shared_owner(global.clone())
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to initialize shared PubSub storage: {error}")
            })?;
        Ok((Arc::new(storage), true))
    } else {
        let storage = crate::pubsub::build_database_pubsub_storage(location.clone_resolved_url())
            .await
            .map_err(|error| anyhow::anyhow!("failed to initialize PubSub storage: {error}"))?;
        Ok((storage, false))
    }
}

/// Select both space stores together so a restricted runtime cannot attach
/// one store while opening a separate owner-capable pool for the other.
pub(crate) async fn create_space_stores(
    global: &crate::db::Database,
    spaces_location: &ResolvedXmppDatabaseUrl,
    links_location: &ResolvedXmppDatabaseUrl,
) -> Result<(
    DatabaseSpacesMetadataStore,
    crate::channel_space_links::DatabaseChannelSpaceLinkStore,
    bool,
)> {
    if global.uses_attested_postgres_pool() {
        if !matches!(spaces_location, ResolvedXmppDatabaseUrl::InheritedGlobal(_))
            || !matches!(links_location, ResolvedXmppDatabaseUrl::InheritedGlobal(_))
        {
            anyhow::bail!(
                "restricted space stores require both database locations to inherit the global pool"
            );
        }
        let spaces = DatabaseSpacesMetadataStore::attach_existing(global.clone())
            .await
            .inspect_err(|_| {
                waddle_xmpp::telemetry::authority::record_restricted_attach_failure(
                    waddle_xmpp::telemetry::attributes::RestrictedStore::Spaces,
                )
            })
            .map_err(|error| {
                anyhow::anyhow!("failed to attach spaces metadata storage: {error}")
            })?;
        let links = crate::channel_space_links::DatabaseChannelSpaceLinkStore::attach_existing(
            global.clone(),
        )
        .await
        .inspect_err(|_| {
            waddle_xmpp::telemetry::authority::record_restricted_attach_failure(
                waddle_xmpp::telemetry::attributes::RestrictedStore::Spaces,
            )
        })
        .map_err(|error| anyhow::anyhow!("failed to attach channel-space link storage: {error}"))?;
        Ok((spaces, links, true))
    } else if global.ingress_pool_size().is_some() {
        if !matches!(spaces_location, ResolvedXmppDatabaseUrl::InheritedGlobal(_))
            || !matches!(links_location, ResolvedXmppDatabaseUrl::InheritedGlobal(_))
        {
            anyhow::bail!("shared owner space stores must inherit the global database location");
        }
        let spaces = DatabaseSpacesMetadataStore::initialize_owner(global.clone()).await?;
        let links = crate::channel_space_links::DatabaseChannelSpaceLinkStore::initialize_owner(
            global.clone(),
        )
        .await?;
        Ok((spaces, links, true))
    } else {
        let spaces = DatabaseSpacesMetadataStore::open(spaces_location.as_deref())
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to initialize spaces metadata storage: {error}")
            })?;
        let links = crate::channel_space_links::DatabaseChannelSpaceLinkStore::open(
            links_location.as_deref(),
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to initialize channel-space link storage: {error}")
        })?;
        Ok((spaces, links, false))
    }
}

pub(crate) async fn register_inbox_lineage(
    state: &AppState,
    startup: &http::InboxStartup,
) -> Result<()> {
    use crate::db::lineage::DurableStore;

    match startup {
        http::InboxStartup::AttachedGlobal(_) => {
            state.register_lineage_alias(DurableStore::Inbox, DurableStore::Global);
            Ok(())
        }
        http::InboxStartup::OwnerInitialized(storage) => {
            bootstrap_and_register_store_lineage(state, DurableStore::Inbox, storage.database())
                .await
        }
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
    if matches!(
        config.action,
        Some(crate::db::lineage::LineageAction::Enroll)
    ) {
        info!(store = %store, "enrolling database lineage");
    }
    let matched = crate::db::bootstrap::bootstrap_store_owner(&database, config).await?;
    if let Some(matched) = matched {
        info!(store = %store, matched = %matched, "adopted database lineage");
        state.note_lineage_adopt_match(matched);
    } else if matches!(
        config.action,
        Some(crate::db::lineage::LineageAction::Adopt(_))
    ) {
        info!(
            store = %store,
            "lineage adopt action did not name this boundary; leaving it untouched"
        );
    }
    Ok(())
}

pub(in crate::server) async fn bootstrap_and_register_store_lineage(
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

pub(in crate::server) async fn bootstrap_mam_lineage(
    state: &AppState,
    storage: &MamStartupStorage,
) -> Result<()> {
    if let Some(alias_target) = storage.lineage_alias_target() {
        state.register_lineage_alias(crate::db::lineage::DurableStore::Mam, alias_target);
        return Ok(());
    }
    let MamStartupStorage::Legacy(storage) = storage else {
        unreachable!("shared MAM storage returns before legacy lineage handling")
    };
    if let Some(pool) = storage.sqlite_pool() {
        let in_memory = crate::db::lineage::sqlite_pool_is_in_memory(pool).await?;
        let database = crate::db::Database::from_sqlite_pool(
            "mam",
            pool.clone(),
            if in_memory {
                crate::db::SqlitePoolStorage::InMemory
            } else {
                crate::db::SqlitePoolStorage::Persistent
            },
        );
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

/// Every trusted home of the S2S snapshot must be a remote domain host
/// grants can be issued to (ADR-0020 D8): neither the account domain nor a
/// domain of one of this deployment's services, the call mixer included.
/// The snapshot's loader
/// refuses the domains it names as local; this refuses the service domains
/// it does not know, and a service domain that is not canonical, which no
/// issuance policy can be built from. A deployment whose XMPP domain is not
/// canonical is refused later in startup, with its own error.
fn check_trusted_homes(
    config: &crate::s2s::config::S2sConfig,
    xmpp_config: &XmppConfig,
) -> anyhow::Result<()> {
    use crate::permissions::host_grant::{
        FederationSnapshot, PrincipalPolicy, ServedAccountDomains,
    };
    let Ok(account) = waddle_xmpp::identity::CanonicalDomain::parse_canonical(&xmpp_config.domain)
    else {
        return Ok(());
    };
    let principals = PrincipalPolicy::new(
        ServedAccountDomains::new([account]),
        Some(FederationSnapshot::of(config)),
    );
    crate::server::routes::websocket::XmppServiceDomains::new(
        &xmpp_config.domain,
        xmpp_config.muc_domain.as_str(),
        xmpp_config.spaces_jid.domain().as_str(),
    )
    .host_grant_issuance_policy(&principals)
    .map(|_| ())
    .ok_or_else(|| {
        anyhow::anyhow!(
            "S2S configuration: admission.trusted_homes names a domain of one of this \
             deployment's services, or a service domain is not canonical"
        )
    })
}

/// The accounts a privileged relation may be written for on this replica:
/// those of its account domain and of the trusted homes of the S2S snapshot
/// it runs, none while S2S is off (ADR-0020 D8). A deployment whose domains
/// are not canonical gets a policy that admits no account, so such a
/// relation is written for nobody rather than for anybody.
fn replica_privileged_issuance(
    xmpp_config: &XmppConfig,
    s2s_config: Option<&crate::s2s::config::S2sConfig>,
) -> crate::permissions::host_grant::IssuancePolicy {
    use crate::permissions::host_grant::{
        FederationSnapshot, IssuancePolicy, PrincipalPolicy, ServedAccountDomains,
    };
    let Ok(account) = waddle_xmpp::identity::CanonicalDomain::parse_canonical(&xmpp_config.domain)
    else {
        return IssuancePolicy::default();
    };
    let principals = PrincipalPolicy::new(
        ServedAccountDomains::new([account]),
        s2s_config.map(FederationSnapshot::of),
    );
    crate::server::routes::websocket::XmppServiceDomains::new(
        &xmpp_config.domain,
        xmpp_config.muc_domain.as_str(),
        xmpp_config.spaces_jid.domain().as_str(),
    )
    .host_grant_issuance_policy(&principals)
    .unwrap_or_default()
}

/// The router's bounds under `config`: its defaults, with the pair queues
/// the snapshot's `[limits] max_pair_queues` allows. Half of them may be
/// opened for what remote senders ask for, as by default.
fn router_limits(config: &crate::s2s::config::S2sConfig) -> crate::s2s::route::RouterLimits {
    let pairs = config.limits().max_pair_queues;
    crate::s2s::route::RouterLimits {
        peer_queues: pairs,
        reply_queues: (pairs / 2).max(1),
        ..crate::s2s::route::RouterLimits::default()
    }
}

/// The S2S router for an enabled S2S configuration: the destinations this
/// deployment serves, exactly as the WebSocket classifier sees them, and the
/// shared correlation and remote room tables in the global database.
fn start_s2s_router(
    config: Arc<crate::s2s::config::S2sConfig>,
    xmpp_config: &XmppConfig,
    global: crate::db::Database,
    stop: tokio_util::sync::CancellationToken,
) -> anyhow::Result<crate::s2s::route::S2sRouter> {
    let served = crate::server::routes::websocket::XmppServiceDomains::new(
        &xmpp_config.domain,
        xmpp_config.muc_domain.as_str(),
        xmpp_config.spaces_jid.domain().as_str(),
    )
    .served_domains(&xmpp_config.domain)
    .ok_or_else(|| anyhow::anyhow!("the XMPP domain is not a valid domain"))?;
    let limits = router_limits(&config);
    Ok(crate::s2s::route::S2sRouter::new(
        crate::s2s::route::RouterOptions {
            config,
            served,
            correlations: Arc::new(crate::s2s::route::SqlIqCorrelations::new(global.clone())),
            rooms: Arc::new(crate::s2s::route::SqlRemoteRoomStore::new(global)),
            limits,
            stop: Some(stop),
        },
    ))
}
