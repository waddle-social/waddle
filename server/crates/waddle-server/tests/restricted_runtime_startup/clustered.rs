//! Owner phase and runtime attachment for a clustered restricted runtime.
//!
//! A plan with a clustering section prepares the claim relations, the peer
//! allowlist, the keypair-pool enrollment and the durable room tables, and
//! grants the runtime the clustered privilege set. The runtime login then
//! attaches each clustering store through its attested main and
//! control-plane pools without creating anything.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use libp2p::identity::ed25519;
use libp2p::PeerId;
use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use waddle_server::clustering::allowlist::{
    prepare_peer_allowlist_owner, AllowlistError, AllowlistOwnerPreparationError, AllowlistStore,
    PostgresAllowlistStore,
};
use waddle_server::clustering::claims::{
    attach_main_claim_occupancy, prepare_claims_owner, ClaimOwnerPreparationError,
    ClaimStoreAttachError, NodeLeaseStore, PostgresClaimStore,
};
use waddle_server::clustering::lease::{
    KeypairSlotLease, KeypoolOwnerError, LeaseAttachError, LeaseIdentity, PostgresKeypairSlotLease,
};
use waddle_server::clustering::NodeId;
use waddle_server::db::bootstrap::ControlPlanePoolLimits;
use waddle_server::db::lineage::PgSchemaName;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use waddle_server::ingress_uow::{
    verify_attested_room_observation_schema, RoomObservationSchemaError,
};
use waddle_server::mam_runtime::DatabaseMamStorage;
use waddle_server::muc_durable::{
    MucOwnerPreparationError, MucRuntimeAttachError, PostgresMucRoomStore,
};
use waddle_server::room_effect_outbox::RoomEffectRuntimeAttachError;
use waddle_server::server::runtime_owner::{
    ClusteringOwnerPlan, ExclusiveDatabase, LegacyKeypairSlots, RestrictedRuntimeOwnerError,
};
use waddle_server::server::ResolvedXmppDatabaseUrl;
use waddle_server::sm_persistence_fenced::{
    FencedSmRuntimeAttachError, PostgresFencedSmPersistence,
};
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity};

use super::{
    assert_catalog_unchanged, assert_runtime_owns_nothing, attach_serving_pool,
    attach_single_node_stores, catalog_entries, execute, exercise_muc, owner_phase_stable_entries,
    relation_exists, role, run_owner_phase, startup_identity, Prepared, Provisioned, DOMAIN,
    POOL_LIMITS, SCHEMA,
};

/// A configured keypair pool: the base64 seeds the server is started with
/// and the PeerIds the owner enrolls, in the same order.
struct KeypairPool {
    seeds: Vec<String>,
    peer_ids: Vec<PeerId>,
}

fn keypair_pool(size: usize) -> KeypairPool {
    let mut seeds = Vec::new();
    let mut peer_ids = Vec::new();
    for _ in 0..size {
        let keypair = ed25519::Keypair::generate();
        seeds.push(base64::engine::general_purpose::STANDARD.encode(keypair.secret().as_ref()));
        peer_ids.push(
            libp2p::identity::Keypair::from(keypair)
                .public()
                .to_peer_id(),
        );
    }
    KeypairPool { seeds, peer_ids }
}

/// A clustering plan for a test that keeps its administrator session open.
fn clustering_plan(pool: &KeypairPool) -> ClusteringOwnerPlan {
    ClusteringOwnerPlan {
        exclusive_database: ExclusiveDatabase::NotChecked,
        ..ClusteringOwnerPlan::new(pool.peer_ids.clone(), Uuid::new_v4())
    }
}

async fn provision_clustered(url: &str, pool: &KeypairPool) -> Provisioned {
    let mut provisioned = Provisioned::new(url).await;
    provisioned.plan.clustering = Some(clustering_plan(pool));
    provisioned
}

/// The attested serving pool with the separate control-plane pool a
/// clustered runtime holds.
async fn attach_clustered_pool(prepared: &Prepared) -> DatabasePool {
    let identity = startup_identity(prepared);
    let preparation = identity
        .preflight(&prepared.runtime_url, POOL_LIMITS)
        .await
        .expect("pinned preflight under the runtime login");
    identity
        .attach_serving(
            preparation,
            &prepared.runtime_url,
            POOL_LIMITS,
            Some(ControlPlanePoolLimits {
                connections: 1,
                acquire_timeout: Duration::from_secs(10),
            }),
        )
        .await
        .expect("attested serving and control-plane pools under the runtime login")
}

async fn owner_database(owner_url: &str) -> Database {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_owned());
    config.pool_size = 2;
    Database::from_config("clustered owner", &config)
        .await
        .expect("owner database")
}

async fn allowlist_rows(admin: &PgPool) -> HashSet<String> {
    sqlx::query_scalar::<_, String>(&format!(
        "SELECT peer_id FROM {SCHEMA}.clustering_peer_allowlist"
    ))
    .fetch_all(admin)
    .await
    .expect("read peer allowlist")
    .into_iter()
    .collect()
}

