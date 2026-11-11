//! Isolated start-quarantine schedules. No serving or provider is selected.

use std::cell::Cell;
use std::path::Path;
use std::task::Poll;
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt;
use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::DeploymentUuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_uow::{DbRetryClass, IngressUnitOfWork, ObservationError};
use waddle_server::test_support::observer_quarantine::{
    claim_observer_pilot, observer_pilot_seed, prepare_observer_pilot_start, seed_observer_pilot,
    ObserverPilotClaim, ObserverPilotCustody, ObserverPilotSeed,
};
use waddle_xmpp::ingress::MessageKey;

use super::{test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

const GUARD: &str = include_str!("../../src/test_support/observer_quarantine_fixture.sql");

fn role_url(fixture: &Fixture, role: &str, app: &str) -> String {
    let mut url =
        url::Url::parse(&url_with_schema(&fixture.base_url, &fixture.target)).expect("URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "application_name")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", role)
        .append_pair("application_name", app);
    url.to_string()
}

async fn uow(url: &str, deployment: DeploymentUuid) -> IngressUnitOfWork {
    uow_with_pool_size(url, deployment, None).await
}

async fn uow_with_pool_size(
    url: &str,
    deployment: DeploymentUuid,
    pool_size: Option<u32>,
) -> IngressUnitOfWork {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_owned());
    if let Some(pool_size) = pool_size {
        config.pool_size = pool_size;
    }
    let db = Database::from_config("observer-quarantine-fixture", &config)
        .await
        .expect("fixture database");
    IngressUnitOfWork::open(
        db,
        LineageConfig {
            deployment_uuid: Some(deployment),
            action: None,
        },
    )
    .expect("fixture UOW")
}

struct Pilot {
    fixture: Fixture,
    seed: ObserverPilotSeed,
    owner: IngressUnitOfWork,
    claimer: IngressUnitOfWork,
    starter: IngressUnitOfWork,
    now: i64,
}

impl Pilot {
    async fn new() -> Self {
        let fixture = Fixture::new(test_url().expect("PostgreSQL URL")).await;
        eprintln!("observer quarantine fixture: {}", fixture.database_name);
        let setup = std::panic::AssertUnwindSafe(Self::setup(&fixture))
            .catch_unwind()
            .await;
        match setup {
            Ok((seed, owner, claimer, starter, now)) => Self {
                fixture,
                seed,
                owner,
                claimer,
                starter,
                now,
            },
            Err(failure) => {
                cleanup_fixture(fixture).await;
                std::panic::resume_unwind(failure);
            }
        }
    }

