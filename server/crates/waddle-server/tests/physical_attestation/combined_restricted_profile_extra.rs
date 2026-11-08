//! Additional sequential fixture preparation, not a complete or selectable role profile.

use anyhow::{ensure, Context};
use sqlx::PgPool;
use waddle_server::call_teardown_outbox::CallTeardownOutboxStore;
use waddle_server::channel_space_links::DatabaseChannelSpaceLinkStore;
use waddle_server::db::lineage::PgIdentity;
use waddle_server::db::Database;
use waddle_server::notification_activity::NotificationActivityStore;
use waddle_server::notification_outbox::NotificationOutboxStore;
use waddle_server::pubsub::{DatabasePubSubStorage, PubSubRuntimeAttachError};
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::vapid_storage::VapidStorage;
use waddle_server::push_service::DatabasePushServiceStore;
use waddle_server::server::push_notification_custody::{
    verify_attested_push_notification_custody, PushNotificationCustodyError,
};
use waddle_server::spaces_metadata::{DatabaseSpacesMetadataStore, SpaceStoreAttachError};

#[derive(Clone, Copy)]
enum ExpectedAttachRefusal {
    InvalidMarker,
    MissingMarker,
    UnsafeRuntimeRole,
}

struct TableGrant {
    table: &'static str,
    privileges: &'static [&'static str],
}

pub(super) struct FixtureRoles<'a> {
    pub(super) owner: &'a str,
    pub(super) api: &'a str,
    pub(super) helper: &'a str,
    pub(super) runtime: &'a str,
}

const READ: &[&str] = &["SELECT"];
const READ_INSERT_UPDATE: &[&str] = &["SELECT", "INSERT", "UPDATE"];
const READ_INSERT_DELETE: &[&str] = &["SELECT", "INSERT", "DELETE"];
const READ_WRITE: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

// This is a fixture table-ACL list sourced from the individual store tests; it
// deliberately says nothing about functions, dependencies, other objects or
// the completeness of the joint role graph.
const TABLES: &[TableGrant] = &[
    TableGrant {
        table: "spaces_metadata",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "channel_space_links",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "_waddle_space_store_owner",
        privileges: READ,
    },
    TableGrant {
        table: "push_registrations",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "notification_activity",
        privileges: READ_INSERT_UPDATE,
    },
    TableGrant {
        table: "notification_activity_owner_state",
        privileges: READ,
    },
    TableGrant {
        table: "notification_candidates",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "notification_outbox",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "notification_outbox_lineage",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "notification_outbox_owner_state",
        privileges: READ,
    },
    TableGrant {
        table: "push_service_schema_marker",
        privileges: READ,
    },
    TableGrant {
        table: "push_owner_locks",
        privileges: READ_INSERT_UPDATE,
    },
    TableGrant {
        table: "push_nodes",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "push_node_locks",
        privileges: READ_INSERT_UPDATE,
    },
    TableGrant {
        table: "push_devices",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "push_delivery_attempts",
        privileges: READ_INSERT_DELETE,
    },
    TableGrant {
        table: "push_publish_jobs",
        privileges: READ_WRITE,
    },
    TableGrant {
        table: "push_publication_orders",
        privileges: READ_INSERT_UPDATE,
    },
    TableGrant {
        table: "push_service_vapid_keys",
        privileges: READ,
    },
    TableGrant {
        table: "push_service_vapid_schema_marker",
        privileges: READ,
    },
    TableGrant {
        table: "call_teardown_outbox",
        privileges: READ_WRITE,
    },
];

