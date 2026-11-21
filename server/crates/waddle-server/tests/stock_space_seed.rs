//! Provision the restricted Waddle side of the isolated ejabberd interop row.
#![cfg(feature = "test-support")]

#[allow(dead_code)]
#[path = "restricted_runtime_support/mod.rs"]
mod support;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

use kameo::actor::Spawn;
use waddle_server::config::SpiceDbConfig;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::permissions::host_grant::{
    BindSpace, BootstrapGrant, EnrollScope, GrantOperationId, GrantSubject, HostGrantAuthority,
    HostGrantStore, IssuancePolicy, ManagedRoom, Reconciliation, RoomAffiliation, ScopeKey,
    ScopeLifecycleId,
};
use waddle_server::permissions::{
    EnsureAuthzCodecIncarnation, EnsureSchema, PermissionActor, RecordAuthzSchemaVersion,
};
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::pubsub::{NodeConfig, PubSubStorage};
use waddle_xmpp_core::pubsub::PubSubNodeType;

const HOME: &str = "home-a.test";
const SPACES: &str = "spaces.home-a.test";
const MUC: &str = "muc.home-a.test";

fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing {key}"))
}

fn isolated_postgres() -> String {
    let value = required("INTEROP_ENROLLED_SPACE_POSTGRES_URL");
    let url = url::Url::parse(&value).expect("fixture PostgreSQL URL");
    assert_eq!(url.scheme(), "postgresql");
    assert!(url.host().is_none() && url.username().is_empty() && url.password().is_none());
    assert_eq!(url.path(), "/waddle");
    assert!(url.fragment().is_none());
    let pairs = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(url.query_pairs().count(), 3);
    assert_eq!(
        pairs.get("user").map(|value| value.as_ref()),
        Some("waddle_harness")
    );
    let socket = Path::new(
        pairs
            .get("host")
            .expect("private socket directory")
            .as_ref(),
    );
    assert!(socket.is_absolute() && socket.ends_with("owner-socket"));
    let permissions = socket.symlink_metadata().expect("fixture socket directory");
    assert!(permissions.is_dir());
    assert_eq!(permissions.permissions().mode() & 0o777, 0o700);
    let port: u16 = pairs
        .get("port")
        .expect("fixture port")
        .parse()
        .expect("port number");
    assert!(socket
        .join(format!(".s.PGSQL.{port}"))
        .symlink_metadata()
        .expect("fixture PostgreSQL socket")
        .file_type()
        .is_socket());
    value
}

fn isolated_spicedb() -> SpiceDbConfig {
    let endpoint = required("INTEROP_ENROLLED_SPACE_SPICEDB_ENDPOINT");
    let url = url::Url::parse(&endpoint).expect("fixture SpiceDB endpoint");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(url.port().is_some());
    assert!(url.username().is_empty() && url.password().is_none());
    assert_eq!(url.path(), "/");
    assert!(url.query().is_none() && url.fragment().is_none());
    SpiceDbConfig {
        endpoint,
        preshared_key: required("INTEROP_ENROLLED_SPACE_SPICEDB_KEY"),
        insecure: true,
    }
}

#[tokio::test]
#[ignore = "spawned only by the isolated stock federation harness"]
async fn provision_enrolled_space_for_stock_peer() {
    assert_eq!(required("INTEROP_ENROLLED_SPACE_DISPOSABLE"), "1");
    let postgres = isolated_postgres();
    let spice = isolated_spicedb();
    let node = required("INTEROP_ENROLLED_SPACE_NODE");
    assert!(node.starts_with("stock-enrolled-space-"));
    let service: jid::BareJid = SPACES.parse().expect("Spaces service");
    let key = ScopeKey::new(service.clone(), &node).expect("fixture Space key");

    let provisioned = support::Provisioned::new(&postgres).await;
    let prepared = support::Prepared::from_provisioned(provisioned, &postgres).await;
    let owner_database = Database::from_config(
        "stock-space-owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, prepared.owner_url.clone()),
    )
    .await
    .expect("owner database");
    let pubsub = DatabasePubSubStorage::initialize_shared_owner(owner_database.clone())
        .await
        .expect("owner PubSub store");
    pubsub
        .get_or_create_node(&service, &node)
        .await
        .expect("create backing node");
    pubsub
        .update_node_config(
            &service,
            &node,
            &NodeConfig {
                node_type: Some(PubSubNodeType::Space),
                ..NodeConfig::spaces_private()
            },
        )
        .await
        .expect("configure XEP-0503 node");

    let mut config = waddle_server::ServerConfig::test_homeserver();
    config.spicedb = Some(spice);
    let store = HostGrantStore::open(owner_database).expect("canonical grant store");
    let permissions = PermissionActor::spawn(
        PermissionActor::from_server_config(&config)
            .await
            .expect("connect isolated SpiceDB")
            .with_host_grant_store(store.clone()),
    );
    permissions
        .ask(EnsureSchema)
        .await
        .expect("permission schema");
    permissions
        .ask(EnsureAuthzCodecIncarnation)
        .await
        .expect("permission codec incarnation");
    permissions
        .ask(RecordAuthzSchemaVersion)
        .await
        .expect("record schema version");
    let domain = |value| CanonicalDomain::parse_exact(value).expect("canonical fixture domain");
    let policy = IssuancePolicy::new(
        [domain(HOME)],
        [domain(SPACES), domain(MUC)],
        std::iter::empty(),
    )
    .expect("fixture issuance policy");
    let owner = GrantSubject::new(format!("admin@{HOME}").parse().expect("owner JID"))
        .expect("canonical owner");
    let room: jid::BareJid = format!("{node}@{MUC}").parse().expect("room JID");
    let lifecycle = ScopeLifecycleId::generate();
    let authority = HostGrantAuthority::new(store.clone(), permissions, policy.clone());
    let enrolled = authority
        .enroll_scope(&EnrollScope {
            operation: GrantOperationId::generate(),
            lifecycle,
            key,
            rooms: vec![ManagedRoom::new(room.clone()).expect("managed fixture room")],
            bootstrap: vec![BootstrapGrant {
                room,
                subject: owner.clone(),
                affiliation: RoomAffiliation::Owner,
            }],
        })
        .await
        .expect("canonical scope enrollment");
    assert!(matches!(
        authority.reconcile(lifecycle).await.expect("project room"),
        Reconciliation::Activated(_)
    ));
    store
        .bind_space(
            &BindSpace {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: enrolled.record().generation,
                owner,
                federated: true,
            },
            policy.served_account_domains(),
        )
        .await
        .expect("bind enrolled Space");
    assert!(matches!(
        authority.reconcile(lifecycle).await.expect("project owner"),
        Reconciliation::Activated(_)
    ));

    // Use the harness's private socket, which outlives this seed process.
    let runtime_url = support::url_with(
        &postgres,
        &prepared.database,
        &prepared.roles.runtime,
        &format!("-c search_path={}", support::SCHEMA),
    );
    let handoff = prepared.handoff_for(&runtime_url).await;
    let environment: std::collections::BTreeMap<_, _> = prepared
        .runtime_env_for(&runtime_url, &handoff)
        .into_iter()
        .collect();
    println!(
        "INTEROP_ENROLLED_SPACE_RUNTIME {}",
        serde_json::to_string(&environment).expect("runtime handoff")
    );
}
