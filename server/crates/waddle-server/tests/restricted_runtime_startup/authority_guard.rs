//! The durable session authority's and the account binding store's
//! relations under the ingress protocol epoch guard.
//!
//! The owner phase creates the four relations with their guard triggers and
//! enrolls them in the guard manifest. The runtime role then holds exactly
//! the privileges of the grant matrix, a write needs the live epoch's
//! transaction-local proof, and each store's attachment refuses a relation
//! whose guard pair is not the installed one.

use std::time::Duration;

use chrono::Utc;
use jid::FullJid;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::account_bindings::{
    AccountBindingAttachError, AccountBindingStore, AccountBindingStoreError,
    DatabaseAccountBindingStore,
};
use waddle_server::db::Database;
use waddle_server::ingress_substrate::{
    IngressSubstrateError, AUTHORITY_GUARDED_TABLES, EPOCH_GUARDED_TABLES,
};
use waddle_server::sm_authority::{
    DurableSmError, DurableSmSelection, EnableError, EnableOperation, EnableRejection,
    EnableRequest,
};
use waddle_server::sm_persistence::SmRuntimeAttachError;
use waddle_ws_test_support::require_test_postgres_url;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
    AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceRevision,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity,
};

use super::{attach_serving_pool, execute, runtime_connection, sqlstate, Prepared, SCHEMA};

/// Each guarded authority relation, a key column, and whether the grant
/// matrix lets the runtime role update it. Operation outcomes are written
/// once and pruned, so the runtime holds no UPDATE on them.
const RUNTIME_MATRIX: [(&str, &str, bool); 4] = [
    ("sm_durable_sessions", "session_id", true),
    ("sm_durable_operations", "operation_id", false),
    ("account_resource_bindings", "full_jid", true),
    ("account_resource_operations", "operation_id", false),
];

const PRIVILEGE_DENIED: &str = "42501";
const GUARD_REFUSED: &str = "P0001";

/// Run `statement` in its own transaction, first installing the epoch proof
/// a writer at `claimed_epoch` installs, and roll back. Returns the SQLSTATE
/// of a refusal.
async fn attempt(
    connection: &mut PgConnection,
    statement: &str,
    claimed_epoch: Option<i64>,
) -> Result<(), Option<String>> {
    let mut tx = connection.begin().await.expect("begin a probe transaction");
    if let Some(epoch) = claimed_epoch {
        sqlx::query(
            "SELECT pg_catalog.set_config('waddle.protocol_epoch', $1, true), \
             pg_catalog.set_config('waddle.protocol_epoch_xid', \
               pg_catalog.pg_current_xact_id()::text, true)",
        )
        .bind(epoch.to_string())
        .execute(&mut *tx)
        .await
        .expect("install an epoch proof");
    }
    let outcome = sqlx::query(statement)
        .execute(&mut *tx)
        .await
        .map(|_| ())
        .map_err(|error| sqlstate(&error));
    tx.rollback().await.expect("roll back the probe");
    outcome
}

/// The non-internal triggers of one relation: name, type, firing mode and
/// function.
async fn guard_triggers(prepared: &Prepared, table: &str) -> Vec<(String, i32, String, String)> {
    sqlx::query_as(
        "SELECT t.tgname::text, t.tgtype::integer, t.tgenabled::text, p.proname::text \
         FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid \
         WHERE n.nspname = $1 AND c.relname = $2 AND NOT t.tgisinternal \
         ORDER BY t.tgname",
    )
    .bind(SCHEMA)
    .bind(table)
    .fetch_all(&prepared.admin)
    .await
    .expect("read the relation's triggers")
}

async fn assert_guard_pair(prepared: &Prepared, table: &str) {
    assert_eq!(
        guard_triggers(prepared, table).await,
        vec![
            (
                format!("{table}_epoch_guard_dml"),
                30,
                "A".to_owned(),
                "waddle_ingress_epoch_guard".to_owned(),
            ),
            (
                format!("{table}_epoch_guard_truncate"),
                34,
                "A".to_owned(),
                "waddle_ingress_truncate_guard".to_owned(),
            ),
        ],
        "{table} carries exactly its guard pair"
    );
}

async fn enrolled_tables(prepared: &Prepared) -> Vec<String> {
    sqlx::query_scalar(&format!(
        "SELECT table_name FROM {SCHEMA}.ingress_epoch_guard_manifest"
    ))
    .fetch_all(&prepared.admin)
    .await
    .expect("read the guard manifest")
}