    async fn setup(
        fixture: &Fixture,
    ) -> (
        ObserverPilotSeed,
        IngressUnitOfWork,
        IngressUnitOfWork,
        IngressUnitOfWork,
        i64,
    ) {
        let admin_url = url_with_schema(&fixture.base_url, &fixture.target);
        let db = Database::from_config(
            "observer-quarantine-migrations",
            &DatabaseConfig::new(DatabaseDriver::Postgres, admin_url.clone()),
        )
        .await
        .expect("migration database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("registered migrations");
        let owner = IngressUnitOfWork::open(
            db,
            LineageConfig {
                deployment_uuid: Some(fixture.lineage.deployment_uuid),
                action: None,
            },
        )
        .expect("owner UOW");
        let observed_at = Utc::now();
        let seed = seed_observer_pilot(&owner, observed_at)
            .await
            .expect("seed real observer work");
        let work_id: String = sqlx::query_scalar(&format!(
            "SELECT id FROM {}.extension_room_observation_work",
            fixture.target
        ))
        .fetch_one(&fixture.admin)
        .await
        .expect("fixture work ID");
        let sql = GUARD
            .replace("__SCHEMA__", &fixture.target)
            .replace("__START_ROLE__", &fixture.runtime)
            .replace("__CLAIM_ROLE__", &fixture.outsider)
            .replace("__WORK_ID__", &work_id)
            .replace(
                "__MESSAGE_KEY__",
                &seed.message_key().to_storage().to_string(),
            )
            .replace(
                "__DELIVERY_KEY__",
                &seed.delivery_key().to_storage().to_string(),
            );
        sqlx::raw_sql(&sql)
            .execute(&fixture.admin)
            .await
            .expect("install disposable guard");
        for sql in [
            format!(
                "GRANT USAGE ON SCHEMA {} TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT SELECT ON ALL TABLES IN SCHEMA {} TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT UPDATE (row_revision) ON {}.ingress_messages TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT UPDATE (plugin_id) ON {}.extension_room_observers TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT UPDATE (source_key) ON {}.extension_room_sources TO {}, {}",
                fixture.target, fixture.runtime, fixture.outsider
            ),
            format!(
                "GRANT UPDATE (status) ON {}.extension_room_observation_work TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "GRANT UPDATE ON {}.extension_room_observation_work TO {}",
                fixture.target, fixture.outsider
            ),
            format!(
                "GRANT INSERT ON {}.ingress_deliveries TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "CREATE TABLE {}.observer_fixture_effects (work_id TEXT NOT NULL)",
                fixture.target
            ),
            format!(
                "GRANT INSERT ON {}.observer_fixture_effects TO {}",
                fixture.target, fixture.runtime
            ),
        ] {
            sqlx::query(&sql)
                .execute(&fixture.admin)
                .await
                .expect("fixture role grant");
        }
        let claimer = uow(
            &role_url(fixture, &fixture.outsider, "observer-pilot-claim"),
            fixture.lineage.deployment_uuid,
        )
        .await;
        let starter = uow(
            &role_url(fixture, &fixture.runtime, "observer-pilot-start"),
            fixture.lineage.deployment_uuid,
        )
        .await;
        let safe_roles: bool = sqlx::query_scalar(
            "SELECT count(*) = 2 AND bool_and(rolcanlogin AND NOT rolsuper AND NOT rolcreaterole \
             AND NOT rolcreatedb AND NOT rolreplication AND NOT rolbypassrls \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid OR m.roleid = r.oid) \
             AND NOT pg_catalog.has_function_privilege(r.oid, 'pg_catalog.pg_control_system()', 'EXECUTE')) \
             FROM pg_catalog.pg_roles r WHERE rolname IN ($1, $2)",
        )
        .bind(&fixture.runtime)
        .bind(&fixture.outsider)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect isolated role membership before grant");
        assert!(safe_roles, "fixture roles are not isolated");
        sqlx::query(&format!(
            "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}, {}",
            fixture.runtime, fixture.outsider
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant fixture-only lineage inspection");
        (
            seed,
            owner,
            claimer,
            starter,
            observed_at.timestamp_millis(),
        )
    }

    async fn status(&self) -> String {
        sqlx::query_scalar(&format!(
            "SELECT status FROM {}.extension_room_observation_work",
            self.fixture.target
        ))
        .fetch_one(&self.fixture.admin)
        .await
        .expect("work status")
    }

    async fn effects(&self) -> i64 {
        sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.observer_fixture_effects",
            self.fixture.target
        ))
        .fetch_one(&self.fixture.admin)
        .await
        .expect("independent effect count")
    }

    async fn fence_rollback(&self) {
        sqlx::query(&format!(
            "SELECT message_key FROM {}.ingress_messages FOR UPDATE",
            self.fixture.target
        ))
        .fetch_one(&self.fixture.admin)
        .await
        .expect("wait for prior transaction rollback");
    }

    async fn cleanup(self) {
        drop(self.owner);
        drop(self.claimer);
        drop(self.starter);
        cleanup_fixture(self.fixture).await;
    }
}

async fn cleanup_fixture(fixture: Fixture) {
    let revoke = sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}, {}",
        fixture.runtime, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await;
    let effective = sqlx::query_scalar::<_, bool>(
        "SELECT count(*) = 2 AND bool_and(NOT pg_catalog.has_function_privilege(\
         r.oid, 'pg_catalog.pg_control_system()', 'EXECUTE')) \
         FROM pg_catalog.pg_roles r WHERE rolname IN ($1, $2)",
    )
    .bind(&fixture.runtime)
    .bind(&fixture.outsider)
    .fetch_one(&fixture.admin)
    .await;
    let database = fixture.database_name.clone();
    let roles = [
        fixture.runtime.clone(),
        fixture.outsider.clone(),
        fixture.installer.clone(),
        fixture.definer.clone(),
    ];
    let cleanup = std::panic::AssertUnwindSafe(fixture.cleanup())
        .catch_unwind()
        .await;
    let verified = async {
        let mut conn = PgConnection::connect(&test_url().expect("PostgreSQL URL")).await?;
        sqlx::query_scalar::<_, bool>(
            "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_database WHERE datname=$1) \
             AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=ANY($2))",
        )
        .bind(&database)
        .bind(&roles[..])
        .fetch_one(&mut conn)
        .await
    }
    .await;
    assert!(
        revoke.is_ok()
            && matches!(effective, Ok(true))
            && cleanup.is_ok()
            && matches!(verified, Ok(true)),
        "fixture cleanup not proven for database {database} and roles {roles:?}"
    );
}

async fn wait_for_lock(pilot: &Pilot, application: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND wait_event_type = 'Lock')",
            )
            .bind(application)
            .fetch_one(&pilot.fixture.admin)
            .await
            .expect("fixture lock observation");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture operation waits for the start transaction");
}

#[tokio::test]
async fn postgres_observer_quarantine_exact_start_and_guard() {
    let Some(_) = test_url() else {
        return;
    };
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let pilot = Pilot::new().await;
    let outcome = std::panic::AssertUnwindSafe(exercise_exact_start(&pilot))
        .catch_unwind()
        .await;
    pilot.cleanup().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure);
    }
}

