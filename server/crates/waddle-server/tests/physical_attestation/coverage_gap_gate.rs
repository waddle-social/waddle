//! The dark coverage inventory refuses every described target.

use std::collections::HashSet;

#[cfg(feature = "clustering")]
use waddle_server::config::ServerMode;
use waddle_server::config::{ServerConfig, SpiceDbConfig};
use waddle_server::db::physical_attestation::{
    inspect_coverage_gap_gate_v1, CoverageGapId, CoverageRefusal, CoverageTarget,
    CoverageTargetError, CoverageTargetStore, COVERAGE_GAPS_V1,
};
use waddle_server::db::DatabaseDriver;
use waddle_server::server::{ResolvedXmppDatabaseUrl, XmppConfig};

const GLOBAL: &str = "postgresql://fixture.invalid/waddle";
const STORES: [CoverageTargetStore; 7] = [
    CoverageTargetStore::Mam,
    CoverageTargetStore::Inbox,
    CoverageTargetStore::PubSub,
    CoverageTargetStore::SpacesMetadata,
    CoverageTargetStore::ChannelSpaceLinks,
    CoverageTargetStore::StreamManagement,
    CoverageTargetStore::PendingDelivery,
];

fn description() -> (ServerConfig, XmppConfig) {
    let mut server = ServerConfig::test_homeserver();
    server.spicedb = Some(SpiceDbConfig {
        endpoint: "http://fixture.invalid".to_owned(),
        preshared_key: "fixture-only".to_owned(),
        insecure: true,
    });
    server.extensions.enabled = false;
    let mut xmpp = XmppConfig::default();
    for store in STORES {
        set_location(
            &mut xmpp,
            store,
            ResolvedXmppDatabaseUrl::InheritedGlobal(GLOBAL.to_owned()),
        );
    }
    (server, xmpp)
}

fn set_location(xmpp: &mut XmppConfig, store: CoverageTargetStore, value: ResolvedXmppDatabaseUrl) {
    let location = match store {
        CoverageTargetStore::Mam => &mut xmpp.mam_database_url,
        CoverageTargetStore::Inbox => &mut xmpp.inbox_database_url,
        CoverageTargetStore::PubSub => &mut xmpp.pubsub_database_url,
        CoverageTargetStore::SpacesMetadata => &mut xmpp.spaces_metadata_database_url,
        CoverageTargetStore::ChannelSpaceLinks => &mut xmpp.channel_space_links_database_url,
        CoverageTargetStore::StreamManagement => &mut xmpp.sm_database_url,
        CoverageTargetStore::PendingDelivery => &mut xmpp.pending_delivery_database_url,
    };
    *location = value;
}

fn target<'a>(server: &'a ServerConfig, xmpp: &'a XmppConfig) -> CoverageTarget<'a> {
    CoverageTarget {
        driver: DatabaseDriver::Postgres,
        server_version_num: 170_000,
        global_database_url: GLOBAL,
        database_owner_role_oid: 10,
        schema_owner_role_oid: 11,
        server,
        xmpp,
    }
}