fn alice() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@localhost".parse().expect("bare JID"),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::new(1),
    )
}

fn resource(name: &str) -> FullJid {
    format!("alice@localhost/{name}").parse().expect("full JID")
}

fn fresh_binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn account_claim() -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@localhost".parse().expect("bare JID"),
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
        ClaimEpoch(1),
    )
}

fn bind(claim: &UserClaimIdentity) -> AccountResourceOperation {
    AccountResourceOperation::new(
        AccountResourceOperationId::fresh(),
        AccountResourceMutation::Bind {
            claim_identity: claim.clone(),
            expected_revision: AccountResourceRevision::INITIAL,
            binding_ref: fresh_binding(),
            lifetime: ResourceLifetime::fresh(),
        },
    )
}

async fn open_durable_sm(main: &Database) -> Result<DurableSmSelection, DurableSmError> {
    DurableSmSelection::open(main, false).await
}

/// The owner phase leaves the four relations guarded and enrolled. Under
/// the runtime login a statement the matrix does not grant is refused for
/// lack of privilege, a granted one is refused by the guard without the
/// live epoch's proof, and both stores commit through their own write entry.
/// Once the epoch moves beyond what the binary implements, the stores refuse
/// before writing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authority_relations_accept_only_matrix_writes_that_prove_the_live_epoch() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;

    let mut enrolled = enrolled_tables(&prepared).await;
    enrolled.sort();
    let mut expected: Vec<String> = EPOCH_GUARDED_TABLES.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(
        enrolled, expected,
        "the manifest enrolls every guarded table"
    );
    for table in AUTHORITY_GUARDED_TABLES {
        assert_guard_pair(&prepared, table).await;
        let monitor_reads: bool =
            sqlx::query_scalar("SELECT pg_catalog.has_table_privilege('pg_monitor', $1, 'SELECT')")
                .bind(format!("{SCHEMA}.{table}"))
                .fetch_one(&prepared.admin)
                .await
                .expect("inspect the monitoring grant");
        assert!(!monitor_reads, "pg_monitor must not read {table}");
    }

    // The owner phase activated epoch 2. Epoch 1 is the proof of a writer
    // that read the epoch before the activation.
    let mut runtime = runtime_connection(&prepared).await;
    assert_eq!(
        RUNTIME_MATRIX.map(|(table, _, _)| table),
        AUTHORITY_GUARDED_TABLES
    );
    for (table, key, may_update) in RUNTIME_MATRIX {
        let relation = format!("{SCHEMA}.{table}");
        for (statement, granted) in [
            (
                format!("INSERT INTO {relation} SELECT * FROM {relation} WHERE false"),
                true,
            ),
            (
                format!("UPDATE {relation} SET {key} = {key} WHERE false"),
                may_update,
            ),
            (format!("DELETE FROM {relation} WHERE false"), true),
        ] {
            let refusal = if granted {
                GUARD_REFUSED
            } else {
                PRIVILEGE_DENIED
            };
            for unproven in [None, Some(1)] {
                assert_eq!(
                    attempt(&mut runtime, &statement, unproven).await,
                    Err(Some(refusal.to_owned())),
                    "{statement} with proof {unproven:?}"
                );
            }
            let proven = attempt(&mut runtime, &statement, Some(2)).await;
            if granted {
                assert_eq!(proven, Ok(()), "{statement} with the live proof");
            } else {
                assert_eq!(
                    proven,
                    Err(Some(PRIVILEGE_DENIED.to_owned())),
                    "{statement} with the live proof"
                );
            }
        }
        // The runtime holds no TRUNCATE, and the truncate guard refuses the
        // statement for a role that does.
        let truncate = format!("TRUNCATE {relation}");
        assert_eq!(
            attempt(&mut runtime, &truncate, Some(2)).await,
            Err(Some(PRIVILEGE_DENIED.to_owned())),
            "{truncate} under the runtime role"
        );
        // CASCADE takes the referencing relation along, so the statement
        // reaches the trigger instead of failing the foreign-key check.
        let refused = sqlx::query(&format!("{truncate} CASCADE"))
            .execute(&prepared.admin)
            .await
            .expect_err("the truncate guard refuses an administrator");
        assert_eq!(
            sqlstate(&refused).as_deref(),
            Some(GUARD_REFUSED),
            "{refused}"
        );
        let owned = format!("ALTER TABLE {relation} DISABLE TRIGGER {table}_epoch_guard_dml");
        assert_eq!(
            attempt(&mut runtime, &owned, None).await,
            Err(Some(PRIVILEGE_DENIED.to_owned())),
            "the runtime must not disable a guard"
        );
    }
    runtime.close().await.expect("close the runtime probe");

    // Both stores attach on the attested serving pool and commit: their
    // write entry attests the connection and installs the live proof.
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let selection = open_durable_sm(main)
        .await
        .expect("the durable session store attaches under the runtime role");
    let sessions = selection.store().expect("the durable store is selected");
    let principal = alice();
    let phone = resource("phone");
    let owner = fresh_binding();
    let enable = EnableRequest {
        binding: &owner,
        principal: &principal,
        full_jid: &phone,
        max_resume: Duration::from_secs(300),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        claim: None,
        now: Utc::now(),
    };
    let operation = EnableOperation::mint();
    let enabled = sessions
        .enable(&operation, enable)
        .await
        .expect("an enable commits at the live epoch");
    assert!(enabled.enabled_reply().resume);
    assert!(sessions
        .session(operation.session())
        .await
        .expect("read the session")
        .is_some());

    let bindings = DatabaseAccountBindingStore::open(main.clone(), false)
        .await
        .expect("the account binding store attaches under the runtime role");
    let claim = account_claim();
    let applied = bindings
        .execute(&claim, &phone, &bind(&claim))
        .await
        .expect("a bind commits at the live epoch");
    assert!(matches!(
        applied.execution,
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { .. })
    ));

    // An activation this binary does not implement: every store write is
    // refused before its first statement, and nothing is stored.
    execute(
        &prepared.admin,
        &format!(
            "UPDATE {SCHEMA}.ingress_protocol_epoch SET epoch = 3, \
             activated_at = pg_catalog.now(), lineage_uuid = '{}' WHERE id = 1",
            prepared.lineage.lineage_uuid.0
        ),
    )
    .await;
    let later = EnableOperation::mint();
    assert!(matches!(
        sessions.enable(&later, enable).await,
        Err(EnableError::Rejected(EnableRejection::Store(
            DurableSmError::EpochUnproven(IngressSubstrateError::UnsupportedLiveEpoch)
        )))
    ));
    assert!(sessions
        .session(later.session())
        .await
        .expect("read the session")
        .is_none());
    let tablet = resource("tablet");
    assert!(matches!(
        bindings.execute(&claim, &tablet, &bind(&claim)).await,
        Err(AccountBindingStoreError::EpochUnproven(
            IngressSubstrateError::UnsupportedLiveEpoch
        ))
    ));
    assert!(bindings
        .load(&tablet)
        .await
        .expect("load the resource")
        .is_none());

    drop(pool);
    prepared.cleanup().await;
}

