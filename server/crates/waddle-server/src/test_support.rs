//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

#[cfg(feature = "test-support")]
pub mod iq_forward;
#[cfg(feature = "test-support")]
pub mod observer_quarantine;
#[cfg(feature = "test-support")]
pub mod postgres_proxy;
#[cfg(feature = "test-support")]
pub(crate) mod relation_guard_lab;
#[cfg(feature = "test-support")]
pub use relation_guard_lab::verify_disposable_relation_guard_chain;
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub use relation_guard_lab::verify_recorded_room_archive;
#[cfg(feature = "test-support")]
mod ordered_claim;
#[cfg(feature = "test-support")]
mod shared_correctness;
#[cfg(feature = "test-support")]
pub use crate::pending_delivery::database::ordered_claim::{OrderedClaimError, OrderedClaimPhase};
#[cfg(feature = "test-support")]
pub use ordered_claim::{OrderedClaimAttemptLab, OrderedClaimLab, OrderedClaimObservationLab};
#[cfg(feature = "test-support")]
pub use shared_correctness::{
    probe_public_mam_acl_rollback, SharedCorrectnessFixture, SharedCorrectnessSubmitError,
    SharedPendingResolverFixture,
};
#[cfg(feature = "test-support")]
pub mod spicedb_wire;
#[cfg(feature = "test-support")]
pub mod supervised_io;

use crate::db::DatabasePool;
use crate::ingress::IngressAuthority;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use std::sync::Arc;

#[cfg(feature = "test-support")]
use crate::config::LineageConfig;
#[cfg(feature = "test-support")]
use crate::db::bootstrap::VerifiedGlobalAttachment;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPgPool;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
#[cfg(feature = "test-support")]
use crate::db::Database;
#[cfg(feature = "test-support")]
use crate::db::DatabaseError;
#[cfg(feature = "test-support")]
use crate::ingress_substrate::with_pooled_receipt_commit_ack_lost;
#[cfg(feature = "test-support")]
use crate::ingress_substrate::PostgresIngressSubstrate;
#[cfg(feature = "test-support")]
use crate::ingress_uow::{
    begin_raw_ingress_writer_with_capability, EffectReceiptRepository, IngressUnitOfWork,
    IngressUowError,
};
#[cfg(feature = "test-support")]
use sqlx::postgres::PgPool;

/// Build the current schema of the shared PostgreSQL test database (the one
/// `WADDLE_TEST_POSTGRES_URL` names) from the deployment catalog.
///
/// The schema baseline builds every table from an empty schema. Tests that
/// share this database also open stores whose constructors create some of
/// those tables themselves, so a test that ran first may have left them
/// behind without the baseline's ledger row. Such a schema is dropped and
/// recreated before the catalog runs: the shared test database may be reset
/// freely, and the tests that use it run one at a time (nextest's `postgres`
/// group). A schema whose ledger already holds the baseline is migrated in
/// place.
pub async fn migrate_shared_test_postgres(
    db: &crate::db::Database,
) -> Result<(), crate::db::DatabaseError> {
    if db.driver() != crate::db::DatabaseDriver::Postgres {
        return Err(crate::db::DatabaseError::ConnectionFailed(
            "the shared test database is PostgreSQL".to_owned(),
        ));
    }
    let conn = db.guard().await?;
    let mut rows = conn
        .query(
            "SELECT pg_catalog.quote_ident(pg_catalog.current_schema()::text), \
                    pg_catalog.to_regclass('_migrations') IS NOT NULL, \
                    EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
                            JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                            WHERE n.nspname = pg_catalog.current_schema() \
                              AND c.relname NOT IN ('_lineage', '_migrations'))",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| crate::db::DatabaseError::ConnectionFailed("no current schema".into()))?;
    let (schema, has_ledger, has_relations): (String, bool, bool) =
        (row.get(0)?, row.get(1)?, row.get(2)?);
    drop(rows);
    let on_baseline = if has_ledger {
        let mut rows = conn
            .query(
                "SELECT EXISTS (SELECT 1 FROM _migrations WHERE version = ?)",
                crate::db_params![crate::db::BASELINE_VERSION],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        }
    } else {
        false
    };
    if has_relations && !on_baseline {
        conn.execute_batch(&format!(
            "DROP SCHEMA {schema} CASCADE; CREATE SCHEMA {schema}"
        ))
        .await?;
    }
    drop(conn);
    crate::db::MigrationRunner::single().run(db).await?;
    Ok(())
}

/// Prepare the legacy observation schema before applying registered migrations.
#[cfg(feature = "test-support")]
pub async fn initialize_room_observations_for_test(
    database: &Database,
) -> Result<(), crate::ingress_uow::ObservationError> {
    crate::ingress_uow::initialize_room_observations(database).await
}

/// Prepare both PostgreSQL space stores in a disposable owner fixture.
/// The caller supplies the expected identity, which is checked inside the owner
/// transaction. This does not attest its source or authorize serving.
#[cfg(feature = "test-support")]
pub async fn prepare_paired_space_stores_owner_for_test(
    db: Database,
    identity: &crate::db::lineage::PgIdentity,
) -> Result<(), crate::spaces_metadata::SpaceStoreAttachError> {
    crate::spaces_metadata::prepare_space_stores_owner(db, identity).await
}

/// Exercise the production SM store selection and attachment path.
#[cfg(feature = "test-support")]
pub async fn attach_sm_startup(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
    clustering_enabled: bool,
) -> anyhow::Result<Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage>> {
    crate::server::attach_sm_startup_for_test(selection, global, clustering_enabled).await
}

/// Exercise the production pending-delivery store selection and attachment path.
#[cfg(feature = "test-support")]
pub async fn attach_pending_startup(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
) -> anyhow::Result<DatabasePendingDeliveryStorage> {
    crate::server::attach_pending_startup_for_test(selection, global).await
}

/// Exercise the webhook delivery ledger attachment that restricted startup
/// runs before mounting the LiveKit webhook route.
#[cfg(feature = "test-support")]
pub async fn attach_webhook_delivery_ledger(
    global: &Database,
) -> Result<(), crate::server::WebhookDeliveryAttachError> {
    crate::server::routes::webhook_delivery::DatabaseWebhookDeliveryStore::attach_existing(
        global.clone(),
    )
    .await
    .map(|_| ())
}

/// Exercise production MAM startup selection without starting HTTP. The
/// observation includes the lineage alias target derived from the selected
/// production branch, if this attachment shares the global boundary.
#[cfg(feature = "test-support")]
pub async fn select_mam_storage_for_test(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    owner_handoff: Option<&crate::server::MamOwnerClaimHandoff>,
    clustering_enabled: bool,
    clustering_claim_pair_live: bool,
    global: &Database,
) -> anyhow::Result<(bool, Option<crate::db::lineage::DurableStore>)> {
    crate::server::select_mam_storage_for_test(
        selection,
        owner_handoff,
        clustering_enabled,
        clustering_claim_pair_live,
        global,
    )
    .await
}

/// Exercise production ingress authority selection without starting HTTP.
#[cfg(feature = "test-support")]
pub async fn create_ingress_authority_at_startup(
    config: &crate::config::IngressConfig,
    pool: &DatabasePool,
    lineage: &LineageConfig,
) -> Result<IngressAuthority, crate::ingress::IngressStartupError> {
    crate::server::create_ingress_authority_for_test(config, pool, lineage).await
}