async fn runtime_holds(admin: &PgPool, runtime: &str, relation: &str, privilege: &str) -> bool {
    sqlx::query_scalar(
        "SELECT pg_catalog.has_table_privilege($1, pg_catalog.format('%I.%I', $2, $3), $4)",
    )
    .bind(runtime)
    .bind(SCHEMA)
    .bind(relation)
    .bind(privilege)
    .fetch_one(admin)
    .await
    .expect("inspect runtime privilege")
}

/// The owner phase prepares every store the clustered runtime attaches, the
/// runtime role attaches each one on its attested pools and uses it, and
/// neither attaching nor a second owner phase changes the catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clustered_owner_phase_prepares_the_stores_the_runtime_role_attaches() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);
    let prepared =
        Prepared::from_provisioned(provision_clustered(&url, &keypairs).await, &url).await;
    let before = catalog_entries(&prepared.admin).await;

    let pool = attach_clustered_pool(&prepared).await;
    let main = pool.global();
    let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());

    // Claims, nodes, steal intents, reaper cursors, allowlist and occupancy
    // are verified once; each claim-fenced store consumes that evidence.
    let claims_evidence = attach_main_claim_occupancy(main.clone())
        .await
        .expect("verify claim and occupancy authority");
    let claims = Arc::new(
        PostgresClaimStore::attach_runtime(main.clone(), &claims_evidence)
            .expect("attach the claim store"),
    );
    let node = NodeIdentity::new("restricted-node", Uuid::new_v4().to_string());
    claims
        .register_with_peer_id(&node, None, Some(keypairs.peer_ids[0].to_string()))
        .await
        .expect("register the node under the runtime role");
    assert!(claims
        .heartbeat(&node, Duration::from_secs(30))
        .await
        .expect("renew the node lease under the runtime role"));
    let room = Entity::new(
        EntityType::RoomActor,
        "room@conference.localhost".to_owned(),
    );
    let epoch = claims
        .acquire(&room, &node)
        .await
        .expect("acquire a claim under the runtime role");
    assert!(claims
        .fence(&room, &node, epoch)
        .await
        .expect("read the claim back under the runtime role"));

    let allowlist = PostgresAllowlistStore::new(main.clone());
    let enrolled: HashSet<PeerId> = keypairs.peer_ids.iter().copied().collect();
    assert_eq!(
        allowlist.enrolled_peers().await.expect("read enrollment"),
        enrolled
    );
    assert_eq!(
        allowlist.effective_peers().await.expect("read admission"),
        HashSet::from([keypairs.peer_ids[0]]),
        "only the PeerId bound to a registered node is admitted"
    );
    assert!(matches!(
        allowlist.ensure_schema().await,
        Err(AllowlistError::OwnerHandleRequired)
    ));

    let lease = PostgresKeypairSlotLease::attach_existing(main.clone(), &keypairs.seeds)
        .await
        .expect("attach the keypair-slot lease");
    let holder = LeaseIdentity {
        node_id: NodeId::generate(),
        node_epoch: Uuid::new_v4(),
    };
    let slot = lease
        .acquire(&holder, keypairs.seeds.len(), Duration::from_secs(30))
        .await
        .expect("lease a keypair slot through the guard");
    lease
        .heartbeat(&holder, slot, Duration::from_secs(30))
        .await
        .expect("renew the keypair slot on the control-plane pool");
    lease
        .release(&holder, slot)
        .await
        .expect("release the keypair slot");

    let _rooms = PostgresMucRoomStore::attach_runtime(
        main.clone(),
        CancellationToken::new(),
        SharedNodeIdentity::new(node.clone()),
        &claims_evidence,
    )
    .await
    .expect("attach the durable room store");
    let _stream_management = PostgresFencedSmPersistence::attach_runtime(
        main.clone(),
        claims.clone(),
        SharedNodeIdentity::new(node.clone()),
        &claims_evidence,
    )
    .await
    .expect("attach fenced stream management");
    DatabaseMamStorage::attach(
        main,
        role(&prepared.roles.schema_owner),
        PgSchemaName(SCHEMA.to_owned()),
    )
    .await
    .expect("attach claim-fenced MAM");
    waddle_server::test_support::attach_pending_startup(&inherited, main)
        .await
        .expect("attach pending delivery");
    // The keypair guards run with the caller's privileges, so the check that
    // refuses owner-privileged triggers on runtime-writable relations holds
    // on a keypool-enrolled database.
    verify_attested_room_observation_schema(
        main,
        &prepared.lineage,
        &role(&prepared.roles.migrator),
    )
    .await
    .expect("verify room observation schema on a clustered database");

    // Evidence and stores are bound to the pools they were verified on. A
    // serving pool without the control-plane pool cannot host the claim
    // store or the keypair lease, and its handle is not the evidence's pool.
    let single = attach_serving_pool(&prepared).await;
    let single_evidence = attach_main_claim_occupancy(single.global().clone())
        .await
        .expect("verify claims on the pool without a control plane");
    assert!(matches!(
        PostgresClaimStore::attach_runtime(single.global().clone(), &single_evidence),
        Err(ClaimStoreAttachError::ControlPlanePoolRequired)
    ));
    assert!(matches!(
        PostgresClaimStore::attach_runtime(single.global().clone(), &claims_evidence),
        Err(ClaimStoreAttachError::ClaimAuthorityMismatch)
    ));
    assert!(matches!(
        PostgresKeypairSlotLease::attach_existing(single.global().clone(), &keypairs.seeds).await,
        Err(LeaseAttachError::AttestedControlPlanePoolRequired)
    ));
    assert!(matches!(
        PostgresMucRoomStore::attach_runtime(
            single.global().clone(),
            CancellationToken::new(),
            SharedNodeIdentity::new(node.clone()),
            &claims_evidence,
        )
        .await,
        Err(MucRuntimeAttachError::ClaimAuthorityMismatch)
    ));
    assert!(matches!(
        PostgresFencedSmPersistence::attach_runtime(
            single.global().clone(),
            claims.clone(),
            SharedNodeIdentity::new(node.clone()),
            &claims_evidence,
        )
        .await,
        Err(FencedSmRuntimeAttachError::ClaimAuthorityMismatch)
    ));
    drop(single_evidence);
    drop(single);

    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "clustered runtime attachment changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;
    claims.retire(&node).await.expect("retire the node");
    drop(lease);
    drop(claims);
    drop(claims_evidence);
    drop(pool);

    // A second owner phase adopts what the first one and the runtime left:
    // the catalog, the enrollment and every retained row stay as they are.
    let stable = owner_phase_stable_entries(&prepared.admin).await;
    let rows_before = clustering_row_digest(&prepared.admin).await;
    prepared
        .rerun_owner_phase()
        .await
        .expect("owner phase reruns on a clustered database");
    assert_eq!(
        owner_phase_stable_entries(&prepared.admin).await,
        stable,
        "a repeated clustered owner phase changed the catalog"
    );
    assert_eq!(clustering_row_digest(&prepared.admin).await, rows_before);

    prepared.cleanup().await;
}