/// A database whose owner phase ran before the guard migration holds the
/// four relations unguarded, created by the store initializers, possibly
/// with rows. The next owner phase accepts it: the relations and their rows
/// stay, and the guard pair and manifest rows are added.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_guards_authority_relations_created_before_the_guard_migration() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;

    // Return the schema to what the earlier owner phase left: the ledger
    // ends at V1033, the relations carry no guard and are not enrolled.
    let manifest = format!("{SCHEMA}.ingress_epoch_guard_manifest");
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {manifest} DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row"
        ),
    )
    .await;
    for table in AUTHORITY_GUARDED_TABLES {
        for suffix in ["dml", "truncate"] {
            execute(
                &prepared.admin,
                &format!("DROP TRIGGER {table}_epoch_guard_{suffix} ON {SCHEMA}.{table}"),
            )
            .await;
        }
        execute(
            &prepared.admin,
            &format!("DELETE FROM {manifest} WHERE table_name = '{table}'"),
        )
        .await;
    }
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {manifest} ENABLE ALWAYS TRIGGER \
             ingress_epoch_guard_manifest_append_only_row"
        ),
    )
    .await;
    execute(
        &prepared.admin,
        // The ledger refuses an applied version above a missing one, so the
        // later migration is undone with it; its statement is repeatable.
        &format!("DELETE FROM {SCHEMA}._migrations WHERE version IN (1034, 1035, 1036)"),
    )
    .await;
    // A row the unguarded binary committed.
    execute(
        &prepared.admin,
        &format!(
            "INSERT INTO {SCHEMA}.sm_durable_operations \
             (operation_id, session_id, kind, outcome, generation, recorded_at_ms) \
             VALUES ('earlier-operation', 'earlier-session', 1, 1, 1, 0)"
        ),
    )
    .await;

    let lineage = prepared
        .rerun_owner_phase()
        .await
        .expect("the owner phase accepts the relations it created earlier");
    assert_eq!(lineage, prepared.lineage);

    for table in AUTHORITY_GUARDED_TABLES {
        assert_guard_pair(&prepared, table).await;
    }
    let enrolled = enrolled_tables(&prepared).await;
    for table in AUTHORITY_GUARDED_TABLES {
        assert!(
            enrolled.iter().any(|name| name == table),
            "{table} is enrolled again"
        );
    }
    let kept: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {SCHEMA}.sm_durable_operations \
         WHERE operation_id = 'earlier-operation'"
    ))
    .fetch_one(&prepared.admin)
    .await
    .expect("count the earlier row");
    assert_eq!(kept, 1);

    // The runtime attaches both stores to the upgraded relations, and a
    // writer without the proof is refused.
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    open_durable_sm(main)
        .await
        .expect("the durable session store attaches after the upgrade");
    DatabaseAccountBindingStore::open(main.clone(), false)
        .await
        .expect("the account binding store attaches after the upgrade");
    let mut runtime = runtime_connection(&prepared).await;
    assert_eq!(
        attempt(
            &mut runtime,
            &format!("DELETE FROM {SCHEMA}.sm_durable_operations"),
            None
        )
        .await,
        Err(Some(GUARD_REFUSED.to_owned()))
    );
    runtime.close().await.expect("close the runtime probe");

    drop(pool);
    prepared.cleanup().await;
}