#[test]
fn known_gaps_are_unique_and_evidence_files_exist() {
    let expected = HashSet::from([
        CoverageGapId::JointRoleGraph,
        CoverageGapId::GlobalMigrations,
        CoverageGapId::IdentityAndSessions,
        CoverageGapId::RosterBlockingProfileUploads,
        CoverageGapId::PermissionBackend,
        CoverageGapId::Inbox,
        CoverageGapId::StreamManagement,
        CoverageGapId::PendingDelivery,
        CoverageGapId::IngressEpoch,
        CoverageGapId::ReceiptJournal,
        CoverageGapId::PubSubProjections,
        CoverageGapId::MamAndClaimPair,
        CoverageGapId::SpacesMetadata,
        CoverageGapId::ChannelSpaceLinks,
        CoverageGapId::RoomCatalog,
        CoverageGapId::MucFixtureTarget,
        CoverageGapId::MucDestroyOutbox,
        CoverageGapId::RoomEffectOutbox,
        CoverageGapId::PushRegistrations,
        CoverageGapId::PushService,
        CoverageGapId::Vapid,
        CoverageGapId::NotificationOutbox,
        CoverageGapId::NotificationActivity,
        CoverageGapId::CallTeardown,
        CoverageGapId::ExtensionData,
    ]);
    let ids: HashSet<_> = COVERAGE_GAPS_V1.iter().map(|gap| gap.id).collect();
    assert_eq!(ids.len(), COVERAGE_GAPS_V1.len(), "duplicate coverage gap");
    assert_eq!(ids, expected);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("server directory")
        .parent()
        .expect("repository root");
    for gap in COVERAGE_GAPS_V1 {
        assert!(!gap.problem.is_empty());
        assert!(!gap.sources.is_empty());
        let mut evidence = String::new();
        for source in gap.sources {
            let text = std::fs::read_to_string(root.join(source))
                .unwrap_or_else(|error| panic!("cannot read evidence file {source}: {error}"));
            evidence.push_str(&text);
        }
        for example in gap.examples {
            assert!(
                evidence.contains(*example),
                "missing source evidence for {example}"
            );
        }
    }
}

#[test]
fn unsupported_version_and_build_are_refused() {
    let (server, xmpp) = description();
    let mut target = target(&server, &xmpp);
    target.driver = DatabaseDriver::Sqlite;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target),
        CoverageRefusal::Unsupported(CoverageTargetError::Postgres17Required)
    );
    target.driver = DatabaseDriver::Postgres;
    target.server_version_num = 160_000;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target),
        CoverageRefusal::Unsupported(CoverageTargetError::Postgres17Required)
    );
    target.server_version_num = 180_000;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target),
        CoverageRefusal::Unsupported(CoverageTargetError::Postgres17Required)
    );
    #[cfg(not(feature = "clustering"))]
    {
        target.server_version_num = 170_000;
        assert_eq!(
            inspect_coverage_gap_gate_v1(&target),
            CoverageRefusal::Unsupported(CoverageTargetError::ClusteringFeatureRequired)
        );
    }
}

#[cfg(feature = "clustering")]
#[test]
fn target_shape_always_refuses_with_the_known_gap_inventory() {
    let (server, xmpp) = description();
    let target = target(&server, &xmpp);
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target),
        CoverageRefusal::Incomplete(COVERAGE_GAPS_V1)
    );
}

#[cfg(feature = "clustering")]
#[test]
fn rejects_child_location_provenance_and_mismatch_for_every_store() {
    let (server, mut xmpp) = description();
    for store in STORES {
        // An explicitly supplied identical DSN is not inherited provenance.
        set_location(
            &mut xmpp,
            store,
            ResolvedXmppDatabaseUrl::Explicit(GLOBAL.to_owned()),
        );
        assert_eq!(
            inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
            CoverageRefusal::Unsupported(CoverageTargetError::ChildMustInheritGlobal(store))
        );
        set_location(&mut xmpp, store, ResolvedXmppDatabaseUrl::EphemeralFallback);
        assert_eq!(
            inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
            CoverageRefusal::Unsupported(CoverageTargetError::ChildMustInheritGlobal(store))
        );
        set_location(
            &mut xmpp,
            store,
            ResolvedXmppDatabaseUrl::InheritedGlobal(
                "postgresql://different.invalid/waddle".to_owned(),
            ),
        );
        assert_eq!(
            inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
            CoverageRefusal::Unsupported(CoverageTargetError::ChildDiffersFromGlobal(store))
        );
        set_location(
            &mut xmpp,
            store,
            ResolvedXmppDatabaseUrl::InheritedGlobal(GLOBAL.to_owned()),
        );
    }
}