/// Relations whose rows the clustered owner phase keeps on every run.
const ADOPTED_TABLES: [&str; 9] = [
    "clustering_claims",
    "clustering_nodes",
    "clustering_steal_intents",
    "clustering_orphan_reaper_cursors",
    "clustering_peer_allowlist",
    "clustering_muc_rooms",
    "clustering_muc_room_affiliations",
    "clustering_muc_room_lifecycles",
    "clustering_muc_admin_receipts",
];

/// The keypair-pool enrollment and its leases.
const KEYPAIR_TABLES: [&str; 3] = [
    "clustering_keypool",
    "clustering_keypool_slots",
    "clustering_keypair_slots",
];

/// Every row of the relations the clustered owner phase adopts, as text.
async fn clustering_row_digest(admin: &PgPool) -> Vec<String> {
    let mut digest = row_digest(admin, &ADOPTED_TABLES).await;
    digest.extend(row_digest(admin, &KEYPAIR_TABLES).await);
    digest
}

/// Every row of `tables` as text, and the claim epoch sequence position.
async fn row_digest(admin: &PgPool, tables: &[&str]) -> Vec<String> {
    let mut digest = Vec::new();
    for table in tables {
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(t)::text FROM {SCHEMA}.{table} t ORDER BY 1"
        ))
        .fetch_all(admin)
        .await
        .unwrap_or_else(|error| panic!("read {table}: {error}"));
        digest.extend(rows.into_iter().map(|row| format!("{table} {row}")));
    }
    let sequence: i64 = sqlx::query_scalar(&format!(
        "SELECT last_value FROM {SCHEMA}.clustering_claim_epoch_seq"
    ))
    .fetch_one(admin)
    .await
    .expect("read claim epoch sequence");
    digest.push(format!("clustering_claim_epoch_seq {sequence}"));
    digest
}

