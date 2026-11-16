//! Lineage enrollment by the owner phase when no lineage action is set.
//!
//! A first install runs the owner phase without `WADDLE_DB_LINEAGE_ACTION`.
//! The phase enrolls a target schema that holds no relation besides the
//! lineage table, and only that: a lineage row that names another deployment
//! is refused rather than replaced, and an unenrolled schema that already
//! holds relations still needs the explicit action.

use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, DeploymentUuid, LineageError};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabaseError};
use waddle_server::server::runtime_owner::RestrictedRuntimeOwnerError;
use waddle_ws_test_support::require_test_postgres_url;

use super::support::{execute, run_owner_phase, Provisioned, SCHEMA};

async fn owner_database(provisioned: &Provisioned) -> Database {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, provisioned.owner_url.clone());
    config.pool_size = 1;
    Database::from_config("lineage enrollment owner", &config)
        .await
        .expect("owner database")
}

async fn lineage_rows(provisioned: &Provisioned) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {SCHEMA}._lineage"))
        .fetch_one(&provisioned.admin)
        .await
        .expect("count lineage rows")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_enrolls_a_fresh_schema_without_an_action() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let provisioned = Provisioned::new(&url).await;
    let mut plan = provisioned.plan.clone();
    plan.lineage.action = None;
    let deployment = plan
        .lineage
        .deployment_uuid
        .expect("fixture deployment identity");

    let enrolled = run_owner_phase(&provisioned.owner_url, &plan)
        .await
        .expect("the owner phase enrolls a fresh schema");
    assert_eq!(enrolled.deployment_uuid, deployment);
    assert!(enrolled.postgres_identity.is_some());
    assert_eq!(lineage_rows(&provisioned).await, 1);

    // A repeat attests the same enrollment instead of writing another.
    let repeated = run_owner_phase(&provisioned.owner_url, &plan)
        .await
        .expect("the owner phase attests its own enrollment");
    assert_eq!(repeated, enrolled);

    // Another deployment pointed at the schema is refused, and the row keeps
    // naming the deployment that enrolled it.
    let mut other = plan.clone();
    other.lineage.deployment_uuid = Some(DeploymentUuid(Uuid::new_v4()));
    let refused = run_owner_phase(&provisioned.owner_url, &other).await;
    assert!(
        matches!(refused, Err(RestrictedRuntimeOwnerError::Global(_))),
        "{refused:?}"
    );
    let kept = run_owner_phase(&provisioned.owner_url, &plan)
        .await
        .expect("the enrolled deployment still attests");
    assert_eq!(kept, enrolled);

    // A schema that holds relations but no lineage row is not fresh: the
    // phase refuses it and writes no row.
    execute(
        &provisioned.admin,
        &format!("DELETE FROM {SCHEMA}._lineage"),
    )
    .await;
    let refused = run_owner_phase(&provisioned.owner_url, &plan).await;
    assert!(
        matches!(
            refused,
            Err(RestrictedRuntimeOwnerError::Lineage(
                DatabaseError::Lineage(LineageError::MissingRow)
            ))
        ),
        "{refused:?}"
    );
    assert_eq!(lineage_rows(&provisioned).await, 0);

    provisioned.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_never_enrolls_over_an_existing_lineage() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let provisioned = Provisioned::new(&url).await;

    // The schema holds nothing but a lineage row for another deployment.
    let existing = LineageConfig {
        deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
        action: None,
    };
    let owner = owner_database(&provisioned).await;
    let enrolled = lineage::enroll(&owner, &existing)
        .await
        .expect("enroll another deployment");
    drop(owner);

    let mut plan = provisioned.plan.clone();
    plan.lineage.action = None;
    let refused = run_owner_phase(&provisioned.owner_url, &plan).await;
    assert!(
        matches!(refused, Err(RestrictedRuntimeOwnerError::Global(_))),
        "{refused:?}"
    );
    assert_eq!(lineage_rows(&provisioned).await, 1);

    // The row is the one written for the other deployment, unchanged.
    let owner = owner_database(&provisioned).await;
    let attested = lineage::verify(&owner, &existing)
        .await
        .expect("the existing lineage still attests");
    assert_eq!(attested, enrolled);
    drop(owner);

    provisioned.cleanup().await;
}