async fn exercise_exact_start(pilot: &Pilot) {
    let claim = claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now)
        .await
        .expect("claim")
        .expect("work");
    let mut early_claimer = PgConnection::connect_with(
        &pilot
            .fixture
            .runtime_options
            .clone()
            .username(&pilot.fixture.outsider),
    )
    .await
    .expect("claimer connection");
    assert!(sqlx::query(&format!(
        "UPDATE {}.extension_room_observation_work SET status = 'started'",
        pilot.fixture.target
    ))
    .execute(&mut early_claimer)
    .await
    .is_err());
    drop(early_claimer);
    let mut early_starter = PgConnection::connect_with(&pilot.fixture.runtime_options)
        .await
        .expect("starter connection");
    assert!(sqlx::query(&format!(
        "UPDATE {}.extension_room_observation_work SET status = 'pending'",
        pilot.fixture.target
    ))
    .execute(&mut early_starter)
    .await
    .is_err());
    drop(early_starter);
    for field in [
        "work",
        "token",
        "body",
        "attempt",
        "config",
        "generation",
        "subscription_generation",
        "identity",
        "source",
        "digest",
        "hash",
        "receipt_message",
        "receipt_kind",
        "receipt_hash",
        "kind",
        "payload",
        "message",
    ] {
        let mut wrong = claim.clone();
        wrong.corrupt_for_test(field).expect("corrupt identity");
        let result = prepare_observer_pilot_start(&pilot.starter, &wrong, pilot.now).await;
        assert!(
            !matches!(result, Ok(Some(_))),
            "wrong {field} obtained a grant"
        );
        pilot.fence_rollback().await;
        assert_eq!(
            pilot.status().await,
            "leased",
            "wrong {field} committed a start"
        );
    }
    for column in ["lease_node_id", "lease_node_incarnation"] {
        sqlx::query(&format!(
            "UPDATE {}.extension_room_observation_work SET {column} = 'stale'",
            pilot.fixture.target
        ))
        .execute(&pilot.fixture.admin)
        .await
        .expect("fixture stale owner");
        assert!(
            prepare_observer_pilot_start(&pilot.starter, &claim, pilot.now)
                .await
                .expect("stale check")
                .is_none()
        );
        sqlx::query(&format!(
            "UPDATE {}.extension_room_observation_work SET {column} = 'local'",
            pilot.fixture.target
        ))
        .execute(&pilot.fixture.admin)
        .await
        .expect("restore fixture owner");
    }
    let prepared = prepare_observer_pilot_start(&pilot.starter, &claim, pilot.now)
        .await
        .expect("prepare")
        .expect("valid start");
    let mut changes = Vec::new();
    for (application, sql) in [
        (
            "observer-fixture-config-change",
            format!(
                "UPDATE {}.extension_room_observers SET generation = generation + 1",
                pilot.fixture.target
            ),
        ),
        (
            "observer-fixture-source-change",
            format!(
                "UPDATE {}.extension_room_sources SET retracted = 1",
                pilot.fixture.target
            ),
        ),
        (
            "observer-fixture-canonical-lock",
            format!(
                "SELECT message_key FROM {}.ingress_messages FOR UPDATE",
                pilot.fixture.target
            ),
        ),
    ] {
        let options = pilot
            .fixture
            .admin_options
            .clone()
            .application_name(application);
        changes.push(tokio::spawn(async move {
            let mut conn = PgConnection::connect_with(&options)
                .await
                .expect("change connection");
            sqlx::query(&sql)
                .execute(&mut conn)
                .await
                .expect("change after start");
        }));
        wait_for_lock(pilot, application).await;
    }
    let grant = prepared.commit().await.expect("acknowledged commit");
    for change in changes {
        change.await.expect("change task");
    }
    assert_eq!(pilot.status().await, "dark_started");
    let (generation, retracted): (String, String) = sqlx::query_as(&format!(
        "SELECT dark_snapshot->'observer'->>'generation', dark_snapshot->'source'->>'retracted' \
         FROM {}.extension_room_observation_work",
        pilot.fixture.target
    ))
    .fetch_one(&pilot.fixture.admin)
    .await
    .expect("preserved original snapshot");
    assert_eq!((generation.as_str(), retracted.as_str()), ("1", "0"));
    assert!(
        sqlx::query(&format!(
            "DELETE FROM {}.extension_room_observation_work",
            pilot.fixture.target
        ))
        .execute(&pilot.fixture.admin)
        .await
        .is_err(),
        "fixture guard rejected deletion even by the setup role"
    );
    let mut claimant = PgConnection::connect_with(
        &pilot
            .fixture
            .runtime_options
            .clone()
            .username(&pilot.fixture.outsider),
    )
    .await
    .expect("claimer connection");
    assert!(sqlx::query(&format!(
        "UPDATE {}.extension_room_observation_work SET status = 'pending'",
        pilot.fixture.target
    ))
    .execute(&mut claimant)
    .await
    .is_err());
    let mut starter = PgConnection::connect_with(&pilot.fixture.runtime_options)
        .await
        .expect("starter connection");
    let role_boundary: bool = sqlx::query_scalar(
        "SELECT count(*) = 2 AND bool_and(\
           r.oid <> n.nspowner AND NOT pg_catalog.has_schema_privilege(r.oid,n.oid,'CREATE') \
           AND NOT pg_catalog.has_table_privilege(r.oid,w.oid,'TRIGGER,TRUNCATE,DELETE') \
           AND NOT pg_catalog.has_table_privilege(r.oid,receipts.oid,'INSERT,UPDATE,DELETE,TRUNCATE,TRIGGER') \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member=r.oid OR m.roleid=r.oid)) \
         FROM pg_catalog.pg_roles r \
         JOIN pg_catalog.pg_namespace n ON n.nspname=$3 \
         JOIN pg_catalog.pg_class w ON w.relnamespace=n.oid AND w.relname='extension_room_observation_work' \
         JOIN pg_catalog.pg_class receipts ON receipts.relnamespace=n.oid AND receipts.relname='ingress_effect_receipts' \
         WHERE r.rolname IN ($1,$2)",
    )
    .bind(&pilot.fixture.runtime)
    .bind(&pilot.fixture.outsider)
    .bind(&pilot.fixture.target)
    .fetch_one(&pilot.fixture.admin)
    .await
    .expect("effective fixture role privileges");
    assert!(role_boundary, "fixture roles gained unplanned authority");
    let public_and_triggers: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
           JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
           CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a \
           WHERE n.nspname=$1 AND c.relname IN ('extension_room_observation_work','ingress_messages','ingress_effect_intents','ingress_effect_receipts','ingress_deliveries') \
             AND a.grantee=0 AND a.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE','TRIGGER')) \
         AND (SELECT count(*)=6 AND bool_and(t.tgenabled='O') FROM pg_catalog.pg_trigger t \
           JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
           WHERE n.nspname=$1 AND t.tgname IN ('observer_quarantine_fixture_guard','observer_fixture_lock_only','observer_fixture_delivery_guard'))",
    )
    .bind(&pilot.fixture.target)
    .fetch_one(&pilot.fixture.admin)
    .await
    .expect("PUBLIC ACL and fixture trigger state");
    assert!(public_and_triggers);
    for sql in [
        format!("UPDATE {}.ingress_messages SET row_revision = row_revision", pilot.fixture.target),
        format!("UPDATE {}.ingress_effect_intents SET payload = payload", pilot.fixture.target),
        format!("INSERT INTO {}.ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT message_key, kind, semantic_identity_hash FROM {}.ingress_effect_intents", pilot.fixture.target, pilot.fixture.target),
        format!("INSERT INTO {}.ingress_deliveries (message_key, delivery_key) VALUES ('{}', '{}')", pilot.fixture.target, pilot.seed.message_key().to_storage(), uuid::Uuid::new_v4()),
        format!("INSERT INTO {}.ingress_deliveries (message_key, delivery_key) VALUES ('{}', '{}')", pilot.fixture.target, uuid::Uuid::new_v4(), pilot.seed.delivery_key().to_storage()),
        format!("ALTER TABLE {}.extension_room_observation_work DISABLE TRIGGER observer_quarantine_fixture_guard", pilot.fixture.target),
    ] {
        assert!(sqlx::query(&sql).execute(&mut starter).await.is_err(), "pilot role wrote an alternate authority row");
    }
    assert!(sqlx::query(&format!(
        "UPDATE {}.extension_room_observation_work SET status = 'pending'",
        pilot.fixture.target
    ))
    .execute(&mut starter)
    .await
    .is_err());
    let mut legacy = PgConnection::connect_with(&pilot.fixture.installer_options)
        .await
        .expect("legacy connection");
    assert!(sqlx::query(&format!(
        "UPDATE {}.extension_room_observation_work SET status = 'pending'",
        pilot.fixture.target
    ))
    .execute(&mut legacy)
    .await
    .is_err());
    assert!(
        claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now + 180_000)
            .await
            .expect("expired query")
            .is_none()
    );
    let table = format!("{}.observer_fixture_effects", pilot.fixture.target);
    let mut custody = grant.into_custody();
    custody
        .invoke(&claim, || async {
            sqlx::query(&format!("INSERT INTO {table} (work_id) VALUES ($1)"))
                .bind(claim.work_id().to_string())
                .execute(&mut starter)
                .await
                .expect("fixture callback");
            uuid::Uuid::new_v4()
        })
        .await
        .expect("fixture callback entered");
    assert_eq!(pilot.effects().await, 1);
    drop(claimant);
    drop(starter);
    drop(legacy);
}