/// A clustered owner phase refuses a plan that disagrees with the database,
/// a database it does not have to itself, and privileges outside the matrix,
/// without changing what is there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clustered_owner_phase_fails_closed_on_a_mismatched_plan_or_database() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);
    let prepared =
        Prepared::from_provisioned(provision_clustered(&url, &keypairs).await, &url).await;
    let enrolled = allowlist_rows(&prepared.admin).await;
    let stable = owner_phase_stable_entries(&prepared.admin).await;
    let clustering = prepared
        .plan
        .clustering
        .clone()
        .expect("clustered fixture plan");

    // Another generation or another pool is not adopted, and the allowlist is
    // not reconciled to a pool the enrollment refused.
    let other_pool = keypair_pool(2);
    for (mismatched, context) in [
        (
            ClusteringOwnerPlan {
                keypool_generation: Uuid::new_v4(),
                ..clustering.clone()
            },
            "another generation",
        ),
        (
            ClusteringOwnerPlan {
                peer_ids: other_pool.peer_ids.clone(),
                ..clustering.clone()
            },
            "another keypair pool",
        ),
        (
            ClusteringOwnerPlan {
                peer_ids: clustering.peer_ids.iter().rev().copied().collect(),
                ..clustering.clone()
            },
            "another PeerId order",
        ),
    ] {
        let mut plan = prepared.plan.clone();
        plan.clustering = Some(mismatched);
        assert!(
            matches!(
                run_owner_phase(&prepared.owner_url, &plan).await,
                Err(RestrictedRuntimeOwnerError::Keypool(
                    KeypoolOwnerError::EnrollmentMismatch
                ))
            ),
            "{context} must be refused"
        );
        assert_eq!(allowlist_rows(&prepared.admin).await, enrolled, "{context}");
    }

    // A database enrolled for clustering is not prepared for a runtime that
    // serves without it.
    let mut single_node = prepared.plan.clone();
    single_node.clustering = None;
    match run_owner_phase(&prepared.owner_url, &single_node).await {
        Err(RestrictedRuntimeOwnerError::KeypoolEnrolledWithoutClusteringPlan { relation }) => {
            assert_eq!(relation, "clustering_keypool");
        }
        other => panic!("a plan without clustering was not refused: {other:?}"),
    }

    // The fixture's administrator session is another login on the database.
    let mut exclusive = prepared.plan.clone();
    exclusive.clustering = Some(ClusteringOwnerPlan {
        exclusive_database: ExclusiveDatabase::Required,
        ..clustering.clone()
    });
    match run_owner_phase(&prepared.owner_url, &exclusive).await {
        Err(RestrictedRuntimeOwnerError::DatabaseNotExclusive {
            sessions,
            prepared_transactions: 0,
        }) => assert!(sessions >= 1, "the administrator session is counted"),
        other => panic!("a shared database was not refused: {other:?}"),
    }

    // A privilege outside the clustered matrix is reported, not revoked.
    execute(
        &prepared.admin,
        &format!(
            "GRANT DELETE ON {SCHEMA}.clustering_nodes TO {}",
            prepared.roles.runtime
        ),
    )
    .await;
    match prepared.rerun_owner_phase().await {
        Err(RestrictedRuntimeOwnerError::ExcessRuntimePrivilege {
            relation,
            privilege,
        }) => assert_eq!(
            (relation.as_str(), privilege.as_str()),
            ("clustering_nodes", "DELETE")
        ),
        other => panic!("an excess clustered privilege was not refused: {other:?}"),
    }
    assert!(
        runtime_holds(
            &prepared.admin,
            &prepared.roles.runtime,
            "clustering_nodes",
            "DELETE"
        )
        .await
    );
    execute(
        &prepared.admin,
        &format!(
            "REVOKE DELETE ON {SCHEMA}.clustering_nodes FROM {}",
            prepared.roles.runtime
        ),
    )
    .await;

    // A relation the matrix does not name still fails the clustered phase.
    execute(
        &prepared.admin,
        &format!("CREATE TABLE {SCHEMA}.clustering_unclassified (id integer)"),
    )
    .await;
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {SCHEMA}.clustering_unclassified OWNER TO {}",
            prepared.roles.schema_owner
        ),
    )
    .await;
    match prepared.rerun_owner_phase().await {
        Err(RestrictedRuntimeOwnerError::UnclassifiedRelation { relation }) => {
            assert_eq!(relation, "clustering_unclassified");
        }
        other => panic!("an unclassified clustered relation was not refused: {other:?}"),
    }
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.clustering_unclassified"),
    )
    .await;

    // The adopting owner preparations are bound to the attested physical
    // identity and refuse a runtime handle.
    let mut wrong = prepared
        .lineage
        .postgres_identity
        .clone()
        .expect("PostgreSQL physical identity");
    wrong.schema.oid += 1;
    let owner = owner_database(&prepared.owner_url).await;
    assert!(matches!(
        prepare_claims_owner(&owner, &wrong).await,
        Err(ClaimOwnerPreparationError::UnsafeOwnerBoundary)
    ));
    assert!(matches!(
        prepare_peer_allowlist_owner(&owner, &wrong, &clustering.peer_ids).await,
        Err(AllowlistOwnerPreparationError::UnsafeOwnerBoundary)
    ));
    assert!(matches!(
        PostgresMucRoomStore::prepare_owner(&owner, &wrong).await,
        Err(MucOwnerPreparationError::UnsafeOwnerBoundary)
    ));
    drop(owner);
    let runtime_pool = attach_serving_pool(&prepared).await;
    let identity = prepared
        .lineage
        .postgres_identity
        .clone()
        .expect("PostgreSQL physical identity");
    assert!(matches!(
        prepare_claims_owner(runtime_pool.global(), &identity).await,
        Err(ClaimOwnerPreparationError::OwnerHandleRequired)
    ));
    assert!(matches!(
        prepare_peer_allowlist_owner(runtime_pool.global(), &identity, &clustering.peer_ids).await,
        Err(AllowlistOwnerPreparationError::OwnerHandleRequired)
    ));
    assert!(matches!(
        PostgresMucRoomStore::prepare_owner(runtime_pool.global(), &identity).await,
        Err(MucOwnerPreparationError::OwnerHandleRequired)
    ));
    drop(runtime_pool);

    // The serving room store admits the monitoring role's owner-granted
    // schema USAGE and nothing wider on the schema or on a room table.
    let serving = attach_serving_pool(&prepared).await;
    let evidence = attach_main_claim_occupancy(serving.global().clone())
        .await
        .expect("verify claim and occupancy authority");
    let attach_rooms = || {
        PostgresMucRoomStore::attach_runtime(
            serving.global().clone(),
            CancellationToken::new(),
            SharedNodeIdentity::new(NodeIdentity::local()),
            &evidence,
        )
    };
    attach_rooms()
        .await
        .expect("the prepared room schema attaches");
    for (grant, revoke, component) in [
        (
            format!(
                "GRANT USAGE ON SCHEMA {SCHEMA} TO {}",
                prepared.roles.old_runtime
            ),
            format!(
                "REVOKE USAGE ON SCHEMA {SCHEMA} FROM {}",
                prepared.roles.old_runtime
            ),
            "runtime role",
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO pg_monitor WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR USAGE ON SCHEMA {SCHEMA} FROM pg_monitor"),
            "runtime role",
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.clustering_muc_rooms TO pg_monitor"),
            format!("REVOKE SELECT ON {SCHEMA}.clustering_muc_rooms FROM pg_monitor"),
            "clustering_muc_rooms",
        ),
        (
            format!(
                "GRANT UPDATE ON {SCHEMA}.clustering_muc_admin_receipts TO {}",
                prepared.roles.runtime
            ),
            format!(
                "REVOKE UPDATE ON {SCHEMA}.clustering_muc_admin_receipts FROM {}",
                prepared.roles.runtime
            ),
            "clustering_muc_admin_receipts",
        ),
    ] {
        execute(&prepared.admin, &grant).await;
        match attach_rooms().await {
            Err(MucRuntimeAttachError::Schema(
                RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges { component: refused },
            )) => assert_eq!(refused, component, "{grant}"),
            Err(error) => panic!("{grant} had the wrong refusal: {error}"),
            Ok(_) => panic!("{grant} was accepted by the room store attachment"),
        }
        execute(&prepared.admin, &revoke).await;
    }
    attach_rooms()
        .await
        .expect("the restored room schema attaches");
    drop(evidence);
    drop(serving);

    // A keypair guard that runs with its owner's privileges on the
    // runtime-writable lease table is refused by the room observation
    // verifier; the owner phase converts the guard and verifies again.
    execute(
        &prepared.admin,
        &format!("ALTER FUNCTION {SCHEMA}.clustering_keypair_slot_guard() SECURITY DEFINER"),
    )
    .await;
    let pool = attach_serving_pool(&prepared).await;
    assert!(matches!(
        verify_attested_room_observation_schema(
            pool.global(),
            &prepared.lineage,
            &role(&prepared.roles.migrator),
        )
        .await,
        Err(RoomObservationSchemaError::UnsafeRuntime)
    ));
    prepared
        .rerun_owner_phase()
        .await
        .expect("owner phase converts an owner-privileged keypair guard");
    verify_attested_room_observation_schema(
        pool.global(),
        &prepared.lineage,
        &role(&prepared.roles.migrator),
    )
    .await
    .expect("room observation verifies once the guard runs as its caller");
    drop(pool);

    assert_eq!(
        owner_phase_stable_entries(&prepared.admin).await,
        stable,
        "the refused and repeated phases left the catalog changed"
    );
    prepared.cleanup().await;
}