pub use crate::server::routes::websocket::muc_invites::{
    claim_invite, list_invites, record_invite_at, InviteStorageError, OutstandingInvite,
    RecordOutcome,
};
/// Hold an ingress acceptance transaction that the host-grant authority
/// admitted, with the scope's guard held, until `proceed` is notified.
/// `entered` is notified when the transaction reaches the gate.
pub async fn with_host_grant_admission_gate<F: std::future::Future>(
    entered: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
    future: F,
) -> F::Output {
    crate::ingress::commit::commit_hooks::with_host_grant_admission_gate(entered, proceed, future)
        .await
}

/// Activate the operation `marker` names as a verified projection would,
/// without a projection behind it. Activation otherwise accepts only the
/// witness that verification of a SpiceDB projection produces; suites that
/// exercise the canonical store and SQL admission without SpiceDB, and
/// cases that replay a delayed activation, use this instead.
///
/// # Errors
/// Returns the store's refusal or failure, as `HostGrantStore::activate`.
pub async fn activate_host_grant_marker(
    store: &crate::permissions::host_grant::HostGrantStore,
    marker: &crate::permissions::host_grant::ProjectionMarker,
) -> Result<
    crate::permissions::host_grant::Activation,
    crate::permissions::host_grant::HostGrantError,
> {
    store
        .activate(
            &crate::permissions::host_grant::VerifiedProjection::without_projection_for_test(
                marker.clone(),
            ),
        )
        .await
}

/// What `/readyz` answers for a replica whose database is healthy and whose
/// lineage is attested, given its host-grant gate: the status code and the
/// body, as the readiness handler builds them.
#[cfg(feature = "test-support")]
pub fn host_grant_readiness_response(
    gate: &crate::permissions::host_grant::HostGrantAttachmentGate,
) -> (u16, serde_json::Value) {
    let (status, body) = crate::server::attested_readiness_response(Some(gate));
    (status.as_u16(), body.0)
}

/// The production source a room actor hydrates its durable audience from:
/// the channel and parent Space membership the permission store holds.
pub use crate::server::durable_membership::PermissionDurableMembershipSource;
/// The production `/ws` RFC 7395 router, so suites can serve a test state
/// over a real loopback WebSocket and drive the live connection loop.
pub use crate::server::routes::websocket::router;
pub use crate::server::routes::websocket::s2s_accounts::WebSocketLocalAccounts;
pub use crate::server::routes::websocket::s2s_delivery::WebSocketLocalDelivery;
pub use crate::server::routes::websocket::s2s_rooms::RoomCarrier;
pub use crate::server::routes::websocket::test_bindings::sync_registry_bindings;
pub use crate::server::routes::websocket::test_state::{
    create_test_session, register_test_connection, seed_local_account,
};
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_ingress_factory, empty_extension_manager, TestStateOverrides,
};
pub use crate::server::routes::websocket::WebSocketState;

/// Build the default in-memory WebSocket state the in-crate suites use
/// (SQLite in memory, single-node registries, the test ingress authority).
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides::default(),
        || std::future::ready(None),
    )
    .await
}

/// The default in-memory WebSocket state of a replica that runs the S2S
/// snapshot `config`, as startup records it when S2S is enabled: decisions
/// about remote principals read its admission policy and trusted-home list.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
#[cfg(feature = "test-support")]
pub async fn websocket_state_running_s2s(
    config: &crate::s2s::config::S2sConfig,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            s2s_federation: Some(crate::permissions::host_grant::FederationSnapshot::of(
                config,
            )),
            ..Default::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Have `router` host the rooms of `state` for remote occupants, with their
/// seats in `seats`, as startup does for a replica with S2S enabled: the
/// router hands what remote principals ask of the MUC service to the
/// state's rooms, a room's stanza for a remote occupant leaves through the
/// router, and decisions that name a seated remote occupant read the
/// evidence its seat was admitted under.
#[cfg(feature = "test-support")]
pub fn host_rooms_for_remote_occupants(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
    seats: crate::db::Database,
) {
    crate::server::routes::websocket::s2s_rooms::host_rooms(state, router, seats);
}

/// `actor` deciding issuance of the privileged relations as startup makes a
/// replica's permission actor decide it, for the served domain of `xmpp`
/// and the trusted homes of `s2s` (ADR-0020 D8). It is the function startup
/// calls, so a case that writes through the result shows what a serving
/// replica refuses.
#[cfg(feature = "test-support")]
pub fn permission_actor_as_startup_builds(
    actor: crate::permissions::PermissionActor,
    xmpp: &crate::server::XmppConfig,
    s2s: Option<&crate::s2s::config::S2sConfig>,
) -> crate::permissions::PermissionActor {
    crate::server::with_replica_privileged_issuance(actor, xmpp, s2s)
}

/// Wait until the Spaces host attached to `router` has decided every
/// publication it took, so that a case reads what left for subscribers at
/// other servers after its last publication.
#[cfg(feature = "test-support")]
pub async fn spaces_publications_settled(router: &crate::s2s::route::S2sRouter) {
    router.spaces_publications_settled().await;
}

/// Have `router` serve the Spaces of `state` to remote principals, as
/// startup does for a replica with S2S enabled: the router hands what
/// remote principals ask of the Spaces PubSub service to the state's
/// Spaces, and takes their event notifications for subscribers at other
/// servers. The state's remote route must be `router` for those
/// notifications to leave.
#[cfg(feature = "test-support")]
pub fn host_spaces_for_remote_principals(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
) {
    crate::server::routes::websocket::s2s_spaces::host_spaces(state, router);
}

/// [`host_rooms_for_remote_occupants`] for a replica whose requests for
/// rooms another replica holds are carried by `carrier`, in place of the
/// cluster's room claims and relay.
#[cfg(feature = "test-support")]
pub fn host_rooms_for_remote_occupants_carrying(
    state: &Arc<WebSocketState>,
    router: &crate::s2s::route::S2sRouter,
    seats: crate::db::Database,
    carrier: Arc<dyn RoomCarrier>,
) {
    crate::server::routes::websocket::s2s_rooms::host_rooms_carrying(
        state,
        router,
        seats,
        Some(carrier),
    );
}

/// The owner side of a remote principal's room request carried over the
/// cluster relay (`waddle.clustering.relay.remote_room_request.v1`), as
/// the relay's handler runs it on the replica that receives the message.
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub async fn relayed_remote_room_request(
    state: &WebSocketState,
    message: crate::clustering::relay::RelayRemoteRoomRequest,
) -> crate::clustering::relay::RelayRemoteRoomRequestReply {
    crate::server::routes::websocket::relayed_remote_room_request(state, message).await
}

/// Whether a failed ask of a room's owner over the cluster relay says that
/// the owner did not answer or could not be reached, so that the router
/// answers the room's requests without asking it for a while, rather than
/// that it answered without taking the request.
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub fn room_owner_silent(error: &crate::clustering::relay::RelayAskError) -> bool {
    crate::server::routes::websocket::owner_silent(error)
}

/// The occupants told that a remote joiner left when its seat, given on a
/// standing its stream does not honour, was undone: of those the room
/// seats now (`seated`), every one but the joiner, those the room answered
/// the joiner with (`present`) and those that hold the joiner's nickname
/// now (`nick_holders`).
#[cfg(feature = "test-support")]
pub fn undone_join_recipients(
    seated: Vec<jid::FullJid>,
    present: &std::collections::HashSet<jid::FullJid>,
    joiner: &jid::FullJid,
    nick_holders: &[jid::FullJid],
) -> Vec<jid::FullJid> {
    crate::server::routes::websocket::handlers::presence::undone_join_recipients(
        seated,
        present,
        joiner,
        nick_holders,
    )
}

/// Make the account `localpart` of the state's domain an owner of the
/// deployment in the legacy permission store: the standing that lets an
/// account create a room no channel backs.
///
/// # Panics
/// Panics if the relationship cannot be written.
#[cfg(feature = "test-support")]
pub async fn make_deployment_owner(state: &WebSocketState, localpart: &str) {
    use crate::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
    let account = [localpart, "@", state.deps.auth_state.xmpp_domain.as_str()].concat();
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(
                    ObjectType::Server,
                    crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID,
                ),
                Relation::new("owner"),
                Subject::user(&account),
            ),
        })
        .await
        .expect("deployment owner relationship");
}

