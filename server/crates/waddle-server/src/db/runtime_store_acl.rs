//! Role and effective ACL checks for restricted same-pool runtime stores.
//!
//! The caller holds ACCESS SHARE on the inspected tables and owns the same
//! bounded read-only transaction used to verify their schema and lineage.

use super::{DatabaseError, Transaction};

#[derive(Clone, Copy)]
struct Relation {
    name: &'static str,
    insert: bool,
    update: bool,
    delete: bool,
}

const INBOX_RELATIONS: &[Relation] = &[
    Relation {
        name: "inbox_entries",
        insert: true,
        update: true,
        delete: false,
    },
    Relation {
        name: "groupchat_notification_recovery",
        insert: true,
        update: true,
        delete: true,
    },
];
const GLOBAL_RELATIONS: &[Relation] = &[
    Relation {
        name: "_lineage",
        insert: false,
        update: false,
        delete: false,
    },
    Relation {
        name: "_migrations",
        insert: false,
        update: false,
        delete: false,
    },
];
const SM_RELATIONS: &[Relation] = &[
    Relation {
        name: "sm_sessions",
        insert: true,
        update: true,
        delete: true,
    },
    Relation {
        name: "sm_unacked",
        insert: true,
        update: false,
        delete: true,
    },
    Relation {
        name: "sm_acknowledged_windows",
        insert: true,
        update: false,
        delete: true,
    },
    Relation {
        name: "sm_ingress_appends",
        insert: true,
        update: true,
        delete: true,
    },
];
/// The durable session authority locks and updates its session rows, deletes
/// purged tombstones, and inserts and prunes operation outcomes. It never
/// updates an operation row: an outcome is written once.
const DURABLE_SM_RELATIONS: &[Relation] = &[
    Relation {
        name: "sm_durable_sessions",
        insert: true,
        update: true,
        delete: true,
    },
    Relation {
        name: "sm_durable_operations",
        insert: true,
        update: false,
        delete: true,
    },
];
/// The account binding store inserts, row-locks and updates resource rows
/// and deletes the row of a resource that holds no binding. It inserts and
/// prunes operation outcomes and never updates one: an outcome is written
/// once.
const ACCOUNT_BINDING_RELATIONS: &[Relation] = &[
    Relation {
        name: "account_resource_bindings",
        insert: true,
        update: true,
        delete: true,
    },
    Relation {
        name: "account_resource_operations",
        insert: true,
        update: false,
        delete: true,
    },
];
const PENDING_RELATIONS: &[Relation] = &[
    Relation {
        name: "pending_delivery",
        insert: true,
        update: true,
        delete: true,
    },
    Relation {
        name: "pending_delivery_sm_promotions",
        insert: true,
        update: false,
        delete: true,
    },
    Relation {
        name: "pending_delivery_startup_migrations",
        insert: false,
        update: false,
        delete: false,
    },
];

const WEBHOOK_RELATIONS: &[Relation] = &[Relation {
    name: "webhook_deliveries",
    insert: true,
    update: true,
    delete: true,
}];

/// Returns the first unsafe component. Privileged role, owner and ACL changes
/// must remain quiescent after this point-in-time check.
pub(crate) async fn verify_inbox(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, INBOX_RELATIONS, false).await
}

pub(crate) async fn verify_global(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, GLOBAL_RELATIONS, false).await
}

pub(crate) async fn verify_sm(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, SM_RELATIONS, false).await
}

pub(crate) async fn verify_durable_sm(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, DURABLE_SM_RELATIONS, false).await
}

pub(crate) async fn verify_account_bindings(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, ACCOUNT_BINDING_RELATIONS, false).await
}

pub(crate) async fn verify_pending(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, PENDING_RELATIONS, true).await
}

pub(crate) async fn verify_webhook_deliveries(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    verify(tx, schema, WEBHOOK_RELATIONS, false).await
}

pub(crate) async fn unexpected_webhook_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, WEBHOOK_RELATIONS, false).await
}

/// Check executable relation objects while the caller holds each table lock.
/// The pending-delivery verifier separately checks the one permitted trigger's
/// exact function and trigger definition. The durable session and account
/// binding relations are enrolled under the ingress epoch guard, so each of
/// them must carry exactly its two guard triggers, bound to the guard
/// functions this source tree installs; every other relation carries none.
pub(crate) async fn unexpected_inbox_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, INBOX_RELATIONS, false).await
}