/// The clustered owner phase needs the database to itself by default, keeps
/// a new generation pending for its activation delay, and refuses a lease
/// table an unattested runtime created unless the plan retires it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clustered_owner_phase_fences_activation_and_legacy_leases() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);

    // With no other login connected, the default plan passes its exclusivity
    // check. The generation stays pending for the delay: the phase reports
    // that after granting, and a rerun after the delay completes.
    let mut provisioned = Provisioned::new(&url).await;
    let delay = Duration::from_secs(2);
    provisioned.plan.clustering = Some(ClusteringOwnerPlan {
        activation_delay: delay,
        ..ClusteringOwnerPlan::new(keypairs.peer_ids.clone(), Uuid::new_v4())
    });
    provisioned.admin.close().await;
    let first = run_owner_phase(&provisioned.owner_url, &provisioned.plan).await;
    provisioned.admin =
        PgPool::connect_with(provisioned.base.clone().database(&provisioned.database))
            .await
            .expect("reconnect the database administrator");
    assert!(
        matches!(
            first,
            Err(RestrictedRuntimeOwnerError::KeypoolActivationPending)
        ),
        "a generation inside its delay must be reported pending: {first:?}"
    );
    let state: String = sqlx::query_scalar(&format!(
        "SELECT state FROM {SCHEMA}.clustering_keypool WHERE singleton"
    ))
    .fetch_one(&provisioned.admin)
    .await
    .expect("read keypool state");
    assert_eq!(state, "pending");
    assert!(
        runtime_holds(
            &provisioned.admin,
            &provisioned.roles.runtime,
            "clustering_claims",
            "INSERT"
        )
        .await,
        "the grants are in place while the generation is pending"
    );
    tokio::time::sleep(delay + Duration::from_millis(300)).await;
    let mut unchecked = provisioned.plan.clone();
    if let Some(clustering) = unchecked.clustering.as_mut() {
        clustering.exclusive_database = ExclusiveDatabase::NotChecked;
    }
    run_owner_phase(&provisioned.owner_url, &unchecked)
        .await
        .expect("the rerun after the delay activates the generation");
    let state: String = sqlx::query_scalar(&format!(
        "SELECT state FROM {SCHEMA}.clustering_keypool WHERE singleton"
    ))
    .fetch_one(&provisioned.admin)
    .await
    .expect("read keypool state");
    assert_eq!(state, "active");
    provisioned.cleanup().await;

    // An unenrolled lease table with a leased row is refused by the default
    // plan and by a retiring plan, and is left exactly as it was.
    let mut provisioned = provision_clustered(&url, &keypairs).await;
    let mut single_node = provisioned.plan.clone();
    single_node.clustering = None;
    run_owner_phase(&provisioned.owner_url, &single_node)
        .await
        .expect("prepare the database without clustering first");
    let owner = owner_database(&provisioned.owner_url).await;
    let legacy = PostgresKeypairSlotLease::initialize_owner(owner.clone())
        .await
        .expect("create the unenrolled lease table");
    let holder = LeaseIdentity {
        node_id: NodeId::generate(),
        node_epoch: Uuid::new_v4(),
    };
    legacy
        .acquire(&holder, 2, Duration::from_secs(30))
        .await
        .expect("lease a slot the way an unattested runtime does");
    drop(legacy);
    drop(owner);
    let leased_rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM {SCHEMA}.clustering_keypair_slots t"
    ))
    .fetch_all(&provisioned.admin)
    .await
    .expect("read the unenrolled lease table");
    assert!(matches!(
        run_owner_phase(&provisioned.owner_url, &provisioned.plan).await,
        Err(RestrictedRuntimeOwnerError::Keypool(
            KeypoolOwnerError::LegacyKeypairSlots
        ))
    ));
    let lease_lifetime = Duration::from_secs(30);
    if let Some(clustering) = provisioned.plan.clustering.as_mut() {
        clustering.legacy_keypair_slots = LegacyKeypairSlots::Retire {
            legacy_ttl: lease_lifetime,
        };
        clustering.activation_delay = lease_lifetime;
    }
    match run_owner_phase(&provisioned.owner_url, &provisioned.plan).await {
        Err(RestrictedRuntimeOwnerError::Keypool(KeypoolOwnerError::LegacyLeaseLive {
            holder: live,
            ..
        })) => assert_eq!(live, holder.node_id.as_str()),
        other => panic!("a leased unenrolled row was not refused: {other:?}"),
    }
    let after: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT row_to_json(t)::text FROM {SCHEMA}.clustering_keypair_slots t"
    ))
    .fetch_all(&provisioned.admin)
    .await
    .expect("read the unenrolled lease table");
    assert_eq!(after, leased_rows, "a refused retirement changed the lease");
    assert!(
        !relation_exists(&provisioned.admin, "clustering_keypool").await,
        "a refused retirement must not enroll"
    );
    provisioned.cleanup().await;
}