/// Run the startup reconciliation of local accounts against `state`'s
/// database, with the domains `state` serves, as a node does before it
/// serves: account keys are backfilled and roster items naming nobody are
/// pruned, while contacts at domains the deployment does not serve stay.
///
/// # Errors
/// Returns the reconciliation's error as text.
pub async fn reconcile_local_accounts(state: &WebSocketState) -> Result<(), String> {
    let served = state
        .deps
        .service_domains
        .served_domains(state.deps.auth_state.xmpp_domain.as_str());
    crate::auth::directory::reconcile_local_accounts(
        state.deps.app_state.db_pool.global_actor(),
        served.as_ref(),
    )
    .await
    .map_err(|error| error.to_string())
}

/// Every resource the account coordinator of `state` still binds for
/// `account`, with no presence filter: what the routes read as the
/// account's connected resources. A connection that ended is listed until
/// the coordinator has unbound it.
pub async fn account_resources(
    state: &WebSocketState,
    account: &jid::BareJid,
) -> Vec<jid::FullJid> {
    crate::server::routes::websocket::handlers::account_reads::resources(state, account).await
}

/// The default in-memory WebSocket state whose app state runs the S2S
/// snapshot of `config`, as a replica with S2S enabled does
/// (`AppState::s2s_federation`), so that the account side of S2S traffic
/// decides admission, and the client paths treat remote contacts, as in
/// production.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_federation(
    config: &crate::s2s::config::S2sConfig,
) -> Arc<WebSocketState> {
    let db_pool = Arc::new(
        DatabasePool::try_new(crate::db::DatabaseConfig::default(), crate::db::PoolConfig)
            .await
            .expect("db pool"),
    );
    let mut app_state = crate::server::AppState::new(Arc::clone(&db_pool));
    app_state.s2s_federation = Some(crate::permissions::host_grant::FederationSnapshot::of(
        config,
    ));
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            app_state: Some(Arc::new(app_state)),
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// [`websocket_state_with_federation`] whose PubSub service is the
/// database-backed store a deployment runs (`DatabasePubSubStorage`, on an
/// in-memory database of its own) rather than the in-memory one the
/// default test state uses: it keeps one subscription for an account,
/// under its bare JID, whichever of its addresses subscribes, as the
/// Spaces host relies on.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_federation_and_database_pubsub(
    config: &crate::s2s::config::S2sConfig,
) -> Arc<WebSocketState> {
    let db_pool = Arc::new(
        DatabasePool::try_new(crate::db::DatabaseConfig::default(), crate::db::PoolConfig)
            .await
            .expect("db pool"),
    );
    let pubsub = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage"),
    );
    let mut app_state = crate::server::AppState::new(Arc::clone(&db_pool));
    app_state.s2s_federation = Some(crate::permissions::host_grant::FederationSnapshot::of(
        config,
    ));
    app_state.pubsub_storage = pubsub.clone();
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            app_state: Some(Arc::new(app_state)),
            database_pubsub_storage: Some(pubsub),
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// The default in-memory WebSocket state whose app state names the Spaces
/// service the test state's Spaces handlers serve (`spaces.example.com`),
/// so that the code reading the app state's service (a join's Space parent
/// repair) reads the nodes those handlers write.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_test_spaces_service() -> Arc<WebSocketState> {
    let db_pool = Arc::new(
        DatabasePool::try_new(crate::db::DatabaseConfig::default(), crate::db::PoolConfig)
            .await
            .expect("db pool"),
    );
    let mut app_state = crate::server::AppState::new(Arc::clone(&db_pool));
    app_state.spaces_jid = "spaces.example.com"
        .parse()
        .expect("test Spaces service parses as BareJid");
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            app_state: Some(Arc::new(app_state)),
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// The default in-memory WebSocket state with `extension_manager` in place
/// of the empty one, for suites that serve extension routes.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_extension_manager(
    extension_manager: Arc<waddle_extensions::ExtensionManager>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        extension_manager,
        TestStateOverrides::default(),
        || std::future::ready(None),
    )
    .await
}

/// Pause each receipt continuation attempt for a canonical message.
#[cfg(feature = "test-support")]
pub fn receipt_custody_probe(
    authority: &IngressAuthority,
    message: waddle_xmpp::ingress::MessageKey,
    lose_ack: bool,
) -> Arc<crate::ingress::ReceiptCustodyProbe> {
    authority.receipt_custody_test_probe(message, lose_ack)
}

/// Bind the same node fatal-fence token that production startup supplies.
#[cfg(feature = "test-support")]
pub fn bind_receipt_custody_fatal_fence(authority: &IngressAuthority, state: &WebSocketState) {
    authority
        .bind_receipt_fatal_fence(state.deps.app_state.node_lifecycle.fatal_fence_token())
        .expect("bind test receipt fatal fence");
}

/// Override the bounded receipt continuation capacity in a test fixture.
#[cfg(feature = "test-support")]
pub fn receipt_custody_capacity(authority: &IngressAuthority, batches: usize, receipts: usize) {
    authority.receipt_custody_test_capacity(batches, receipts);
}

/// Inspect retained pinned operations and their original backend observations.
#[cfg(feature = "test-support")]
pub fn receipt_custody_unresolved(
    authority: &IngressAuthority,
) -> Vec<crate::ingress::ReceiptCustodyUnresolved> {
    authority.receipt_custody_test_unresolved()
}

/// Build the real ingress executor without its owner-side GC task. The
/// existing database and lineage must already be prepared by the fixture.
#[cfg(feature = "test-support")]
pub fn quiescent_pinned_receipt_ingress(
    database: Database,
    lineage: LineageConfig,
) -> IngressAuthority {
    IngressAuthority::for_pinned_receipt_test(database, lineage)
}

/// Select the pinned receipt worker for the real executor in a dark fixture.
#[cfg(feature = "test-support")]
pub async fn select_pinned_receipt_custody(
    authority: &IngressAuthority,
    pinned: crate::db::physical_attestation::PinnedReceiptAuthority,
    grant: crate::clustering::singleton_lease::LeaseGrant,
) -> bool {
    authority
        .select_pinned_receipt_for_test(pinned, grant)
        .await
}