#[cfg(feature = "clustering")]
#[test]
fn rejects_known_unsupported_configuration_and_owner_pins() {
    let (mut server, mut xmpp) = description();
    server.clustering.enabled = true;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::ClusteringMustBeDisabled)
    );
    server.clustering.enabled = false;
    server.mode = ServerMode::Standalone;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::HomeServerRequired)
    );
    server.mode = ServerMode::HomeServer;
    xmpp.native_auth_enabled = false;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::XmppAndNativeAuthRequired)
    );
    xmpp.native_auth_enabled = true;
    xmpp.enabled = false;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::XmppAndNativeAuthRequired)
    );
    xmpp.enabled = true;
    let spicedb = server.spicedb.take();
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::SpiceDbRequired)
    );
    server.spicedb = spicedb;
    server.auth.providers.push(
        serde_json::from_value(serde_json::json!({
            "id": "fixture", "display_name": "Fixture", "kind": "oidc",
            "client_id": "fixture", "scopes": []
        }))
        .expect("auth provider fixture"),
    );
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::ExternalAuthProvidersUnsupported)
    );
    server.auth.providers.clear();
    server.extensions.enabled = true;
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::ExtensionsUnsupported)
    );
    server.extensions.enabled = false;
    server.extensions.modules.push(
        serde_json::from_value(serde_json::json!({ "name": "fixture" }))
            .expect("extension fixture"),
    );
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::ExtensionsUnsupported)
    );
    server.extensions.modules.clear();
    server.apns = waddle_server::push_service::ApnsConfig::from_vars([
        ("WADDLE_APNS_KEY_PATH", "/fixture/key.p8"),
        ("WADDLE_APNS_TEAM_ID", "TEAM123456"),
        ("WADDLE_APNS_KEY_ID", "KEY1234567"),
        ("WADDLE_APNS_BUNDLE_ID", "example.test"),
    ])
    .expect("valid APNs fixture");
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::PushProvidersUnsupported)
    );
    server.apns = None;
    server.fcm = waddle_server::push_service::FcmConfig::from_vars([
        ("WADDLE_FCM_PROJECT_ID", "waddle-social"),
        ("WADDLE_FCM_SERVICE_ACCOUNT_FILE", "/fixture/fcm.json"),
        ("WADDLE_FCM_APP_ID", "social.waddle.android"),
    ])
    .expect("valid FCM fixture");
    assert_eq!(
        inspect_coverage_gap_gate_v1(&target(&server, &xmpp)),
        CoverageRefusal::Unsupported(CoverageTargetError::PushProvidersUnsupported)
    );
    server.fcm = None;
    let mut candidate = target(&server, &xmpp);
    candidate.global_database_url = " ";
    assert_eq!(
        inspect_coverage_gap_gate_v1(&candidate),
        CoverageRefusal::Unsupported(CoverageTargetError::EmptyGlobalUrl)
    );
    candidate.global_database_url = GLOBAL;
    for (database_owner, schema_owner) in [(0, 11), (10, 0), (10, 10)] {
        candidate.database_owner_role_oid = database_owner;
        candidate.schema_owner_role_oid = schema_owner;
        assert_eq!(
            inspect_coverage_gap_gate_v1(&candidate),
            CoverageRefusal::Unsupported(CoverageTargetError::OwnerPinsMustBeNonzeroAndDistinct)
        );
    }
}

