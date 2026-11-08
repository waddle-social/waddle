//! Additional fixture preparation, not a complete or selectable role profile.

use anyhow::{ensure, Context};
use sqlx::PgPool;
use waddle_server::call_teardown_outbox::CallTeardownOutboxStore;
use waddle_server::channel_space_links::DatabaseChannelSpaceLinkStore;
use waddle_server::db::lineage::PgIdentity;
use waddle_server::db::Database;
use waddle_server::notification_activity::NotificationActivityStore;
use waddle_server::notification_outbox::NotificationOutboxStore;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::vapid_storage::VapidStorage;
use waddle_server::push_service::DatabasePushServiceStore;
use waddle_server::spaces_metadata::DatabaseSpacesMetadataStore;

struct TableGrant {
    table: &'static str,
    privileges: &'static [&'static str],
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
    identity: &PgIdentity,
    admin: &PgPool,
    owner: &str,
    runtime: &str,
) -> anyhow::Result<()> {
    // Never import an operator's VAPID bootstrap secret into a disposable
    // database. Owner preparation otherwise reads this process environment.
    ensure!(
        std::env::var_os("WADDLE_VAPID_PRIVATE_KEY").is_none(),
        "the disposable fixture requires WADDLE_VAPID_PRIVATE_KEY to be unset"
    );

    for table in [
        "spaces_metadata",
        "channel_space_links",
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

    // The public PostgreSQL initializers create the two tables but invalidate
    // the paired owner marker. The strict paired preparation API is not public
    // to integration tests; this fixture therefore cannot attach these stores.
    DatabaseSpacesMetadataStore::initialize_owner(owner_db.clone())
        .await
        .context("legacy spaces owner initialization")?;
    DatabaseChannelSpaceLinkStore::initialize_owner(owner_db.clone())
        .await
        .context("legacy channel links owner initialization")?;
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

    let space_marker_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM guarded._waddle_space_store_owner")
            .fetch_one(admin)
            .await?;
    ensure!(
        space_marker_rows == 0,
        "legacy spaces preparation certified a paired marker"
    );
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
    Ok(())
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