/// Reconstruct the versioned operation identity from a recovered intent key.
#[cfg(feature = "test-support")]
pub fn receipt_operation_from_intent(
    message: waddle_xmpp::ingress::MessageKey,
    key: &crate::ingress::EffectReceiptKey,
) -> crate::db::physical_attestation::ReceiptOperation {
    crate::ingress::receipt_operation_for_test(message, key)
}

/// Attach an existing notification candidate through the normal owner path.
#[cfg(feature = "test-support")]
pub async fn attach_notification_candidate_lineage_for_test(
    uow: &IngressUnitOfWork,
    message: waddle_xmpp::ingress::MessageKey,
    effect: &waddle_xmpp::ingress::IngressEffectKey,
    candidate: &crate::notification_outbox::NotificationCandidate,
) -> Result<(), IngressUowError> {
    let mut transaction = uow.begin().await?;
    NotificationOutboxStore::attach_candidate_lineage_with_api(
        transaction.transaction_mut(),
        None,
        message,
        effect,
        candidate,
    )
    .await?;
    transaction.commit().await
}

/// Whether both the authority and its receipt worker carry this reserved lane.
#[cfg(feature = "test-support")]
pub fn receipt_custody_shares_ingress_lane(
    authority: &IngressAuthority,
    database: &Database,
) -> bool {
    authority.receipt_custody_shares_ingress_lane(database)
}

/// Build a test-only WebSocket fixture that advertises and dispatches the
/// dark SASL2 OAuthBearer path. This fixture does not assert TLS termination
/// or prove that a request arrived without early data.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_sasl2() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            sasl2_test_capability: true,
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// [`websocket_state_with_sasl2`]. Every fixture selects the durable
/// stream-management authority on its database, as startup does, so the
/// dark SASL2 handshake processes XEP-0198 inline resume and XEP-0386
/// Bind 2.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_sasl2_and_durable_sm() -> Arc<WebSocketState> {
    websocket_state_with_sasl2().await
}

/// [`websocket_state_with_sasl2`] on a state that never selected a
/// stream-management authority.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_sasl2_without_durable_sm() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            sasl2_test_capability: true,
            without_durable_sm: true,
            ..TestStateOverrides::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Resolve store URL provenance with the same normalization used by startup.
#[cfg(feature = "test-support")]
pub fn resolve_xmpp_database_url_for_test(
    explicit: Option<&str>,
    global: Option<&str>,
) -> crate::server::ResolvedXmppDatabaseUrl {
    crate::server::resolve_xmpp_database_url_values(explicit, global)
}

/// Opaque test handle for the production inbox startup result and its lineage.
#[cfg(feature = "test-support")]
pub struct InboxStartupForTest(crate::server::InboxStartup);

#[cfg(feature = "test-support")]
impl InboxStartupForTest {
    pub fn storage(&self) -> Arc<crate::inbox::DatabaseInboxStorage> {
        Arc::clone(self.0.storage())
    }

    pub async fn register_lineage(&self, state: &crate::server::AppState) -> anyhow::Result<()> {
        crate::server::register_inbox_lineage(state, &self.0).await
    }
}

/// Exercise the production inbox startup factory with a supplied database.
#[cfg(feature = "test-support")]
pub async fn create_inbox_startup_for_test(
    selection: &crate::server::ResolvedXmppDatabaseUrl,
    global: &Database,
) -> anyhow::Result<InboxStartupForTest> {
    crate::server::create_ingress_inbox_storage(selection, global)
        .await
        .map(InboxStartupForTest)
}

/// Consume a physically attested SQLx pool into the crate's database wrapper.
/// The pool and its opaque writer capability stay paired by construction.
#[cfg(feature = "test-support")]
pub fn database_from_attested_postgres_pool(name: &str, pool: AttestedPgPool) -> Database {
    Database::from_attested_postgres_pool(name, pool)
}

/// Wrap a physically attested pool in separate ordinary and ingress lanes.
#[cfg(feature = "test-support")]
pub async fn database_pool_from_attested_postgres_pool(
    name: &str,
    pool: AttestedPgPool,
    main_budget: u32,
    ingress_budget: u32,
) -> Result<DatabasePool, crate::db::DatabaseError> {
    DatabasePool::from_attested_postgres_pool(name, pool, main_budget, ingress_budget).await
}

/// Initialize MAM on the same SQLite pool used by a disposable fixture.
#[cfg(feature = "test-support")]
pub async fn initialize_mam_on_sqlite_test_pool(
    pool: &DatabasePool,
) -> Result<(), waddle_xmpp::mam::MamStorageError> {
    let sqlite = pool.global().sqlite_pool().ok_or_else(|| {
        waddle_xmpp::mam::MamStorageError::Database("test fixture requires SQLite".to_owned())
    })?;
    waddle_xmpp::mam::SqlxMamStorage::from_sqlite_pool(sqlite.clone()).await?;
    Ok(())
}

/// Wrap attested main/ingress and optional control-plane pools for integration
/// tests. All supplied pools must share the same attestation contract.
#[cfg(feature = "test-support")]
pub async fn database_pool_from_attested_postgres_pools(
    name: &str,
    pool: AttestedPgPool,
    control_plane_pool: Option<(AttestedPgPool, u32)>,
    main_budget: u32,
    ingress_budget: u32,
) -> Result<DatabasePool, crate::db::DatabaseError> {
    DatabasePool::from_attested_postgres_pools(
        name,
        pool,
        control_plane_pool,
        main_budget,
        ingress_budget,
    )
    .await
}

/// Read-only observations of a verified attachment with one slot per lane.
#[cfg(feature = "test-support")]
pub struct VerifiedAttachmentObservations {
    pub has_control_plane_pool: bool,
    pub has_control_system_execute: bool,
    pub control_plane_identity: Option<(String, String, String)>,
    pub second_control_plane_blocked: bool,
    pub main_available_while_control_plane_busy: bool,
    pub control_plane_available_while_main_busy: bool,
    pub second_main_blocked: bool,
    pub second_ingress_blocked: bool,
}