#[tokio::test]
async fn postgres_observer_quarantine_rollback_and_competing_start() {
    let Some(_) = test_url() else {
        return;
    };
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let pilot = Pilot::new().await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let first = claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now)
            .await
            .expect("claim")
            .expect("work");
        let prepared = prepare_observer_pilot_start(&pilot.starter, &first, pilot.now)
            .await
            .expect("prepare")
            .expect("start");
        drop(prepared);
        pilot.fence_rollback().await;
        let retry = claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now + 180_000)
            .await
            .expect("reclaim")
            .expect("expired unstarted reservation");
        assert_ne!(first.lease(), retry.lease());
        let now = pilot.now + 180_000;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let mut contenders = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let starter = pilot.starter.clone();
            let claim = retry.clone();
            contenders.push(tokio::spawn(async move {
                barrier.wait().await;
                match prepare_observer_pilot_start(&starter, &claim, now).await {
                    Ok(Some(start)) => Some(start.commit().await.expect("start commit")),
                    Ok(None) => None,
                    Err(error) => {
                        assert!(matches!(
                            error.downcast_ref::<ObservationError>(),
                            Some(ObservationError::RetryableDatabase(
                                DbRetryClass::CanonicalLockContention
                            ))
                        ));
                        None
                    }
                }
            }));
        }
        barrier.wait().await;
        let mut grants = Vec::new();
        for contender in contenders {
            if let Some(grant) = contender.await.expect("contender") {
                grants.push(grant);
            }
        }
        assert_eq!(grants.len(), 1, "one competing start receives a grant");
        assert!(prepare_observer_pilot_start(&pilot.starter, &retry, now)
            .await
            .expect("second start")
            .is_none());
        drop(grants);
        assert_eq!(pilot.status().await, "dark_started");
        assert_eq!(pilot.effects().await, 0);
    })
    .catch_unwind()
    .await;
    pilot.cleanup().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure);
    }
}