/// The stream management preparation the clustered runtime relies on builds
/// the replay de-duplication index over retained rows, and a retained
/// duplicate fails the phase before any runtime grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_builds_the_replay_dedup_index_over_retained_rows_or_fails_closed() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);
    let provisioned = provision_clustered(&url, &keypairs).await;

    // A database that served without clustering, holding replay rows in a
    // schema without the index.
    let mut single_node = provisioned.plan.clone();
    single_node.clustering = None;
    run_owner_phase(&provisioned.owner_url, &single_node)
        .await
        .expect("prepare the database without clustering first");
    execute(
        &provisioned.admin,
        &format!("DROP INDEX {SCHEMA}.idx_sm_unacked_dedup"),
    )
    .await;
    let insert = |sequence: i64, inbound: i64| {
        format!(
            "INSERT INTO {SCHEMA}.sm_unacked \
             (stream_id, sequence, stanza_xml, original_receipt_at_ms, origin_stream_id, \
              inbound_seq) \
             VALUES ('retained-stream', {sequence}, '<message/>', 1, 'origin-stream', {inbound})"
        )
    };
    execute(&provisioned.admin, &insert(1, 7)).await;
    execute(&provisioned.admin, &insert(2, 7)).await;

    match run_owner_phase(&provisioned.owner_url, &provisioned.plan).await {
        Err(RestrictedRuntimeOwnerError::StreamManagement(_)) => {}
        other => panic!("a retained duplicate did not fail the phase: {other:?}"),
    }
    let index_exists = |admin: PgPool| async move {
        sqlx::query_scalar::<_, bool>("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
            .bind(format!("{SCHEMA}.idx_sm_unacked_dedup"))
            .fetch_one(&admin)
            .await
            .expect("inspect the de-duplication index")
    };
    assert!(!index_exists(provisioned.admin.clone()).await);
    let retained: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}.sm_unacked"))
        .fetch_one(&provisioned.admin)
        .await
        .expect("count retained replay rows");
    assert_eq!(retained, 2, "the failed phase must keep every retained row");
    // The phase stopped before any clustering store or clustered grant.
    assert!(!relation_exists(&provisioned.admin, "clustering_claims").await);
    assert!(!relation_exists(&provisioned.admin, "clustering_keypool").await);

    // Without the duplicate the same phase builds the index over the row
    // that remains and completes.
    execute(
        &provisioned.admin,
        &format!("DELETE FROM {SCHEMA}.sm_unacked WHERE sequence = 2"),
    )
    .await;
    run_owner_phase(&provisioned.owner_url, &provisioned.plan)
        .await
        .expect("owner phase completes over duplicate-free retained rows");
    assert!(index_exists(provisioned.admin.clone()).await);
    let retained: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}.sm_unacked"))
        .fetch_one(&provisioned.admin)
        .await
        .expect("count retained replay rows");
    assert_eq!(retained, 1);
    provisioned.cleanup().await;
}