/// Exercise the private attachment without exposing its database or a raw
/// query interface. Transactions are read-only and remain inside this helper.
#[cfg(feature = "test-support")]
pub async fn observe_verified_attachment(
    attachment: &VerifiedGlobalAttachment,
    observer: &PgPool,
) -> Result<VerifiedAttachmentObservations, DatabaseError> {
    let database = attachment.database_for_test();
    let has_control_plane_pool = database.has_control_plane_pool();
    let has_control_system_execute = {
        let guard = database.guard().await?;
        let mut rows = guard
            .query(
                "SELECT pg_catalog.has_function_privilege(current_user, \
                 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure, 'EXECUTE')",
                (),
            )
            .await?;
        rows.next()
            .await?
            .ok_or_else(|| DatabaseError::QueryFailed("missing privilege result".to_owned()))?
            .get::<bool>(0)?
    };

    let (
        control_plane_identity,
        second_control_plane_blocked,
        main_available_while_control_plane_busy,
        control_plane_available_while_main_busy,
    ) = if has_control_plane_pool {
        let guard = database.control_plane_guard().await?;
        let mut rows = guard
            .query(
                "SELECT pg_catalog.current_database(), pg_catalog.current_schema()::text, \
                 session_user::text",
                (),
            )
            .await?;
        let identity = rows
            .next()
            .await?
            .ok_or_else(|| DatabaseError::QueryFailed("missing control-plane identity".into()))?;
        let identity = (
            identity.get::<String>(0)?,
            identity.get::<String>(1)?,
            identity.get::<String>(2)?,
        );

        // Keep a statement active on one reserved pool while checking that
        // its own capacity is bounded and the other pool remains available.
        // Observe the server-side query before probing the second slot so the
        // capacity assertion does not depend on task scheduling latency.
        let control_plane_marker = "p21_control_plane_capacity_sentinel";
        let control_plane_query =
            format!("SELECT pg_catalog.pg_sleep(1) /* {control_plane_marker} */");
        let slow_control_plane_database = database.clone();
        let slow_control_plane = tokio::spawn(async move {
            slow_control_plane_database
                .control_plane_guard()
                .await?
                .query(&control_plane_query, ())
                .await?;
            Ok::<(), DatabaseError>(())
        });
        wait_for_active_pool_query(observer, control_plane_marker).await?;
        let second_control_plane_blocked =
            tokio::time::timeout(std::time::Duration::from_millis(75), async {
                database
                    .control_plane_guard()
                    .await?
                    .query("SELECT 1", ())
                    .await
            })
            .await
            .is_err();
        let main_available_while_control_plane_busy =
            tokio::time::timeout(std::time::Duration::from_millis(150), async {
                database.guard().await?.query("SELECT 1", ()).await
            })
            .await
            .is_ok();
        slow_control_plane
            .await
            .map_err(|error| DatabaseError::QueryFailed(error.to_string()))??;

        let main_marker = "p21_main_capacity_sentinel";
        let main_query = format!("SELECT pg_catalog.pg_sleep(1) /* {main_marker} */");
        let slow_main_database = database.clone();
        let slow_main = tokio::spawn(async move {
            slow_main_database
                .guard()
                .await?
                .query(&main_query, ())
                .await?;
            Ok::<(), DatabaseError>(())
        });
        wait_for_active_pool_query(observer, main_marker).await?;
        let control_plane_available_while_main_busy =
            tokio::time::timeout(std::time::Duration::from_millis(150), async {
                database
                    .control_plane_guard()
                    .await?
                    .query("SELECT 1", ())
                    .await
            })
            .await
            .is_ok();
        slow_main
            .await
            .map_err(|error| DatabaseError::QueryFailed(error.to_string()))??;

        (
            Some(identity),
            second_control_plane_blocked,
            main_available_while_control_plane_busy,
            control_plane_available_while_main_busy,
        )
    } else {
        (None, false, false, false)
    };

    let mut main = database.begin().await?;
    main.execute("SET TRANSACTION READ ONLY", ()).await?;
    let ingress_database = database.ingress_lane();
    let mut ingress = ingress_database.begin().await?;
    ingress.execute("SET TRANSACTION READ ONLY", ()).await?;
    let second_main_blocked =
        tokio::time::timeout(std::time::Duration::from_millis(200), database.begin())
            .await
            .is_err();
    drop(main);
    let mut next_main = database.begin().await?;
    next_main.execute("SET TRANSACTION READ ONLY", ()).await?;
    let second_ingress_blocked = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        ingress_database.begin(),
    )
    .await
    .is_err();

    Ok(VerifiedAttachmentObservations {
        has_control_plane_pool,
        has_control_system_execute,
        control_plane_identity,
        second_control_plane_blocked,
        main_available_while_control_plane_busy,
        control_plane_available_while_main_busy,
        second_main_blocked,
        second_ingress_blocked,
    })
}

#[cfg(feature = "test-support")]
async fn wait_for_active_pool_query(observer: &PgPool, marker: &str) -> Result<(), DatabaseError> {
    let wait = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let active: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = pg_catalog.current_database() AND state = 'active' \
                 AND query LIKE $1)",
            )
            .bind(format!("%{marker}%"))
            .fetch_one(observer)
            .await?;
            if active {
                return Ok::<(), DatabaseError>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| DatabaseError::QueryFailed("pool capacity query did not start".into()))?;
    wait
}

/// Probe the attached control-plane lane after a test has changed trusted
/// database state. This lets PostgreSQL integration tests prove that every
/// checkout re-runs the physical attestor and fails closed on drift.
#[cfg(feature = "test-support")]
pub async fn probe_verified_control_plane(
    attachment: &VerifiedGlobalAttachment,
) -> Result<(), DatabaseError> {
    let mut rows = attachment
        .database_for_test()
        .control_plane_guard()
        .await?
        .query("SELECT 1", ())
        .await?;
    rows.next()
        .await?
        .ok_or_else(|| DatabaseError::QueryFailed("control-plane probe returned no row".into()))?;
    Ok(())
}

/// Characterize that startup-only detached sessions are closed after a
/// reserved shared-pool lane has been created.
#[cfg(feature = "test-support")]
pub async fn detached_startup_connection_is_available(database: &Database) -> bool {
    database
        .detached_postgres_startup_connection()
        .await
        .is_ok()
}

/// Wrap an ordinary PostgreSQL pool without attaching any physical writer
/// authority. This supports wrong-pool identity checks in dark tests.
#[cfg(feature = "test-support")]
pub fn database_from_unattested_postgres_pool(name: &str, pool: PgPool) -> Database {
    Database::from_postgres_pool(name, pool)
}