#[tokio::test]
async fn postgres_observer_quarantine_result_custody() {
    let Some(_) = test_url() else {
        return;
    };
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for scenario in ["identity_and_retry", "cancel_during", "panic_during"] {
        let pilot = Pilot::new().await;
        let outcome = std::panic::AssertUnwindSafe(exercise_result_custody(&pilot, scenario))
            .catch_unwind()
            .await;
        pilot.cleanup().await;
        if let Err(failure) = outcome {
            std::panic::resume_unwind(failure);
        }
    }
}

async fn exercise_result_custody(pilot: &Pilot, scenario: &str) {
    let claim = claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now)
        .await
        .expect("claim")
        .expect("work");
    let grant = prepare_observer_pilot_start(&pilot.starter, &claim, pilot.now)
        .await
        .expect("prepare")
        .expect("start")
        .commit()
        .await
        .expect("acknowledged start");
    assert!(
        prepare_observer_pilot_start(&pilot.starter, &claim, pilot.now)
            .await
            .expect("duplicate start")
            .is_none()
    );
    let mut custody = grant.into_custody();
    match scenario {
        "identity_and_retry" => exercise_identity_and_retry(&claim, &mut custody).await,
        "cancel_during" => exercise_cancel_during(&claim, &mut custody).await,
        "panic_during" => exercise_panic_during(&claim, &mut custody).await,
        _ => panic!("unknown fixture scenario"),
    }
    assert_eq!(pilot.status().await, "dark_started");
    assert_eq!(pilot.effects().await, 0);
}

async fn exercise_identity_and_retry(
    claim: &ObserverPilotClaim,
    custody: &mut ObserverPilotCustody,
) {
    let calls = Cell::new(0);
    let token = uuid::Uuid::new_v4();
    let fields = [
        "work",
        "token",
        "body",
        "attempt",
        "config",
        "generation",
        "subscription_generation",
        "identity",
        "source",
        "digest",
        "hash",
        "receipt_message",
        "receipt_kind",
        "receipt_hash",
        "kind",
        "payload",
        "message",
    ];
    for field in fields {
        let mut wrong = claim.clone();
        wrong.corrupt_for_test(field).expect("corrupt identity");
        assert!(
            custody
                .invoke(&wrong, || async {
                    calls.set(calls.get() + 1);
                    token
                })
                .await
                .is_err(),
            "wrong {field} entered the callback"
        );
    }
    assert_eq!(calls.get(), 0);

    // Creating and dropping a future without polling does not enter its body.
    let unpolled = custody.invoke(claim, || async {
        calls.set(calls.get() + 1);
        token
    });
    drop(unpolled);
    assert_eq!(calls.get(), 0);
    assert!(custody.retry(claim, |_| ()).is_err());
    let invocation = custody.invoke(claim, || async {
        calls.set(calls.get() + 1);
        token
    });
    let mut invocation = Box::pin(invocation);
    assert!(matches!(
        futures::poll!(invocation.as_mut()),
        Poll::Ready(Ok(()))
    ));
    // Dropping the completed future cannot discard the returned value.
    drop(invocation);
    assert_eq!(calls.get(), 1);

    for field in fields {
        let mut wrong = claim.clone();
        wrong.corrupt_for_test(field).expect("corrupt identity");
        assert!(
            custody.retry(&wrong, |_| ()).is_err(),
            "wrong {field} used the result"
        );
    }
    let original = custody
        .retry(claim, std::convert::identity)
        .expect("original returned value");
    assert_eq!(original, token);
    let attempts = Cell::new(0);
    let attempts_ref = &attempts;
    let retry = custody
        .retry(claim, |result| async move {
            attempts_ref.set(attempts_ref.get() + 1);
            assert_eq!(result, token);
            // Altering the retry's own copy cannot alter the value in custody.
            let mut altered_bytes = result.into_bytes();
            altered_bytes[0] ^= 0xff;
            assert_ne!(uuid::Uuid::from_bytes(altered_bytes), result);
            std::future::pending::<()>().await;
        })
        .expect("fixture retry");
    let mut retry = Box::pin(retry);
    assert!(matches!(futures::poll!(retry.as_mut()), Poll::Pending));
    drop(retry);
    assert_eq!(attempts.get(), 1);
    let actual = custody
        .retry(claim, std::convert::identity)
        .expect("retry after cancellation");
    assert_eq!(actual, original);
    assert_eq!(actual, token);
    assert!(custody
        .invoke(claim, || async {
            calls.set(calls.get() + 1);
            token
        })
        .await
        .is_err());
    assert_eq!(calls.get(), 1, "retry never invoked the callback again");
}