/// Start the server the way a deployment without the restricted role runs
/// it: clustered, under the credential that owns the schema, creating every
/// relation itself.
async fn start_owner_capable_cluster_node(
    provisioned: &Provisioned,
    keypairs: &KeypairPool,
    lease_lifetime: Duration,
) -> TestServer {
    let swarm_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener.local_addr().expect("local address").port()
    };
    let deployment = provisioned
        .plan
        .lineage
        .deployment_uuid
        .expect("fixture deployment identity")
        .0
        .to_string();
    let lease_ttl = lease_lifetime.as_millis().to_string();
    let heartbeat = (lease_lifetime.as_millis() / 3).to_string();
    let env: Vec<(&'static str, String)> = vec![
        ("WADDLE_DB_DRIVER", "postgres".to_owned()),
        ("WADDLE_DATABASE_URL", provisioned.owner_url.clone()),
        ("WADDLE_DEPLOYMENT_UUID", deployment),
        ("WADDLE_DB_LINEAGE_ACTION", "enroll".to_owned()),
        ("WADDLE_CLUSTERING_ENABLED", "true".to_owned()),
        (
            "WADDLE_CLUSTERING_LISTEN_ADDRS",
            format!("/ip4/127.0.0.1/tcp/{swarm_port}"),
        ),
        ("WADDLE_CLUSTERING_KEYPAIR_POOL", keypairs.seeds.join(",")),
        ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", heartbeat),
        ("WADDLE_CLUSTERING_LEASE_TTL_MS", lease_ttl),
    ];
    tokio::task::spawn_blocking(move || {
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        TestServer::start_with_extra_envs(&[("alice", "alice-password")], &env)
    })
    .await
    .expect("owner-capable clustered server starts")
}