/// Run the dark raw ingress writer against the attested pool carried by
/// `authority`. Supplying a different `database` must fail before DML.
#[cfg(feature = "test-support")]
pub async fn attested_raw_ingress_candidate_write(
    database: &Database,
    authority: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(authority)?;
    let mut transaction = begin_raw_ingress_writer_with_capability(database, &capability).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![31_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Run the raw writer of a handle without physical attestation against a
/// disposable relation-guard candidate. That writer takes the epoch row
/// lock and installs the epoch proof but never the schema prelock, so the
/// candidate must reject its guarded write. An attested handle selects the
/// typed ingress entry by itself; this names the unattested begin directly
/// so the candidate's refusal is observed on any handle.
#[cfg(feature = "test-support")]
pub async fn legacy_ingress_candidate_write(
    database: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let mut transaction = crate::ingress_substrate::begin_unattested_raw_writer(database).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![34_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Run an attested candidate write with an intentionally stale transaction
/// epoch proof. The candidate must reject the guarded DML.
#[cfg(feature = "test-support")]
pub async fn attested_raw_candidate_write_with_epoch(
    database: &Database,
    authority: &Database,
    key: uuid::Uuid,
    epoch: &str,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(authority)?;
    let mut transaction = begin_raw_ingress_writer_with_capability(database, &capability).await?;
    transaction
        .execute(
            "SELECT set_config('waddle.protocol_epoch', ?, true)",
            crate::db_params![epoch.to_owned()],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![35_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Exercise the dark UoW entrypoint with the database's own physical writer
/// capability, without returning its raw transaction to the integration test.
#[cfg(feature = "test-support")]
pub async fn attested_uow_candidate_write(
    database: &Database,
    lineage: LineageConfig,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let unit_of_work =
        IngressUnitOfWork::open(database.clone(), lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    transaction
        .transaction_mut()
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![32_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// Record and read back a receipt through one attested UoW transaction. The
/// tuple reports exact-key presence, completion status, and receipt-key count
/// before that same caller-owned transaction commits.
#[cfg(feature = "test-support")]
pub async fn attested_uow_receipt_write(
    database: &Database,
    lineage: LineageConfig,
    key: waddle_xmpp::ingress::MessageKey,
    kind: crate::ingress_substrate::EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(bool, bool, usize), IngressUowError> {
    let unit_of_work =
        IngressUnitOfWork::open(database.clone(), lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    EffectReceiptRepository::record_receipt(&mut transaction, key, kind, hash).await?;
    let present = EffectReceiptRepository::contains(&mut transaction, key, kind, hash).await?;
    let complete = EffectReceiptRepository::receipts_complete(&mut transaction, key).await?;
    let keys = EffectReceiptRepository::keys(&mut transaction, key).await?;
    transaction.commit().await?;
    Ok((present, complete, keys.len()))
}

/// Record one canonical direct-effect receipt and scheduler descendant through
/// the same physically attested UoW transaction. An optional barrier exposes
/// only the backend PID and keeps that repository transaction open for an
/// owner-transition lock-order test.
#[cfg(feature = "test-support")]
pub async fn attested_uow_receipt_and_descendant_write(
    database: &Database,
    lineage: LineageConfig,
    key: waddle_xmpp::ingress::MessageKey,
    digest: waddle_xmpp::ingress::SemanticDigest,
    intent: waddle_xmpp::ingress::IngressEffectIntent,
    descendant: uuid::Uuid,
    barrier: Option<(
        tokio::sync::oneshot::Sender<i32>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> Result<waddle_xmpp::ingress::ProtocolEpoch, IngressUowError> {
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    };

    let unit_of_work = IngressUnitOfWork::open(database.clone(), lineage)?;
    let mut transaction = unit_of_work.begin().await?;
    let epoch = transaction.protocol_epoch();
    CanonicalMessageRepository::record_message(&mut transaction, key, &digest, None).await?;
    EffectIntentRepository::reconcile(&mut transaction, key, std::slice::from_ref(&intent), false)
        .await?;
    let receipt = crate::ingress::receipt_key(&intent)?;
    EffectReceiptRepository::record_receipt(
        &mut transaction,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?;
    EffectDescendantRepository::attach(&mut transaction, key, &intent.semantic_key(), descendant)
        .await?;

    if let Some((ready, release)) = barrier {
        let mut rows = transaction
            .transaction_mut()
            .query("SELECT pg_catalog.pg_backend_pid()", ())
            .await?;
        let pid = rows
            .next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?
            .get::<i32>(0)?;
        drop(rows);
        let _ = ready.send(pid);
        let _ = release.await;
    }

    transaction.commit().await?;
    Ok(epoch)
}

/// Simulate a lost successful COMMIT acknowledgement while retaining the
/// actual commit, then exercise the pooled receipt path's exact-key readback.
#[cfg(feature = "test-support")]
pub async fn attested_pooled_receipt_write_after_lost_ack(
    database: &Database,
    key: waddle_xmpp::ingress::MessageKey,
    kind: crate::ingress_substrate::EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(), IngressUowError> {
    with_pooled_receipt_commit_ack_lost(async {
        EffectReceiptRepository::record_receipt_pooled(database, key, kind, hash).await
    })
    .await
}

/// Hold an attested UoW's epoch row lock until the caller releases it. Only
/// the backend PID crosses the test-support boundary; the raw transaction
/// remains private to this helper.
#[cfg(feature = "test-support")]
pub async fn hold_attested_uow_epoch_lock(
    database: Database,
    lineage: LineageConfig,
    ready: tokio::sync::oneshot::Sender<i32>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), IngressUowError> {
    let unit_of_work = IngressUnitOfWork::open(database, lineage)?.with_attested_pg_writer()?;
    let mut transaction = unit_of_work.begin().await?;
    let mut rows = transaction
        .transaction_mut()
        .query("SELECT pg_backend_pid()", ())
        .await?;
    let pid = rows
        .next()
        .await?
        .ok_or(IngressUowError::EpochProofMissing)?
        .get::<i32>(0)?;
    drop(rows);
    let _ = ready.send(pid);
    let _ = release.await;
    transaction.commit().await?;
    Ok(())
}

/// Exercise the dark substrate writer entrypoint without exposing the raw
/// transaction to the integration test.
#[cfg(feature = "test-support")]
pub async fn attested_substrate_candidate_write(
    database: &Database,
    key: uuid::Uuid,
) -> Result<(), IngressUowError> {
    let capability = writer_capability(database)?;
    let substrate = PostgresIngressSubstrate::open(database.clone())?;
    let mut transaction = substrate.begin_with_attested_pg_writer(&capability).await?;
    transaction
        .execute(
            "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?::uuid, 1, ?)",
            crate::db_params![key.to_string(), vec![33_u8; 32]],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(feature = "test-support")]
fn writer_capability(
    database: &Database,
) -> Result<AttestedPostgresWriterCapability, IngressUowError> {
    database
        .attested_pg_writer_capability()
        .ok_or(IngressUowError::AttestedPoolRequired)
}

/// Overrides for production WebSocket admission in in-process tests.
#[cfg(feature = "test-support")]
pub struct WebSocketAdmissionTestConfig {
    pub capacity: crate::config::MaxWebSocketConnections,
    /// `None` is standalone; `Some(None)` represents missing clustered identity.
    pub clustered_identity: Option<Option<waddle_xmpp::ownership::SharedNodeIdentity>>,
    pub barriers: crate::websocket_transport::AdmissionTestBarriers,
    /// `None` keeps the test server's default keepalive policy.
    pub ws_keepalive: Option<waddle_xmpp::protocol::KeepaliveConfig>,
}

#[cfg(feature = "test-support")]
pub async fn websocket_state_with_admission(
    config: WebSocketAdmissionTestConfig,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            websocket_admission: Some(config),
            ..Default::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Build a minimal WebSocket state over a caller-owned database pool and ingress authority.
///
/// The ingress authority must come from [`crate::ingress::IngressAuthority::try_new`] when a test
/// drives recovery through `trigger_maintenance`, because it owns the maintenance task.
pub async fn websocket_state_with_ingress(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
) -> Arc<WebSocketState> {
    build_websocket_state_with_ingress(db_pool, move || std::future::ready(Some(ingress))).await
}

/// Build a WebSocket state over a caller-owned PostgreSQL pool and ingress
/// authority whose authorization is wired as a PostgreSQL deployment's: the
/// permission actor is fenced by the global database's host-grant store,
/// the actor and the state's host-grant gate share one enrolled set, and the
/// WebSocket routes use the app state's room registry, whose room actors
/// take the members-only binding of managed rooms. The host-grant gate is
/// bound to the ingress authority's attachment gate, so a suite attaches the
/// surfaces and acceptance together (through `attach_host_grants`, the
/// gate's verifier, or `host_grants.attach`, which reaches the same gate),
/// and a re-verification of that gate closes them together. The app state
/// names the WebSocket test state's MUC and Spaces services, so the admin
/// commands a suite registers act on the rooms and Spaces its clients see.
///
/// The enrolled set is loaded in the background; a suite that enrolls a
/// scope after building the state calls `host_grants.enrolled().refresh()`
/// instead of waiting for the next refresh interval.
///
/// # Panics
/// Panics if the pool is not PostgreSQL, or if fixture construction fails.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_host_grant_fence(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
) -> Arc<WebSocketState> {
    host_grant_fence_state(db_pool, ingress, None).await
}

/// [`websocket_state_with_host_grant_fence`] for a replica that runs the
/// S2S snapshot `config`, as startup records it when S2S is enabled.
///
/// # Panics
/// Panics if the pool is not PostgreSQL, or if fixture construction fails.
#[cfg(feature = "test-support")]
pub async fn websocket_state_with_host_grant_fence_running_s2s(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
    config: &crate::s2s::config::S2sConfig,
) -> Arc<WebSocketState> {
    host_grant_fence_state(
        db_pool,
        ingress,
        Some(crate::permissions::host_grant::FederationSnapshot::of(
            config,
        )),
    )
    .await
}

#[cfg(feature = "test-support")]
async fn host_grant_fence_state(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
    s2s_federation: Option<crate::permissions::host_grant::FederationSnapshot>,
) -> Arc<WebSocketState> {
    use crate::permissions::host_grant::{EnrolledScopes, HostGrantGate, HostGrantStore};
    use kameo::actor::Spawn;
    let database = db_pool.global().clone();
    let store =
        HostGrantStore::open(database.clone()).expect("the host-grant fence needs PostgreSQL");
    let enrolled = EnrolledScopes::watch(store.clone());
    let permission_actor = crate::permissions::PermissionActor::spawn(
        crate::permissions::PermissionActor::new_for_tests(Arc::new(database.clone()))
            .with_host_grant_authority(store.clone(), enrolled.clone()),
    );
    let host_grants = HostGrantGate::with_enrolled_scopes(store, enrolled);
    // The surfaces decide managed rooms with the attachment the ingress
    // authority's attachment gate holds, the one acceptance uses, as a
    // replica's startup binds them.
    assert!(
        host_grants.bind_attachment(ingress.host_grant_gate()),
        "a fresh host-grant gate binds the ingress authority's attachment gate"
    );
    let mut app_state = crate::server::AppState::new(Arc::clone(&db_pool));
    // The admin commands read the app state's services; they are the
    // WebSocket test state's own.
    app_state.muc_domain = "muc.example.com"
        .parse()
        .expect("test MUC domain parses as DomainPart");
    app_state.spaces_jid = "spaces.example.com"
        .parse()
        .expect("test Spaces service parses as BareJid");
    let membership: Arc<dyn waddle_xmpp::muc::affiliation::DurableMembershipSource> = Arc::new(
        crate::server::durable_membership::PermissionDurableMembershipSource::new(
            permission_actor.clone(),
        )
        .with_host_grants(host_grants.clone()),
    );
    app_state.room_registry = waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::spawn(
        waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::new(
            "muc.example.com".to_string(),
            crate::config::test_occupant_id_secret(),
        )
        .with_server_hats(app_state.server_hats.clone())
        .with_membership_source(membership),
    );
    app_state.permission_actor = permission_actor;
    app_state.host_grants = host_grants;
    app_state.s2s_federation = s2s_federation;
    let pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(
            database.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("database-backed pending delivery test storage"),
    );
    let notification_settings_projection =
        Arc::new(NotificationSettingsProjectionStore::new(database.clone()));
    NotificationOutboxStore::try_new(database)
        .await
        .expect("notification outbox test schema");
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            app_state: Some(Arc::new(app_state)),
            pending_delivery_storage: Some(pending_delivery_storage),
            notification_settings_projection: Some(notification_settings_projection),
            share_app_room_registry: true,
            ..Default::default()
        },
        move || std::future::ready(Some(ingress)),
    )
    .await
}

/// Broadcast a room's own message (a system message from the room's bare
/// JID) the way the call-thread and pin surfaces do: archived, then routed
/// to the room's seated occupants that may receive it. `Some` with the
/// assigned stanza ID when it was archived.
#[cfg(feature = "test-support")]
pub async fn broadcast_room_system_message(
    state: &WebSocketState,
    room: jid::BareJid,
    message: xmpp_parsers::message::Message,
) -> Option<String> {
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
    crate::server::routes::interpret::broadcast_room_system_message(&deps, room, Box::new(message))
        .await
}

/// Sweep one managed room now, as the revocation fence's sweep does when
/// this replica installs a new generation of the room's scope: occupants the
/// grants no longer admit are removed and affiliations the room actor
/// mirrors above the recorded ones are lowered. `true` when the room is in
/// line with its grants or this state holds no actor for it.
#[cfg(feature = "test-support")]
pub async fn sweep_host_grant_room(state: &WebSocketState, room: &jid::BareJid) -> bool {
    crate::server::host_grant_revocation::sweep_room_for_test(state, room).await
}

/// Hold the configuration lock of `room` in this process until the permit
/// is dropped, as an administration request of the room holds it from
/// before it records a change until the room actor has applied it.
#[cfg(feature = "test-support")]
pub async fn hold_room_config_lock(room: &jid::BareJid) -> tokio::sync::OwnedSemaphorePermit {
    crate::admin::channels::acquire_room_config_lock(room).await
}

/// Whether `requester` may still read `room`, decided again after a
/// protected read: `None` for a room no enrolled host-grant scope manages.
#[cfg(feature = "test-support")]
pub async fn host_grant_read_still_admitted(
    state: &WebSocketState,
    room: &jid::BareJid,
    requester: Option<&jid::BareJid>,
) -> Option<bool> {
    crate::server::routes::websocket::handlers::presence::host_grant_read_still_admitted(
        state, room, requester,
    )
    .await
}

/// Record one affiliation change of the managed room `room` as an XEP-0045
/// administration request does, with the store's commit ending as `fault`
/// says, and answer with the request's reply: `Ok` when the change is
/// recorded, otherwise the stanza error's defined condition.
///
/// # Panics
/// Panics if no enrolled scope manages `room` on this state.
#[cfg(feature = "test-support")]
pub async fn record_room_affiliation_under_commit_fault(
    state: &WebSocketState,
    room: &jid::BareJid,
    initiator: &jid::BareJid,
    target: &jid::BareJid,
    affiliation: waddle_xmpp::Affiliation,
    fault: crate::permissions::host_grant::HostGrantCommitFault,
) -> Result<(), xmpp_parsers::stanza_error::DefinedCondition> {
    use crate::permissions::host_grant::{with_host_grant_commit_fault, RoomBinding, RoomGate};
    let RoomGate::Managed(grants) = state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room))
        .await
    else {
        panic!("the room is not a managed room of this state");
    };
    with_host_grant_commit_fault(
        fault,
        crate::server::routes::websocket::handlers::iq::muc_admin_host_grant::record_host_grant_affiliation(
            state,
            &grants,
            initiator,
            &crate::permissions::host_grant::PrincipalEvidence::NoStream,
            target,
            affiliation,
        ),
    )
    .await
    .map_err(|refusal| refusal.error.defined_condition)
}

/// Record one affiliation change of a managed room as an XEP-0045
/// administration request does, against `grants`, the room's grants as the
/// request read them: a snapshot older than the scope's generation is the
/// request that another change overtook between its read and its commit.
/// Answers with the request's reply: `Ok` when the change is recorded,
/// otherwise the stanza error's defined condition.
#[cfg(feature = "test-support")]
pub async fn record_room_affiliation_with_grants(
    state: &WebSocketState,
    grants: &crate::permissions::host_grant::ManagedRoomGrants,
    initiator: &jid::BareJid,
    target: &jid::BareJid,
    affiliation: waddle_xmpp::Affiliation,
) -> Result<(), xmpp_parsers::stanza_error::DefinedCondition> {
    crate::server::routes::websocket::handlers::iq::muc_admin_host_grant::record_host_grant_affiliation(
        state,
        grants,
        initiator,
        &crate::permissions::host_grant::PrincipalEvidence::NoStream,
        target,
        affiliation,
    )
    .await
    .map_err(|refusal| refusal.error.defined_condition)
}

/// Fan out the publication of `payload` as item `item_id` on the node
/// `node` of `owner`, as a publish request does once the item is stored.
/// `is_pep` is the request's PEP classification, which is `true` for a
/// request addressed to a service as well as for one to an account.
#[cfg(feature = "test-support")]
pub async fn fan_out_published_item(
    state: &WebSocketState,
    owner: &jid::BareJid,
    node: &str,
    item_id: &str,
    payload: xmpp_parsers::minidom::Element,
    is_pep: bool,
) {
    use crate::server::routes::websocket::handlers::pubsub_fanout::{
        fan_out_publish, FanOutRequest,
    };
    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some(item_id.to_owned()),
        publisher: None,
        payload: Some(payload),
    };
    fan_out_publish(
        state,
        FanOutRequest {
            owner,
            node,
            published_item: &item,
            item_id,
            publisher: None,
            publisher_full: None,
            is_pep,
        },
    )
    .await;
}

/// Run the topology bootstrap a WebSocket server runs at startup against
/// this state: its catalog, Spaces and community nodes, room actors and
/// seeds.
#[cfg(feature = "test-support")]
pub async fn bootstrap_fresh_topology(state: &Arc<WebSocketState>) -> Result<(), String> {
    let account_domain =
        waddle_xmpp::identity::CanonicalDomain::parse_exact(&state.deps.auth_state.xmpp_domain)
            .map_err(|error| error.to_string())?;
    crate::server::topology::bootstrap_fresh_xmpp_topology(
        &state.deps.app_state,
        Arc::clone(&state.deps.protocol.pubsub_storage),
        &state.deps.service_domains,
        &account_domain,
        &state.deps.protocol.room_registry,
    )
    .await
    .map_err(|error| error.to_string())
}

/// Record `member` as a member of the group DM `group_dm_id` through the
/// group-DM member writers (the permission relation and its mirror).
#[cfg(feature = "test-support")]
pub async fn persist_group_dm_member(
    state: &crate::server::AppState,
    group_dm_id: &str,
    member: &jid::BareJid,
) -> Result<(), waddle_xmpp::XmppError> {
    crate::admin::channels::persist_group_dm_member_tuple(state, group_dm_id, member).await
}

/// Plan the extension bot of `plugin` posting `body` into `room`, as the
/// extension host does before it submits the message, and return the
/// planner's refusal: `None` when the bot was seated and the post planned.
#[cfg(feature = "test-support")]
pub async fn extension_bot_groupchat_refusal(
    state: &Arc<WebSocketState>,
    plugin: &str,
    room: &jid::BareJid,
    body: &str,
) -> Option<String> {
    use crate::server::routes::interpret::{
        plan_extension_bot_groupchat, BotOccupancy, ExtensionRoomMessage,
    };
    let plugin = waddle_extensions::PluginId::try_new(plugin).expect("plugin id");
    let bot = state
        .deps
        .service_domains
        .extension_bot_jid(&plugin)
        .expect("extension bot JID");
    let message = ExtensionRoomMessage {
        plugin,
        body: waddle_extensions::DisplayText::try_new(body).expect("message body"),
        room: waddle_extensions::RoomJid::try_new(room.to_string()).expect("room JID"),
        preferred_nick: None,
        stanza_id: None,
        thread_id: None,
        reply_to: None,
        markup: Vec::new(),
        extensions: None,
    };
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
    let mut occupancy = BotOccupancy::default();
    plan_extension_bot_groupchat(
        &deps,
        room.clone(),
        bot,
        message,
        state,
        None,
        &mut occupancy,
    )
    .await
    .err()
    .map(|refusal| refusal.to_string())
}

/// Build all fixture schemas before creating an authority that starts maintenance.
///
/// `start_ingress` runs only after the WebSocket state's final schema constructor
/// completes. The returned handle is the exact authority installed in the state,
/// so callers can bind recovery and join its real maintenance task.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_ingress_factory<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> (Arc<WebSocketState>, Arc<IngressAuthority>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = IngressAuthority>,
{
    let state = build_websocket_state_with_ingress(db_pool, move || async move {
        Some(Arc::new(start_ingress().await))
    })
    .await;
    let ingress = Arc::clone(&state.deps.protocol.ingress);
    (state, ingress)
}

async fn build_websocket_state_with_ingress<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> Arc<WebSocketState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<Arc<IngressAuthority>>>,
{
    let database = db_pool.global().clone();
    let pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(
            database.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("database-backed pending delivery test storage"),
    );
    let notification_settings_projection =
        Arc::new(NotificationSettingsProjectionStore::new(database.clone()));
    NotificationOutboxStore::try_new(database)
        .await
        .expect("notification outbox test schema");
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            pending_delivery_storage: Some(pending_delivery_storage),
            notification_settings_projection: Some(notification_settings_projection),
            ..Default::default()
        },
        start_ingress,
    )
    .await
}

/// Bind `resource` as a live local connection the way a WebSocket bind
/// does, and return the receiver of its outbound channel.
///
/// The account coordinator is the only account authority, and live delivery
/// hands a stanza only to the local registry entry that carries the binding
/// the coordinator names. A suite that drives ingress effects without a
/// socket therefore records the binding at the coordinator (published, then
/// confirmed active), registers the connection entry for that exact binding
/// and mirrors it into the `UserActor`, as registration does.
///
/// # Panics
/// Panics if the state has no selected coordinator or the coordinator does
/// not activate the binding.
pub async fn bind_live_resource(
    state: &WebSocketState,
    resource: &jid::FullJid,
    capacity: usize,
) -> tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza> {
    use crate::account_coordinator::{
        AccountBindingAuthority, ConfirmOutcome, CoordinatorBindingPublisher, PublishOutcome,
    };
    use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
    use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
    let authority = state
        .deps
        .app_state
        .account_authority()
        .authority()
        .cloned()
        .expect("the fixture selects the account coordinator");
    let publisher = CoordinatorBindingPublisher::new(authority as Arc<dyn AccountBindingAuthority>);
    let binding = BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
    let PublishOutcome::Published(published) = publisher
        .publish(resource, &binding, ResourceLifetime::fresh())
        .await
        .expect("the coordinator answers the bind")
    else {
        panic!("the coordinator did not publish the binding of {resource}");
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    let registry = &state.deps.protocol.connection_registry;
    registry.register_entry(
        resource.clone(),
        waddle_xmpp::registry::ConnectionEntry::new_for_binding(sender, binding),
    );
    assert!(
        matches!(
            publisher
                .confirm(resource, &published)
                .await
                .expect("the coordinator answers the confirmation"),
            ConfirmOutcome::Confirmed(_)
        ),
        "the coordinator did not activate the binding of {resource}"
    );
    state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: resource.clone(),
            entry: registry.entry(resource).expect("registered connection"),
        })
        .await
        .expect("registered user resource");
    receiver
}