async fn exercise_cancel_during(claim: &ObserverPilotClaim, custody: &mut ObserverPilotCustody) {
    let calls = Cell::new(0);
    let invocation = custody.invoke(claim, || async {
        calls.set(calls.get() + 1);
        std::future::pending::<uuid::Uuid>().await
    });
    let mut invocation = Box::pin(invocation);
    assert!(matches!(futures::poll!(invocation.as_mut()), Poll::Pending));
    assert_eq!(calls.get(), 1, "the callback was entered");
    drop(invocation);
    assert!(custody.retry(claim, |_| ()).is_err());
    assert!(custody
        .invoke(claim, || async {
            calls.set(calls.get() + 1);
            uuid::Uuid::new_v4()
        })
        .await
        .is_err());
    assert_eq!(calls.get(), 1, "a cancelled invocation stays uncertain");
}

async fn exercise_panic_during(claim: &ObserverPilotClaim, custody: &mut ObserverPilotCustody) {
    let calls = Cell::new(0);
    let invocation = custody.invoke(claim, || async {
        calls.set(calls.get() + 1);
        panic!("fixture callback panic");
    });
    assert!(std::panic::AssertUnwindSafe(invocation)
        .catch_unwind()
        .await
        .is_err());
    assert!(custody.retry(claim, |_| ()).is_err());
    assert!(custody
        .invoke(claim, || async {
            calls.set(calls.get() + 1);
            uuid::Uuid::new_v4()
        })
        .await
        .is_err());
    assert_eq!(calls.get(), 1, "a panicked invocation stays uncertain");
}

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture process signal");
}

// Hold the actual PostgreSQL COMMIT response until a fresh backend has read
// the committed row, then close both sides without forwarding the response.
async fn lose_start_commit_reply(
    listener: tokio::net::TcpListener,
    upstream: sqlx::postgres::PgConnectOptions,
    held: oneshot::Sender<()>,
    disconnect: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    });
    loop {
        let tag = server_read.read_u8().await?;
        let length = server_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::other("invalid backend frame"));
        }
        let mut body = vec![0; (length - 4) as usize];
        server_read.read_exact(&mut body).await?;
        if tag == b'C' && body == b"COMMIT\0" {
            held.send(())
                .map_err(|_| std::io::Error::other("holder dropped"))?;
            disconnect
                .await
                .map_err(|_| std::io::Error::other("disconnect dropped"))?;
            break;
        }
        client_write.write_u8(tag).await?;
        client_write.write_u32(length).await?;
        client_write.write_all(&body).await?;
    }
    forward.abort();
    let _ = forward.await;
    client_write.shutdown().await
}

#[tokio::test]
#[ignore = "subprocess entry for observer quarantine crash schedules"]
async fn observer_child() {
    let value: serde_json::Value =
        serde_json::from_str(&std::env::var("WADDLE_OBSERVER_PILOT_CHILD").expect("child input"))
            .expect("JSON");
    let text = |key: &str| value[key].as_str().expect("child field");
    let deployment = DeploymentUuid(uuid::Uuid::parse_str(text("deployment")).expect("deployment"));
    let seed = observer_pilot_seed(MessageKey::from_storage(
        uuid::Uuid::parse_str(text("message")).expect("message"),
    ))
    .expect("seed");
    let claimer = uow(text("claim_url"), deployment).await;
    let expect_error = value["expect_commit_error"].as_bool() == Some(true);
    let starter =
        uow_with_pool_size(text("start_url"), deployment, expect_error.then_some(1)).await;
    let now = value["now"].as_i64().expect("clock");
    let work = claim_observer_pilot(&claimer, &seed, now)
        .await
        .expect("child claim");
    if value["check_only"].as_bool() == Some(true) {
        assert!(work.is_none(), "replacement cannot claim a dark start");
        std::fs::write(text("probe"), "unclaimable").expect("signal replacement");
        return;
    }
    let claim = work.expect("work");
    let start = prepare_observer_pilot_start(&starter, &claim, now)
        .await
        .expect("child prepare")
        .expect("start");
    std::fs::write(text("prepared"), "prepared").expect("signal prepared");
    wait_file(Path::new(text("commit"))).await;
    let result = start.commit().await;
    let grant = match result {
        Err(_) if expect_error => {
            std::fs::write(text("commit_error"), "error").expect("signal COMMIT error");
            return;
        }
        result => result.expect("child acknowledged commit"),
    };
    std::fs::write(text("granted"), "granted").expect("signal grant");
    if !expect_error {
        wait_file(Path::new(text("invoke"))).await;
    }
    let mut custody = grant.into_custody();
    custody
        .invoke(&claim, || async {
            std::fs::write(text("callback"), "callback").expect("signal callback");
            let mut conn = PgConnection::connect(text("effect_url"))
                .await
                .expect("effect connection");
            sqlx::query(&format!(
                "INSERT INTO {}.observer_fixture_effects (work_id) VALUES ($1)",
                text("schema")
            ))
            .bind(claim.work_id().to_string())
            .execute(&mut conn)
            .await
            .expect("independent effect");
            uuid::Uuid::new_v4()
        })
        .await
        .expect("fixture callback entered");
}