/// The production upgrade path: a database whose every relation was created
/// by a clustered server running under the schema owner is adopted by the
/// owner phase without losing a retained row, and then passes each exact
/// verifier the restricted runtime attaches through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clustered_owner_phase_adopts_a_database_an_owner_capable_runtime_built() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);
    let mut provisioned = provision_clustered(&url, &keypairs).await;
    let lease_lifetime = Duration::from_millis(1500);

    // The deployment pipeline enrolls the allowlist before the first node.
    let owner = owner_database(&provisioned.owner_url).await;
    PostgresAllowlistStore::new(owner.clone())
        .ensure_schema()
        .await
        .expect("create the peer allowlist");
    for peer in &keypairs.peer_ids {
        owner
            .guard()
            .await
            .expect("owner connection")
            .execute(
                "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
                waddle_server::db_params![peer.to_string()],
            )
            .await
            .expect("enroll a peer");
    }
    drop(owner);

    let server = start_owner_capable_cluster_node(&provisioned, &keypairs, lease_lifetime).await;
    let mut alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        "alice-password",
        "laptop",
    )
    .await
    .expect("alice authenticates on the owner-capable server");
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "console",
    )
    .await
    .expect("admin authenticates on the owner-capable server");
    exercise_muc(&mut admin, &mut alice).await;
    drop(alice);
    drop(admin);
    // The process is killed, as a node that crashes mid-rollout is: its
    // keypair lease stays held until the lease lifetime runs out.
    drop(server);

    for table in [
        "clustering_muc_rooms",
        "clustering_claims",
        "clustering_nodes",
    ] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}.{table}"))
            .fetch_one(&provisioned.admin)
            .await
            .unwrap_or_else(|error| panic!("count {table}: {error}"));
        assert!(
            rows > 0,
            "the owner-capable runtime must leave rows in {table}"
        );
    }
    let retained = row_digest(&provisioned.admin, &ADOPTED_TABLES).await;
    let archived: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}.mam_messages"))
        .fetch_one(&provisioned.admin)
        .await
        .expect("count archived messages");
    assert!(archived > 0);

    // The unenrolled lease table is retired only once the killed node's
    // lease has run out on the database clock. A plan that assumes a longer
    // lease lifetime than the node ran with still sees the row as leased.
    let assumed_lifetime = Duration::from_secs(120);
    if let Some(clustering) = provisioned.plan.clustering.as_mut() {
        clustering.legacy_keypair_slots = LegacyKeypairSlots::Retire {
            legacy_ttl: assumed_lifetime,
        };
        clustering.activation_delay = assumed_lifetime;
    }
    let first = run_owner_phase(&provisioned.owner_url, &provisioned.plan).await;
    assert!(
        matches!(
            first,
            Err(RestrictedRuntimeOwnerError::Keypool(
                KeypoolOwnerError::LegacyLeaseLive { .. }
            ))
        ),
        "the killed node's lease must still fence the retirement: {first:?}"
    );
    // With the lifetime the node ran with, the lease runs out, the table is
    // retired, and the new generation stays pending for one more lifetime.
    if let Some(clustering) = provisioned.plan.clustering.as_mut() {
        clustering.legacy_keypair_slots = LegacyKeypairSlots::Retire {
            legacy_ttl: lease_lifetime,
        };
        clustering.activation_delay = lease_lifetime;
    }
    tokio::time::sleep(lease_lifetime + Duration::from_millis(300)).await;
    let second = run_owner_phase(&provisioned.owner_url, &provisioned.plan).await;
    assert!(
        matches!(
            second,
            Err(RestrictedRuntimeOwnerError::KeypoolActivationPending)
        ),
        "the new generation must stay pending for its delay: {second:?}"
    );
    tokio::time::sleep(lease_lifetime + Duration::from_millis(300)).await;
    let lineage = run_owner_phase(&provisioned.owner_url, &provisioned.plan)
        .await
        .expect("owner phase adopts the owner-capable runtime's database");

    assert_eq!(
        row_digest(&provisioned.admin, &ADOPTED_TABLES).await,
        retained,
        "adoption must keep every retained clustering and room row"
    );
    let archived_after: i64 =
        sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}.mam_messages"))
            .fetch_one(&provisioned.admin)
            .await
            .expect("count archived messages");
    assert_eq!(archived_after, archived);

    let prepared = Prepared::attest(provisioned, lineage, &url).await;
    let before = catalog_entries(&prepared.admin).await;
    let pool = attach_clustered_pool(&prepared).await;
    let main = pool.global();
    let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());

    let claims_evidence = attach_main_claim_occupancy(main.clone())
        .await
        .expect("adopted claim and occupancy authority verifies");
    let claims = Arc::new(
        PostgresClaimStore::attach_runtime(main.clone(), &claims_evidence)
            .expect("attach the claim store"),
    );
    let node = NodeIdentity::new("restricted-node", Uuid::new_v4().to_string());
    let lease = PostgresKeypairSlotLease::attach_existing(main.clone(), &keypairs.seeds)
        .await
        .expect("rebuilt keypair-slot lease verifies");
    let holder = LeaseIdentity {
        node_id: NodeId::generate(),
        node_epoch: Uuid::new_v4(),
    };
    let slot = lease
        .acquire(&holder, keypairs.seeds.len(), Duration::from_secs(30))
        .await
        .expect("lease a keypair slot on the adopted database");
    lease
        .release(&holder, slot)
        .await
        .expect("release the keypair slot");
    PostgresMucRoomStore::attach_runtime(
        main.clone(),
        CancellationToken::new(),
        SharedNodeIdentity::new(node.clone()),
        &claims_evidence,
    )
    .await
    .expect("adopted durable room schema verifies");
    PostgresFencedSmPersistence::attach_runtime(
        main.clone(),
        claims.clone(),
        SharedNodeIdentity::new(node.clone()),
        &claims_evidence,
    )
    .await
    .expect("adopted stream management schema verifies");
    DatabaseMamStorage::attach(
        main,
        role(&prepared.roles.schema_owner),
        PgSchemaName(SCHEMA.to_owned()),
    )
    .await
    .expect("adopted MAM schema verifies");
    attach_single_node_stores(&prepared, &pool, &inherited).await;

    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "attachment changed the adopted catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;
    drop(lease);
    drop(claims);
    drop(claims_evidence);
    drop(pool);

    // The adopted database is a fixed point of the owner phase.
    let stable = owner_phase_stable_entries(&prepared.admin).await;
    let rows = clustering_row_digest(&prepared.admin).await;
    prepared
        .rerun_owner_phase()
        .await
        .expect("owner phase reruns on the adopted database");
    assert_eq!(
        owner_phase_stable_entries(&prepared.admin).await,
        stable,
        "a repeated owner phase changed the adopted catalog"
    );
    assert_eq!(clustering_row_digest(&prepared.admin).await, rows);
    prepared.cleanup().await;
}