/// A stream-management session left without a durable authority record is
/// drained by the owner phase: its retained message reaches offline storage
/// once, its retained IQ does not, its rows are deleted, and a second owner
/// phase changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_drains_sessions_without_a_durable_record() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let epoch: i64 = sqlx::query_scalar(&format!(
        "SELECT epoch FROM {SCHEMA}.ingress_protocol_epoch WHERE id = 1"
    ))
    .fetch_one(&prepared.admin)
    .await
    .expect("read the live epoch");
    let mut tx = prepared.admin.begin().await.expect("begin the seed");
    sqlx::query(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', $1, true), \
         pg_catalog.set_config('waddle.protocol_epoch_xid', \
           pg_catalog.pg_current_xact_id()::text, true)",
    )
    .bind(epoch.to_string())
    .execute(&mut *tx)
    .await
    .expect("install the epoch proof");
    sqlx::query(&format!(
        "INSERT INTO {SCHEMA}.sm_sessions (stream_id, user_id, full_jid, inbound_count, \
             outbound_count, last_acked, detached_at_ms, max_resume_duration_ms, \
             carbons_enabled, roster_interested, presence_available, presence_priority) \
         VALUES ('legacy-stream', 'alice@example.test', 'alice@example.test/phone', \
             0, 2, 0, 0, 300000, 0, 0, 1, 0)"
    ))
    .execute(&mut *tx)
    .await
    .expect("seed the legacy session");
    for (sequence, wire) in [
        (
            1_i64,
            "<message xmlns='jabber:client' type='chat' id='m1' from='bob@example.test/desk' \
             to='alice@example.test/phone'><body>retained</body></message>",
        ),
        (
            2_i64,
            "<iq xmlns='jabber:client' type='get' id='ping' from='example.test' \
             to='alice@example.test/phone'><ping xmlns='urn:xmpp:ping'/></iq>",
        ),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {SCHEMA}.sm_unacked \
                 (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES ('legacy-stream', $1, $2, 1700000000000)"
        ))
        .bind(sequence)
        .bind(wire)
        .execute(&mut *tx)
        .await
        .expect("seed a retained frame");
    }
    tx.commit().await.expect("commit the seed");

    let stored = || async {
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM {SCHEMA}.pending_delivery \
             WHERE recipient_jid = 'alice@example.test' \
               AND transient_xml LIKE '%>retained</body>%'"
        ))
        .fetch_one(&prepared.admin)
        .await
        .expect("count offline rows")
    };
    let legacy_rows = || async {
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT (SELECT count(*) FROM {SCHEMA}.sm_sessions) \
                  + (SELECT count(*) FROM {SCHEMA}.sm_unacked)"
        ))
        .fetch_one(&prepared.admin)
        .await
        .expect("count stream-management rows")
    };

    prepared
        .rerun_owner_phase()
        .await
        .expect("the owner phase drains the legacy session");
    assert_eq!(stored().await, 1);
    assert_eq!(legacy_rows().await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {SCHEMA}.pending_delivery"))
            .fetch_one(&prepared.admin)
            .await
            .expect("count every offline row"),
        1,
        "the retained IQ is not stored offline"
    );

    prepared
        .rerun_owner_phase()
        .await
        .expect("a second owner phase finds nothing to drain");
    assert_eq!(stored().await, 1);

    prepared.cleanup().await;
}