/// Inspect representative facts in the existing combined owner fixture. This
/// does not turn those facts, or the gap list, into an exhaustive catalog proof.
#[cfg(feature = "clustering")]
pub(super) async fn exercise_combined_fixture(
    admin: &sqlx::PgPool,
    database_owner: &str,
    schema_owner: &str,
    main: &waddle_server::db::Database,
) -> anyhow::Result<()> {
    use anyhow::ensure;
    use sqlx::postgres::types::Oid;

    let (version, database_oid, schema_oid): (i32, Oid, Oid) = sqlx::query_as(
        "SELECT pg_catalog.current_setting('server_version_num')::integer, d.datdba, n.nspowner \
         FROM pg_catalog.pg_database d JOIN pg_catalog.pg_namespace n ON n.nspname='guarded' \
         WHERE d.datname=pg_catalog.current_database()",
    )
    .fetch_one(admin)
    .await?;
    ensure!(
        database_oid != schema_oid,
        "combined fixture must have distinct owners"
    );
    let (server, xmpp) = description();
    let mut candidate = target(&server, &xmpp);
    candidate.server_version_num = u32::try_from(version)?;
    candidate.database_owner_role_oid = database_oid.0;
    candidate.schema_owner_role_oid = schema_oid.0;
    ensure!(
        matches!(inspect_coverage_gap_gate_v1(&candidate), CoverageRefusal::Incomplete(gaps) if gaps == COVERAGE_GAPS_V1)
    );

    // Exercise the real attested selector with the requested disabled-cluster
    // target. The refusal happens before MAM attachment or any claim reads.
    let handoff = waddle_server::server::MamOwnerClaimHandoff::new(
        waddle_server::db::physical_attestation::PgRoleName::new(schema_owner)?,
        waddle_server::db::lineage::PgSchemaName("guarded".to_owned()),
    )?;
    let error = waddle_server::test_support::select_mam_storage_for_test(
        &xmpp.mam_database_url,
        Some(&handoff),
        false,
        false,
        main,
    )
    .await
    .expect_err("clustering-disabled attested MAM must refuse without a claim pair");
    ensure!(
        error.to_string()
            == "attested MAM storage requires live clustering claim handles in clustering builds",
        "MAM failed for an unexpected reason: {error}"
    );
    ensure!(matches!(
        waddle_server::room_effect_outbox::RoomEffectOutboxStore::try_new(main.clone()).await,
        Err(waddle_server::room_effect_outbox::RoomEffectOutboxError::OwnerPreparationRequired)
    ));
    ensure!(matches!(
        waddle_server::notification_activity::NotificationActivityStore::try_new(main.clone()).await,
        Err(waddle_server::notification_activity::NotificationActivityError::OwnerBootstrapRequiresOwnerHandle)
    ));
    ensure!(matches!(
        waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(main.clone()).await,
        Err(waddle_xmpp::push::PushError::StorageError(message))
            if message == "push-registration owner initialization cannot use an attested runtime handle"
    ));

    for name in [
        "_lineage",
        "_migrations",
        "users",
        "auth_identities",
        "ingress_messages",
        "pubsub_nodes",
        "mam_messages",
        "clustering_muc_rooms",
        "clustering_muc_room_effects",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='guarded' AND c.relname=$1 AND c.relkind='r')")
            .bind(name).fetch_one(admin).await?;
        ensure!(
            exists,
            "combined fixture did not prepare expected representative {name}"
        );
    }
    for name in [
        "spaces_metadata",
        "channel_space_links",
        "push_registrations",
        "push_service_vapid_keys",
        "notification_activity",
        "notification_outbox",
        "call_teardown_outbox",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='guarded' AND c.relname=$1 AND c.relkind='r')")
            .bind(name).fetch_one(admin).await?;
        ensure!(
            !exists,
            "combined fixture unexpectedly prepared omitted family object {name}"
        );
    }

    // A separate schema really owned by this database's owner exercises the
    // equality refusal with PostgreSQL-sourced OIDs; no catalog proof escapes.
    sqlx::query(&format!(
        "CREATE SCHEMA coverage_equal_owner AUTHORIZATION {database_owner}"
    ))
    .execute(admin)
    .await?;
    let (actual_database_owner, actual_schema_owner): (Oid, Oid) = sqlx::query_as(
        "SELECT d.datdba, n.nspowner FROM pg_catalog.pg_database d \
         JOIN pg_catalog.pg_namespace n ON n.nspname='coverage_equal_owner' \
         WHERE d.datname=pg_catalog.current_database()",
    )
    .fetch_one(admin)
    .await?;
    ensure!(actual_database_owner == actual_schema_owner);
    candidate.database_owner_role_oid = actual_database_owner.0;
    candidate.schema_owner_role_oid = actual_schema_owner.0;
    ensure!(matches!(
        inspect_coverage_gap_gate_v1(&candidate),
        CoverageRefusal::Unsupported(CoverageTargetError::OwnerPinsMustBeNonzeroAndDistinct)
    ));
    sqlx::query("DROP SCHEMA coverage_equal_owner")
        .execute(admin)
        .await?;
    Ok(())
}