pub(crate) async fn unexpected_sm_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, SM_RELATIONS, false).await
}

pub(crate) async fn unexpected_durable_sm_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, DURABLE_SM_RELATIONS, true).await
}

pub(crate) async fn unexpected_account_binding_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, ACCOUNT_BINDING_RELATIONS, true).await
}

pub(crate) async fn unexpected_pending_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<&'static str>, DatabaseError> {
    unexpected_executable_schema(tx, schema, PENDING_RELATIONS, false).await
}

async fn unexpected_executable_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    relations: &[Relation],
    epoch_guarded: bool,
) -> Result<Option<&'static str>, DatabaseError> {
    for relation in relations {
        let mut rows = tx.query(
            "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
             AND (? <> 0 OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
               WHERE tr.tgrelid = c.oid AND NOT tr.tgisinternal \
                 AND NOT (c.relname = 'pending_delivery' \
                   AND tr.tgname = 'pending_notification_recovery_ordinal_immutable'))) \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r'",
            crate::db_params![epoch_guarded, schema, relation.name],
        ).await?;
        let safe = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        drop(rows);
        if !safe
            || (epoch_guarded && !epoch_guard_triggers_match(tx, schema, relation.name).await?)
        {
            return Ok(Some(relation.name));
        }
    }
    Ok(None)
}

/// Longest guard function body read back for comparison. Both installed
/// bodies are far shorter; a longer one is not one of them.
const EPOCH_GUARD_BODY_LIMIT: i64 = 16_384;

/// Whether `relation` carries exactly the V1009 guard pair: a statement-level
/// trigger before INSERT, UPDATE and DELETE bound to
/// `waddle_ingress_epoch_guard`, and one before TRUNCATE bound to
/// `waddle_ingress_truncate_guard`. Both fire for every session
/// (`ENABLE ALWAYS`), and both functions live in the relation's schema, are
/// owned by its owner, run with the caller's rights and have a body this
/// source tree installs. Foreign-key triggers are internal and not counted.
async fn epoch_guard_triggers_match(
    tx: &mut Transaction<'_>,
    schema: &str,
    relation: &str,
) -> Result<bool, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT tr.tgname::text, tr.tgtype::integer, p.proname::text, \
               pg_catalog.left(p.prosrc, ?::integer), \
               tr.tgenabled = 'A' AND tr.tgparentid = 0 AND tr.tgconstraint = 0 \
               AND NOT tr.tgdeferrable AND NOT tr.tginitdeferred AND tr.tgnargs = 0 \
               AND tr.tgattr::text = '' AND tr.tgqual IS NULL \
               AND tr.tgoldtable IS NULL AND tr.tgnewtable IS NULL \
               AND p.pronamespace = n.oid AND p.proowner = c.relowner AND NOT p.prosecdef \
             FROM pg_catalog.pg_trigger tr \
             JOIN pg_catalog.pg_class c ON c.oid = tr.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_proc p ON p.oid = tr.tgfoid \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r' \
               AND NOT tr.tgisinternal",
            crate::db_params![EPOCH_GUARD_BODY_LIMIT, schema, relation],
        )
        .await?;
    let expected = [
        (
            format!("{relation}_epoch_guard_dml"),
            30_i64,
            "waddle_ingress_epoch_guard",
        ),
        (
            format!("{relation}_epoch_guard_truncate"),
            34_i64,
            "waddle_ingress_truncate_guard",
        ),
    ];
    let mut found = [false; 2];
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let kind: i64 = row.get(1)?;
        let function: String = row.get(2)?;
        let body: String = row.get(3)?;
        let shape: bool = row.get(4)?;
        let Some(index) = expected.iter().position(|(trigger, trigger_kind, guard)| {
            *trigger == name && *trigger_kind == kind && *guard == function
        }) else {
            return Ok(false);
        };
        if found[index]
            || !shape
            || !super::migrations::is_installed_epoch_guard_body(&function, &body)
        {
            return Ok(false);
        }
        found[index] = true;
    }
    Ok(found == [true; 2])
}