/// Each store's attachment accepts its relations only with the installed
/// guard pair: a disabled, origin-only, missing, rebound or additional
/// trigger is refused, and the attachment passes again once it is undone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_attachment_refuses_a_drifted_authority_guard() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();

    execute(
        &prepared.admin,
        &format!(
            "CREATE FUNCTION {SCHEMA}.authority_guard_probe() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
        ),
    )
    .await;
    execute(
        &prepared.admin,
        &format!(
            "ALTER FUNCTION {SCHEMA}.authority_guard_probe() OWNER TO {}",
            prepared.roles.schema_owner
        ),
    )
    .await;

    for table in AUTHORITY_GUARDED_TABLES {
        let relation = format!("{SCHEMA}.{table}");
        let dml = format!("{table}_epoch_guard_dml");
        let truncate = format!("{table}_epoch_guard_truncate");
        let always_dml = format!("ALTER TABLE {relation} ENABLE ALWAYS TRIGGER {dml}");
        let create_dml = |function: &str| {
            format!(
                "CREATE TRIGGER {dml} BEFORE INSERT OR UPDATE OR DELETE ON {relation} \
                 FOR EACH STATEMENT EXECUTE FUNCTION {SCHEMA}.{function}()"
            )
        };
        let deviations: [(Vec<String>, Vec<String>); 5] = [
            (
                vec![format!("ALTER TABLE {relation} DISABLE TRIGGER {dml}")],
                vec![always_dml.clone()],
            ),
            // A trigger that fires only for the `origin` replication role is
            // skipped by a session that sets another one.
            (
                vec![format!("ALTER TABLE {relation} ENABLE TRIGGER {dml}")],
                vec![always_dml.clone()],
            ),
            (
                vec![format!("DROP TRIGGER {truncate} ON {relation}")],
                vec![
                    format!(
                        "CREATE TRIGGER {truncate} BEFORE TRUNCATE ON {relation} \
                         FOR EACH STATEMENT EXECUTE FUNCTION \
                         {SCHEMA}.waddle_ingress_truncate_guard()"
                    ),
                    format!("ALTER TABLE {relation} ENABLE ALWAYS TRIGGER {truncate}"),
                ],
            ),
            (
                vec![
                    format!("DROP TRIGGER {dml} ON {relation}"),
                    create_dml("authority_guard_probe"),
                    always_dml.clone(),
                ],
                vec![
                    format!("DROP TRIGGER {dml} ON {relation}"),
                    create_dml("waddle_ingress_epoch_guard"),
                    always_dml.clone(),
                ],
            ),
            (
                vec![format!(
                    "CREATE TRIGGER authority_guard_extra BEFORE INSERT ON {relation} \
                     FOR EACH STATEMENT EXECUTE FUNCTION {SCHEMA}.authority_guard_probe()"
                )],
                vec![format!("DROP TRIGGER authority_guard_extra ON {relation}")],
            ),
        ];
        for (deviation, undo) in deviations {
            for statement in &deviation {
                execute(&prepared.admin, statement).await;
            }
            if table.starts_with("sm_durable_") {
                assert!(
                    matches!(
                        open_durable_sm(main).await,
                        Err(DurableSmError::RuntimeAttach(
                            SmRuntimeAttachError::UnexpectedExecutableSchema { component }
                        )) if component == table
                    ),
                    "the durable session store must refuse after: {deviation:?}"
                );
            } else {
                assert!(
                    matches!(
                        DatabaseAccountBindingStore::open(main.clone(), false).await,
                        Err(AccountBindingStoreError::RuntimeAttach(
                            AccountBindingAttachError::UnexpectedExecutableSchema { component }
                        )) if component == table
                    ),
                    "the account binding store must refuse after: {deviation:?}"
                );
            }
            for statement in &undo {
                execute(&prepared.admin, statement).await;
            }
            assert_guard_pair(&prepared, table).await;
        }
    }

    open_durable_sm(main)
        .await
        .expect("the durable session store attaches to the restored guards");
    DatabaseAccountBindingStore::open(main.clone(), false)
        .await
        .expect("the account binding store attaches to the restored guards");

    drop(pool);
    prepared.cleanup().await;
}