#[tokio::test]
async fn postgres_observer_quarantine_process_crash_schedules() {
    let Some(_) = test_url() else {
        return;
    };
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for mode in ["before_commit", "lost_ack", "after_grant", "changed_source"] {
        process_schedule(mode).await;
    }
}

#[tokio::test]
async fn postgres_observer_quarantine_lost_commit_response() {
    let Some(_) = test_url() else {
        return;
    };
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    process_schedule("lost_response").await;
}

async fn process_schedule(mode: &str) {
    let pilot = Pilot::new().await;
    let outcome = std::panic::AssertUnwindSafe(exercise_process_schedule(&pilot, mode))
        .catch_unwind()
        .await;
    pilot.cleanup().await;
    if let Err(failure) = outcome {
        std::panic::resume_unwind(failure);
    }
}

async fn exercise_process_schedule(pilot: &Pilot, mode: &str) {
    let dir = tempfile::tempdir().expect("child signals");
    let file = |name| dir.path().join(name);
    let prepared = file("prepared");
    let commit = file("commit");
    let granted = file("granted");
    let invoke = file("invoke");
    let callback = file("callback");
    let commit_error = file("commit_error");
    let probe = file("probe");
    let mut blocker = if mode == "lost_ack" {
        let mut conn = PgConnection::connect_with(&pilot.fixture.admin_options)
            .await
            .expect("blocker");
        sqlx::query("SELECT pg_advisory_lock(2229001)")
            .execute(&mut conn)
            .await
            .expect("commit barrier");
        sqlx::raw_sql(&format!("CREATE FUNCTION {}.observer_commit_barrier() RETURNS trigger LANGUAGE plpgsql SET lock_timeout = '10s' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(2229001); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER observer_commit_barrier AFTER UPDATE ON {}.extension_room_observation_work DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.status = 'dark_started') EXECUTE FUNCTION {}.observer_commit_barrier()", pilot.fixture.target, pilot.fixture.target, pilot.fixture.target))
            .execute(&pilot.fixture.admin).await.expect("deferred commit barrier");
        Some(conn)
    } else {
        None
    };
    let effect_url = role_url(
        &pilot.fixture,
        &pilot.fixture.runtime,
        "observer-pilot-child-start",
    );
    let (start_url, response_drop) = if mode == "lost_response" {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local proxy listener");
        let port = listener.local_addr().expect("proxy port").port();
        let (held, waiting) = oneshot::channel();
        let (disconnect, disconnected) = oneshot::channel();
        let proxy = tokio::spawn(lose_start_commit_reply(
            listener,
            pilot.fixture.admin_options.clone(),
            held,
            disconnected,
        ));
        let start_url = crate::postgres_proxy::local_proxy_url(&effect_url, port)
            .expect("local proxy URL")
            .to_string();
        (start_url, Some((waiting, disconnect, proxy)))
    } else {
        (effect_url.clone(), None)
    };
    let input = serde_json::json!({
        "deployment": pilot.fixture.lineage.deployment_uuid.0.to_string(),
        "message": pilot.seed.message_key().to_storage().to_string(),
        "claim_url": role_url(&pilot.fixture, &pilot.fixture.outsider, "observer-pilot-child-claim"),
        "start_url": start_url, "effect_url": effect_url,
        "now": pilot.now,
        "schema": pilot.fixture.target,
        "prepared": prepared, "commit": commit, "granted": granted, "invoke": invoke,
        "callback": callback, "commit_error": commit_error, "probe": probe,
        "expect_commit_error": mode == "lost_response",
    });
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .arg("--ignored")
        .arg("--exact")
        .arg("observer_quarantine::observer_child")
        .arg("--nocapture")
        .env("WADDLE_OBSERVER_PILOT_CHILD", input.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect("child process");
    wait_file(&prepared).await;
    if mode == "before_commit" {
        child.start_kill().expect("kill before COMMIT");
        child.wait().await.expect("child exit");
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let gone: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name = 'observer-pilot-child-start')")
                    .fetch_one(&pilot.fixture.admin).await.expect("old backend state");
                if gone { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("old backend fenced before reclaim");
        let retry = claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now + 180_000)
            .await
            .expect("reclaim");
        assert!(retry.is_some(), "rolled back start remains reclaimable");
    } else {
        std::fs::write(&commit, "commit").expect("release commit");
        if let Some((waiting, disconnect, proxy)) = response_drop {
            tokio::time::timeout(Duration::from_secs(10), waiting)
                .await
                .expect("COMMIT response held")
                .expect("server emitted CommandComplete COMMIT");
            assert!(child.try_wait().expect("child state").is_none());
            let mut reader = PgConnection::connect_with(
                &pilot
                    .fixture
                    .admin_options
                    .clone()
                    .application_name("observer-pilot-fresh-reader"),
            )
            .await
            .expect("fresh readback connection");
            let reader_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut reader)
                .await
                .expect("reader backend");
            let starter_pid: i32 = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='observer-pilot-child-start'",
            )
            .fetch_one(&mut reader)
            .await
            .expect("original start backend");
            assert_ne!(reader_pid, starter_pid);
            let (work_id, status, snapshot): (String, String, String) = sqlx::query_as(&format!(
                "SELECT id, status, dark_snapshot::text FROM {}.extension_room_observation_work",
                pilot.fixture.target
            ))
            .fetch_one(&mut reader)
            .await
            .expect("fresh committed row");
            let snapshot: serde_json::Value = serde_json::from_str(&snapshot).expect("snapshot");
            assert_eq!(status, "dark_started");
            assert_eq!(snapshot["work"]["id"], work_id);
            assert_eq!(
                snapshot["work"]["message_key"],
                pilot.seed.message_key().to_storage().to_string()
            );
            drop(reader);
            assert!(child.try_wait().expect("child remains live").is_none());
            disconnect.send(()).expect("drop COMMIT response");
            proxy.await.expect("proxy task").expect("proxy");
            let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .expect("live caller returned")
                .expect("child exit");
            assert!(status.success(), "caller did not return cleanly");
            assert!(commit_error.exists(), "caller did not observe COMMIT error");
            assert!(!granted.exists(), "indeterminate COMMIT minted a grant");
            assert!(!callback.exists(), "indeterminate COMMIT entered callback");
            assert_eq!(pilot.effects().await, 0);
        } else if mode == "lost_ack" {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name = 'observer-pilot-child-start' AND query = 'COMMIT' AND wait_event_type = 'Lock')")
                        .fetch_one(&pilot.fixture.admin).await.expect("commit state");
                    if waiting { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("child blocked inside COMMIT");
            waddle_ws_test_support::suspend_child_process(child.id().expect("child PID"));
            sqlx::query("SELECT pg_advisory_unlock(2229001)")
                .execute(blocker.as_mut().expect("blocker"))
                .await
                .expect("release barrier");
            tokio::time::timeout(Duration::from_secs(8), async {
                while pilot.status().await != "dark_started" {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("commit visible");
            child.start_kill().expect("kill before ACK consumed");
            child.wait().await.expect("child exit");
            assert!(!granted.exists(), "indeterminate start minted no grant");
        } else {
            wait_file(&granted).await;
            if mode == "changed_source" {
                for sql in [
                    format!(
                        "UPDATE {}.extension_room_observers SET generation = generation + 1",
                        pilot.fixture.target
                    ),
                    format!(
                        "UPDATE {}.extension_room_sources SET retracted = 1",
                        pilot.fixture.target
                    ),
                ] {
                    sqlx::query(&sql)
                        .execute(&pilot.fixture.admin)
                        .await
                        .expect("change fixture authority");
                }
                std::fs::write(&invoke, "invoke").expect("allow callback");
                assert!(child.wait().await.expect("child exit").success());
                assert_eq!(pilot.effects().await, 1);
            } else {
                child.start_kill().expect("crash after grant");
                child.wait().await.expect("child exit");
            }
        }
        assert_eq!(pilot.status().await, "dark_started");
        assert!(
            claim_observer_pilot(&pilot.claimer, &pilot.seed, pilot.now + 180_000)
                .await
                .expect("restart query")
                .is_none()
        );
        let mut replacement = input.clone();
        replacement["check_only"] = true.into();
        replacement["now"] = (pilot.now + 180_000).into();
        replacement["start_url"] = replacement["effect_url"].clone();
        let status =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .arg("--ignored")
                .arg("--exact")
                .arg("observer_quarantine::observer_child")
                .arg("--nocapture")
                .env("WADDLE_OBSERVER_PILOT_CHILD", replacement.to_string())
                .status()
                .await
                .expect("replacement process");
        assert!(status.success());
        assert!(
            probe.exists(),
            "independent replacement checked the durable row"
        );
    }
    if mode != "changed_source" {
        assert_eq!(pilot.effects().await, 0);
    }
    drop(blocker);
}