async fn verify(
    tx: &mut Transaction<'_>,
    schema: &str,
    relations: &[Relation],
    pending_sequence: bool,
) -> Result<Option<&'static str>, DatabaseError> {
    if !runtime_role_is_restricted(tx, schema).await? {
        return Ok(Some("runtime role"));
    }
    for relation in relations {
        if !relation_privileges_match(tx, schema, *relation).await? {
            return Ok(Some(relation.name));
        }
    }
    if pending_sequence && !pending_sequence_is_safe(tx, schema).await? {
        return Ok(Some("pending delivery sequence"));
    }
    Ok(None)
}

async fn runtime_role_is_restricted(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<bool, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole \
         AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls \
         AND session_user::text = r.rolname AND current_user::text = r.rolname \
         AND r.oid <> d.datdba AND r.oid <> n.nspowner \
         AND NOT pg_catalog.pg_has_role(r.oid, n.nspowner, 'SET') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid OR m.roleid = r.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p WHERE p.pronamespace = n.oid AND p.proowner = r.oid) \
         AND NOT pg_catalog.has_database_privilege(r.oid, d.oid, 'CREATE,TEMP') \
         AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE') \
         AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET') \
         AND pg_catalog.current_setting('session_replication_role') = 'origin' \
         FROM pg_catalog.pg_roles r JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_database d ON d.datname = pg_catalog.current_database() \
         WHERE r.rolname = current_user",
            crate::db_params![schema],
        )
        .await?;
    match rows.next().await? {
        Some(row) => row.get(0),
        None => Ok(false),
    }
}

async fn relation_privileges_match(
    tx: &mut Transaction<'_>,
    schema: &str,
    relation: Relation,
) -> Result<bool, DatabaseError> {
    let mut rows = tx.query(
        "SELECT c.relkind = 'r' AND NOT c.relrowsecurity \
         AND r.oid <> c.relowner AND NOT pg_catalog.pg_has_role(r.oid, c.relowner, 'SET') \
         AND pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT') \
         AND pg_catalog.has_table_privilege(r.oid, c.oid, 'INSERT') = (? <> 0) \
         AND (? <> 0 OR NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'INSERT')) \
         AND pg_catalog.has_table_privilege(r.oid, c.oid, 'UPDATE') = (? <> 0) \
         AND (? <> 0 OR NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'UPDATE')) \
         AND (pg_catalog.has_table_privilege(r.oid, c.oid, 'DELETE') = (? <> 0)) \
         AND NOT pg_catalog.has_table_privilege(r.oid, c.oid, 'TRUNCATE,REFERENCES,TRIGGER') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, 'REFERENCES') \
         AND NOT pg_catalog.has_table_privilege(r.oid, c.oid, \
           'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid, c.oid, \
           'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION') \
         AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
           THEN NOT pg_catalog.has_table_privilege(r.oid, c.oid, 'MAINTAIN') ELSE true END \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) a WHERE a.grantee = 0) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl \
           WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped AND acl.grantee = 0) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_roles r ON r.rolname = current_user WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![
            relation.insert,
            relation.insert,
            relation.update,
            relation.update,
            relation.delete,
            schema,
            relation.name
        ],
    ).await?;
    match rows.next().await? {
        Some(row) => row.get(0),
        None => Ok(false),
    }
}

async fn pending_sequence_is_safe(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<bool, DatabaseError> {
    let mut rows = tx.query(
        "SELECT c.relkind = 'S' AND c.relowner <> r.oid \
         AND NOT pg_catalog.pg_has_role(r.oid, c.relowner, 'SET') \
         AND pg_catalog.has_sequence_privilege(r.oid, c.oid, 'USAGE') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid, c.oid, 'UPDATE') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid, c.oid, 'USAGE WITH GRANT OPTION,SELECT WITH GRANT OPTION,UPDATE WITH GRANT OPTION') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl, pg_catalog.acldefault('S', c.relowner))) a WHERE a.grantee = 0) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_roles r ON r.rolname = current_user \
         WHERE c.oid = pg_catalog.pg_get_serial_sequence(pg_catalog.format('%I.%I', ?, 'pending_delivery'), 'notification_recovery_ordinal')::pg_catalog.regclass",
        crate::db_params![schema],
    ).await?;
    match rows.next().await? {
        Some(row) => row.get(0),
        None => Ok(false),
    }
}