pub(super) async fn prepare_and_check(
    owner_db: &Database,
    main: &Database,
    identity: &PgIdentity,
    admin: &PgPool,
    roles: FixtureRoles<'_>,
) -> anyhow::Result<()> {
    let FixtureRoles {
        owner,
        api,
        helper,
        runtime,
    } = roles;
    // Never import an operator's VAPID bootstrap secret into a disposable
    // database. Owner preparation otherwise reads this process environment.
    ensure!(
        std::env::var_os("WADDLE_VAPID_PRIVATE_KEY").is_none(),
        "the disposable fixture requires WADDLE_VAPID_PRIVATE_KEY to be unset"
    );

    for table in [
        "spaces_metadata",
        "channel_space_links",
        "_waddle_space_store_owner",
        "push_registrations",
        "push_nodes",
        "push_service_vapid_keys",
        "notification_outbox",
        "notification_activity",
        "call_teardown_outbox",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'guarded' AND c.relname = $1)",
        )
        .bind(table)
        .fetch_one(admin)
        .await?;
        ensure!(!exists, "{table} existed before additional preparation");
    }

    // Keep both stores and their identity marker inside the owner transaction.
    // This test-support API prepares fixture state only; it does not authorize
    // server startup or serving.
    waddle_server::test_support::prepare_paired_space_stores_owner_for_test(
        owner_db.clone(),
        identity,
    )
    .await
    .context("paired space-store owner preparation")?;
    DatabasePushRegistrationStore::initialize_owner(owner_db.clone())
        .await
        .context("push registration owner initialization")?;
    NotificationActivityStore::initialize_owner(owner_db.clone())
        .await
        .context("notification activity owner preparation")?;
    CallTeardownOutboxStore::initialize_owner(owner_db, identity)
        .await
        .context("call teardown owner preparation")?;
    DatabasePushServiceStore::initialize_owner(owner_db, identity)
        .await
        .context("Push Service owner preparation")?;
    NotificationOutboxStore::prepare_owner(owner_db, identity)
        .await
        .context("notification outbox owner preparation")?;
    DatabasePushServiceStore::adopt_notification_ancestry_owner(owner_db, identity)
        .await
        .context("Push Service notification ancestry preparation")?;
    VapidStorage::prepare_owner(owner_db, identity, &[7_u8; 32])
        .await
        .context("VAPID owner preparation")?;

    assert_space_owner_boundary(admin, owner, runtime).await?;
    assert_paired_space_marker(admin, identity).await?;
    let push_phase: i64 = sqlx::query_scalar(
        "SELECT owner_phase FROM guarded.push_service_schema_marker WHERE marker_id = 1",
    )
    .fetch_one(admin)
    .await?;
    ensure!(push_phase == 1, "Push Service owner phase did not complete");
    let matching_key: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM guarded.push_service_vapid_schema_marker m \
         JOIN guarded.push_service_vapid_keys k ON k.kid = m.initial_kid \
         WHERE m.marker_id = 1 AND k.retired_at_ms IS NULL)",
    )
    .fetch_one(admin)
    .await?;
    ensure!(
        matching_key,
        "VAPID owner marker does not identify an active key"
    );

    for table in TABLES {
        sqlx::query(&format!(
            "GRANT {} ON guarded.{} TO {runtime}",
            table.privileges.join(", "),
            table.table
        ))
        .execute(admin)
        .await?;
        ensure!(
            table_matches(admin, owner, runtime, table).await?,
            "{} has wrong owner or fixture table ACL",
            table.table
        );
    }

    // The two runtime handles share the exact attested main pool, and both
    // attach paths verify the same paired owner marker read-only.
    let spaces = DatabaseSpacesMetadataStore::attach_existing(main.clone())
        .await
        .context("attach paired spaces metadata store")?;
    let links = DatabaseChannelSpaceLinkStore::attach_existing(main.clone())
        .await
        .context("attach paired channel-space links store")?;
    drop(links);
    drop(spaces);

    // Both attachment entry points refuse identity drift and a missing marker.
    sqlx::query(
        "UPDATE guarded._waddle_space_store_owner SET database_name = 'wrong' WHERE marker_id = 1",
    )
    .execute(admin)
    .await?;
    require_both_attach_refusals(main, ExpectedAttachRefusal::InvalidMarker, "wrong marker")
        .await?;
    sqlx::query(
        "UPDATE guarded._waddle_space_store_owner SET database_name = $1 WHERE marker_id = 1",
    )
    .bind(&identity.database.name.0)
    .execute(admin)
    .await?;

    sqlx::query("DELETE FROM guarded._waddle_space_store_owner WHERE marker_id = 1")
        .execute(admin)
        .await?;
    require_both_attach_refusals(main, ExpectedAttachRefusal::MissingMarker, "missing marker")
        .await?;
    // Restore through the paired owner path so subsequent controls start from
    // the exact owner-prepared state rather than hand-writing its certificate.
    waddle_server::test_support::prepare_paired_space_stores_owner_for_test(
        owner_db.clone(),
        identity,
    )
    .await
    .context("restore paired space stores after missing-marker refusal")?;
    assert_paired_space_marker(admin, identity).await?;

    // Exercise the runtime's real catalog verifier with one missing and one
    // excess right. The exact table ACL assertions above also cover the
    // SELECT-only owner marker grant.
    sqlx::query(&format!(
        "REVOKE INSERT ON guarded.spaces_metadata FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    require_both_attach_refusals(
        main,
        ExpectedAttachRefusal::UnsafeRuntimeRole,
        "missing spaces INSERT",
    )
    .await?;
    sqlx::query(&format!(
        "GRANT INSERT ON guarded.spaces_metadata TO {runtime}"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!(
        "GRANT TRUNCATE ON guarded.spaces_metadata TO {runtime}"
    ))
    .execute(admin)
    .await?;
    require_both_attach_refusals(
        main,
        ExpectedAttachRefusal::UnsafeRuntimeRole,
        "excess spaces TRUNCATE",
    )
    .await?;
    sqlx::query(&format!(
        "REVOKE TRUNCATE ON guarded.spaces_metadata FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    let spaces = DatabaseSpacesMetadataStore::attach_existing(main.clone())
        .await
        .context("re-attach paired spaces metadata after ACL restoration")?;
    let links = DatabaseChannelSpaceLinkStore::attach_existing(main.clone())
        .await
        .context("re-attach paired links after ACL restoration")?;
    drop(links);
    drop(spaces);

    // Demonstrate that this limited ACL witness rejects both missing and
    // excess rights; no reusable profile proof escapes the fixture.
    let registrations = TABLES
        .iter()
        .find(|t| t.table == "push_registrations")
        .context("registration fixture contract")?;
    sqlx::query(&format!(
        "REVOKE INSERT ON guarded.push_registrations FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        !table_matches(admin, owner, runtime, registrations).await?,
        "missing INSERT accepted"
    );
    sqlx::query(&format!(
        "GRANT INSERT ON guarded.push_registrations TO {runtime}"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!(
        "GRANT TRUNCATE ON guarded.push_registrations TO {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        !table_matches(admin, owner, runtime, registrations).await?,
        "excess TRUNCATE accepted"
    );
    sqlx::query(&format!(
        "REVOKE TRUNCATE ON guarded.push_registrations FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(table_matches(admin, owner, runtime, registrations).await?);

    assert_push_custody_refusals(admin, main, identity, owner, api, helper, runtime).await?;
    Ok(())
}

/// Preserve the existing A/H/R split while proving why the current joint
/// Push reader cannot complete on this fixture. The migrated Foundation heap
/// remains schema-owner-owned; A has its candidate DML grant and R has no
/// direct SELECT grant. The joint reader first reaches the legacy PubSub
/// verifier, whose helper check requires schema ownership, before attempting
/// its later ACCESS SHARE lock on Foundation.
async fn assert_push_custody_refusals(
    admin: &PgPool,
    main: &Database,
    identity: &PgIdentity,
    schema_owner: &str,
    api: &str,
    helper: &str,
    runtime: &str,
) -> anyhow::Result<()> {
    let role_matrix: bool = sqlx::query_scalar(
        "SELECT foundation.relowner = namespace.nspowner \
         AND schema_role.oid = namespace.nspowner \
         AND api_role.rolname = $2 AND helper_role.rolname = $3 \
         AND runtime_role.rolname = $4 \
         AND api_role.oid <> namespace.nspowner \
         AND pg_catalog.has_table_privilege(api_role.oid, foundation.oid, 'SELECT') \
         AND pg_catalog.has_table_privilege(api_role.oid, foundation.oid, 'INSERT') \
         AND pg_catalog.has_table_privilege(api_role.oid, foundation.oid, 'UPDATE') \
         AND pg_catalog.has_table_privilege(api_role.oid, foundation.oid, 'DELETE') \
         AND NOT pg_catalog.has_table_privilege(runtime_role.oid, foundation.oid, 'SELECT') \
         AND NOT pg_catalog.has_any_column_privilege(runtime_role.oid, foundation.oid, 'SELECT') \
         AND pubsub_lock.proowner = helper_role.oid \
         AND pubsub_lock.proowner <> namespace.nspowner \
         FROM pg_catalog.pg_class foundation \
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid = foundation.relnamespace \
         JOIN pg_catalog.pg_roles schema_role ON schema_role.rolname = $1 \
         JOIN pg_catalog.pg_roles api_role ON api_role.rolname = $2 \
         JOIN pg_catalog.pg_roles helper_role ON helper_role.rolname = $3 \
         JOIN pg_catalog.pg_roles runtime_role ON runtime_role.rolname = $4 \
         JOIN pg_catalog.pg_proc pubsub_lock ON pubsub_lock.pronamespace = namespace.oid \
           AND pubsub_lock.proname = 'pubsub_runtime_attach_lock' AND pubsub_lock.pronargs = 0 \
         WHERE namespace.nspname = $5 AND foundation.relname = 'ingress_effect_descendants' \
           AND foundation.relkind = 'r'",
    )
    .bind(schema_owner)
    .bind(api)
    .bind(helper)
    .bind(runtime)
    .bind(&identity.schema.name.0)
    .fetch_one(admin)
    .await?;
    ensure!(
        role_matrix,
        "combined Push custody role matrix drifted: expected schema-owned Foundation heap, A DML, H helper, and no R SELECT"
    );

    // This public runtime attachment uses the legacy verifier directly. It
    // must refuse the H-owned helper instead of returning a PubSub handle.
    match DatabasePubSubStorage::attach_existing(main.clone()).await {
        Err(PubSubRuntimeAttachError::InvalidAttachLock) => {}
        Err(error) => anyhow::bail!("legacy PubSub verifier refused unexpectedly: {error}"),
        Ok(store) => {
            drop(store);
            anyhow::bail!("legacy PubSub verifier accepted the H-owned helper")
        }
    }

    // PostgreSQL requires SELECT for this exact ACCESS SHARE lock used by the
    // joint diagnostic. Exercise it as R without widening any fixture grant.
    let mut runtime_tx = admin.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {runtime}"))
        .execute(&mut *runtime_tx)
        .await?;
    let current_role: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *runtime_tx)
        .await?;
    ensure!(
        current_role == runtime,
        "Foundation refusal did not run as R"
    );
    let lock = sqlx::query("LOCK TABLE guarded.ingress_effect_descendants IN ACCESS SHARE MODE")
        .execute(&mut *runtime_tx)
        .await;
    let lock_error = lock.expect_err("R unexpectedly acquired the Foundation read lock");
    let sqlstate = lock_error
        .as_database_error()
        .and_then(|error| error.code())
        .map(|code| code.into_owned());
    ensure!(
        sqlstate.as_deref() == Some("42501"),
        "Foundation ACCESS SHARE refusal returned unexpected SQLSTATE: {sqlstate:?}"
    );
    runtime_tx.rollback().await?;

    // The public joint diagnostic returns evidence only; it does not expose
    // functional store handles. Its stage ordering checks Push/outbox first,
    // then the legacy PubSub helper, and attempts Foundation only after that
    // helper succeeds. Preserve the exact H-stage refusal in this fixture.
    match verify_attested_push_notification_custody(main.clone(), &[7_u8; 32]).await {
        Err(PushNotificationCustodyError::PubSub(PubSubRuntimeAttachError::InvalidAttachLock)) => {}
        Err(error) => {
            anyhow::bail!("joint Push custody diagnostic refused at an unexpected stage: {error}")
        }
        Ok(evidence) => {
            anyhow::bail!("joint Push custody diagnostic unexpectedly reached evidence: {evidence}")
        }
    }
    Ok(())
}

async fn assert_space_owner_boundary(
    admin: &PgPool,
    owner: &str,
    runtime: &str,
) -> anyhow::Result<()> {
    let safe: bool = sqlx::query_scalar(
        "SELECT namespace.nspowner = owner.oid \
         AND owner.rolname = $1 AND NOT owner.rolcanlogin AND NOT owner.rolinherit \
         AND NOT owner.rolsuper AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole \
         AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
           WHERE membership.member = owner.oid) \
         AND runtime.rolname = $2 AND runtime.rolcanlogin AND NOT runtime.rolinherit \
         AND NOT runtime.rolsuper AND NOT runtime.rolcreatedb AND NOT runtime.rolcreaterole \
         AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls \
         AND runtime.oid <> owner.oid AND runtime.oid <> database.datdba \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
           WHERE membership.member = runtime.oid OR membership.roleid = runtime.oid) \
         AND pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'USAGE') \
         AND NOT pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'CREATE') \
         AND (SELECT count(*) = 3 AND COALESCE(bool_and(relation.relowner = owner.oid), false) \
              FROM pg_catalog.pg_class relation \
              WHERE relation.relnamespace = namespace.oid \
                AND relation.relkind = 'r' \
                AND relation.relname = ANY(ARRAY[ \
                  'spaces_metadata', 'channel_space_links', '_waddle_space_store_owner'])) \
         FROM pg_catalog.pg_namespace namespace \
         JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
         JOIN pg_catalog.pg_roles runtime ON runtime.rolname = $2 \
         JOIN pg_catalog.pg_database database \
           ON database.datname = pg_catalog.current_database() \
         WHERE namespace.nspname = 'guarded'",
    )
    .bind(owner)
    .bind(runtime)
    .fetch_one(admin)
    .await?;
    ensure!(
        safe,
        "paired space tables do not match owner/runtime role boundary"
    );
    Ok(())
}

async fn assert_paired_space_marker(admin: &PgPool, identity: &PgIdentity) -> anyhow::Result<()> {
    let exact: bool = sqlx::query_scalar(
        "SELECT count(*) = 1 AND COALESCE(bool_and( \
           marker_id = 1 AND marker_version = 1 \
           AND spaces_contract_version = 1 AND channel_links_contract_version = 1 \
           AND system_identifier = $1 AND database_oid = $2 AND database_name = $3 \
           AND schema_oid = $4 AND schema_name = $5), false) \
         FROM guarded._waddle_space_store_owner",
    )
    .bind(identity.system_identifier.to_string())
    .bind(i64::from(identity.database.oid))
    .bind(&identity.database.name.0)
    .bind(i64::from(identity.schema.oid))
    .bind(&identity.schema.name.0)
    .fetch_one(admin)
    .await?;
    ensure!(
        exact,
        "paired space marker differs from the supplied identity"
    );
    Ok(())
}

async fn require_both_attach_refusals(
    main: &Database,
    expected: ExpectedAttachRefusal,
    label: &str,
) -> anyhow::Result<()> {
    let spaces = DatabaseSpacesMetadataStore::attach_existing(main.clone()).await;
    let links = DatabaseChannelSpaceLinkStore::attach_existing(main.clone()).await;
    ensure!(
        refusal_matches(&spaces, expected),
        "spaces metadata attachment did not reject {label}"
    );
    ensure!(
        refusal_matches(&links, expected),
        "channel-space link attachment did not reject {label}"
    );
    Ok(())
}

fn refusal_matches<T>(
    result: &Result<T, SpaceStoreAttachError>,
    expected: ExpectedAttachRefusal,
) -> bool {
    matches!(
        (expected, result),
        (
            ExpectedAttachRefusal::InvalidMarker,
            Err(SpaceStoreAttachError::OwnerMarkerInvalid)
        ) | (
            ExpectedAttachRefusal::MissingMarker,
            Err(SpaceStoreAttachError::OwnerMarkerMissing)
        ) | (
            ExpectedAttachRefusal::UnsafeRuntimeRole,
            Err(SpaceStoreAttachError::UnsafeRuntimeRole)
        )
    )
}

async fn table_matches(
    admin: &PgPool,
    owner: &str,
    runtime: &str,
    table: &TableGrant,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT c.relkind = 'r' AND owner.rolname = $2 AND NOT owner.rolcanlogin \
         AND runtime.rolcanlogin AND runtime.oid <> owner.oid \
         AND NOT runtime.rolsuper AND NOT runtime.rolcreatedb AND NOT runtime.rolcreaterole \
         AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls AND NOT runtime.rolinherit \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m \
           WHERE m.member = runtime.oid OR m.roleid = runtime.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
           WHERE a.attrelid = c.oid AND a.attacl IS NOT NULL) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
           COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl \
           WHERE acl.grantee <> c.relowner AND (acl.grantee <> runtime.oid \
             OR acl.grantor <> c.relowner OR acl.is_grantable \
             OR NOT (acl.privilege_type = ANY($4::text[])))) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest(ARRAY[ \
           'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER', 'MAINTAIN' \
         ]::text[]) privilege WHERE pg_catalog.has_table_privilege(runtime.oid, c.oid, privilege) \
           <> (privilege = ANY($4::text[]))) \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_roles owner ON owner.oid = c.relowner \
         JOIN pg_catalog.pg_roles runtime ON runtime.rolname = $3 \
         WHERE n.nspname = 'guarded' AND c.relname = $1",
    )
    .bind(table.table)
    .bind(owner)
    .bind(runtime)
    .bind(table.privileges)
    .fetch_optional(admin)
    .await?
    .unwrap_or(false))
}
