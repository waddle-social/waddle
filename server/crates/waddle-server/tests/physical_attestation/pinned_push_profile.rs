//! PG17 witnesses for the rollback-only pinned Push-family profile.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use sqlx::postgres::types::Oid;
use sqlx::postgres::PgPool;
use waddle_server::db::physical_attestation::{
    PinnedPartialProfileError, PinnedPushProfileError, PinnedPushProfilePinsV1,
    PinnedSelectedEntryError, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::pubsub::RestrictedServingProfileV1;

use super::pinned_pubsub_inventory::InventoryFixture;

const HELPER_BODY_TAG: &str = "v1_pubsub_lock";
const PINNED_PUSH_PUBSUB_GATE_KEY: i64 = 7_534_519_881_230_429_064;

async fn relation_oid(admin: &PgPool, name: &str) -> anyhow::Result<u32> {
    let oid: Oid = sqlx::query_scalar(
        "SELECT c.oid FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'guarded' AND c.relname = $1",
    )
    .bind(name)
    .fetch_one(admin)
    .await?;
    Ok(oid.0)
}

async fn profile_pins(
    admin: &PgPool,
    roles: (&str, &str, &str, &str),
    pubsub: &RestrictedServingProfileV1,
) -> anyhow::Result<PinnedPushProfilePinsV1> {
    let (runtime, api, helper, migrator) = roles;
    let partial = super::pinned_partial_profile::pins(admin, api, helper, migrator).await?;
    let mut push_relation_oids = [0; 17];
    for (slot, name) in push_relation_oids
        .iter_mut()
        .zip(PinnedPushProfilePinsV1::push_relation_names())
    {
        *slot = relation_oid(admin, name).await?;
    }
    ensure!(
        runtime != api && runtime != helper && api != helper,
        "fixture role names are not distinct"
    );
    Ok(PinnedPushProfilePinsV1 {
        partial,
        pubsub: pubsub.clone(),
        push_relation_oids,
    })
}

fn helper_body() -> anyhow::Result<String> {
    let source = include_str!("../../src/db/migrations/owner/serving_role_graph_v1_candidate.sql");
    Ok(source
        .split_once(&format!("AS ${HELPER_BODY_TAG}$"))
        .context("Push profile helper source start")?
        .1
        .split_once(&format!("${HELPER_BODY_TAG}$"))
        .context("Push profile helper source end")?
        .0
        .replace("__SCHEMA__", "\"guarded\""))
}

async fn replace_helper_body(admin: &PgPool, body: &str) -> anyhow::Result<()> {
    let statement = format!(
        "CREATE OR REPLACE FUNCTION guarded.pubsub_runtime_attach_lock() \
         RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER \
         SET search_path = pg_catalog, pg_temp AS ${HELPER_BODY_TAG}${body}${HELPER_BODY_TAG}$"
    );
    sqlx::query(&statement).execute(admin).await?;
    Ok(())
}

fn helper_refusal(result: &Result<(), PinnedSelectedEntryError>) -> bool {
    matches!(
        result,
        Err(PinnedSelectedEntryError::PushProfile(
            PinnedPushProfileError::Helper(_) | PinnedPushProfileError::HelperBounds
        ))
    )
}

async fn active_runtime_transaction(
    admin: &PgPool,
    runtime: &str,
    pid: i32,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
         WHERE pid=$1 AND usename=$2 AND xact_start IS NOT NULL)",
    )
    .bind(pid)
    .bind(runtime)
    .fetch_one(admin)
    .await?)
}

async fn relation_lock_held(
    admin: &PgPool,
    pid: i32,
    oid: u32,
    mode: &str,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid=$1 AND locktype='relation' AND relation=$2 \
           AND mode=$3 AND granted)",
    )
    .bind(pid)
    .bind(Oid(oid))
    .bind(mode)
    .fetch_one(admin)
    .await?)
}

pub(super) async fn exercise(
    admin: &PgPool,
    fixture: &InventoryFixture,
    roles: (&str, &str, &str, &str),
    pubsub: &RestrictedServingProfileV1,
) -> anyhow::Result<()> {
    let (runtime, api, _helper, _migrator) = roles;
    let expected_names = [
        "push_registrations",
        "notification_activity",
        "notification_activity_owner_state",
        "notification_candidates",
        "notification_outbox",
        "notification_outbox_lineage",
        "notification_outbox_owner_state",
        "push_service_schema_marker",
        "push_owner_locks",
        "push_nodes",
        "push_node_locks",
        "push_devices",
        "push_delivery_attempts",
        "push_publish_jobs",
        "push_publication_orders",
        "push_service_vapid_keys",
        "push_service_vapid_schema_marker",
    ];
    ensure!(
        PinnedPushProfilePinsV1::push_relation_names() == expected_names,
        "pinned Push profile relation order changed"
    );

    let pins = profile_pins(admin, roles, pubsub).await?;
    let selected = Arc::new(fixture.selected()?);
    let had_monitor_select: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
         CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) acl \
         JOIN pg_catalog.pg_roles monitor ON monitor.rolname='pg_monitor' \
         WHERE c.oid=$1 AND acl.grantee=monitor.oid AND acl.privilege_type='SELECT')",
    )
    .bind(Oid(pins.pubsub.helper_relation_oids[6]))
    .fetch_one(admin)
    .await?;
    if had_monitor_select {
        sqlx::query("REVOKE SELECT ON guarded.pubsub_nodes FROM pg_monitor")
            .execute(admin)
            .await?;
    }
    selected
        .inspect_push_profile_v1(&pins)
        .await
        .context("positive pinned Push profile")?;

    // Pause after the H-owned helper and catalog checks. The only heap locks
    // are the 12 fixed PubSub barriers; Push and Foundation heaps remain
    // unread and unlocked. R still holds the shared catalog fence until the
    // selected transaction acknowledges rollback.
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let running = {
        let selected = Arc::clone(&selected);
        let pins = pins.clone();
        tokio::spawn(async move {
            selected
                .inspect_push_profile_v1_paused_for_test(&pins, ready, resume)
                .await
        })
    };
    let pid = tokio::time::timeout(Duration::from_secs(15), observed).await??;
    ensure!(
        active_runtime_transaction(admin, runtime, pid).await?,
        "paused profile is not live on its pinned R backend"
    );
    for oid in pins.pubsub.helper_relation_oids {
        ensure!(
            relation_lock_held(admin, pid, oid, "ShareUpdateExclusiveLock").await?,
            "H helper did not hold each fixed PubSub barrier"
        );
    }
    let foundation_oid = relation_oid(admin, "ingress_effect_descendants").await?;
    for oid in pins.push_relation_oids.into_iter().chain([foundation_oid]) {
        ensure!(
            !relation_lock_held(admin, pid, oid, "AccessShareLock").await?,
            "pinned profile read a Push or Foundation heap"
        );
    }
    let exclusive_catalog: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(admin)
            .await?;
    ensure!(
        !exclusive_catalog,
        "catalog mutator passed the pinned profile's shared fence"
    );
    release
        .send(())
        .map_err(|_| anyhow::anyhow!("release pinned Push profile"))?;
    tokio::time::timeout(Duration::from_secs(10), running)
        .await??
        .context("paused pinned Push profile")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle: bool = sqlx::query_scalar(
                "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
            )
            .bind(pid)
            .fetch_optional(admin)
            .await?
            .unwrap_or(true);
            if idle {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    // The runtime can never lock or read the Foundation descendant heap. The
    // combined fixture separately asserts its exact PostgreSQL 42501 refusal.
    let mut foundation = admin.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {runtime}"))
        .execute(&mut *foundation)
        .await?;
    let current_user: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *foundation)
        .await?;
    ensure!(current_user == runtime, "Foundation denial did not use R");
    let denied = sqlx::query("LOCK TABLE guarded.ingress_effect_descendants IN ACCESS SHARE MODE")
        .execute(&mut *foundation)
        .await
        .expect_err("R unexpectedly acquired a Foundation read lock");
    let sqlstate = denied
        .as_database_error()
        .and_then(|error| error.code())
        .map(|code| code.into_owned());
    ensure!(
        sqlstate.as_deref() == Some("42501"),
        "Foundation denial used an unexpected SQLSTATE: {sqlstate:?}"
    );
    foundation.rollback().await?;

    let mut wrong_function = pins.clone();
    wrong_function.pubsub.helper_function_oid = u32::MAX;
    let function_pin_drift = selected.inspect_push_profile_v1(&wrong_function).await;
    ensure!(
        matches!(
            function_pin_drift,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::HelperBounds
                    | PinnedPushProfileError::Helper(_)
                    | PinnedPushProfileError::InvalidPins
            ))
        ),
        "wrong helper OID was accepted: {function_pin_drift:?}"
    );

    sqlx::query("REVOKE EXECUTE ON FUNCTION guarded.pubsub_runtime_attach_lock() FROM PUBLIC")
        .execute(admin)
        .await?;
    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION guarded.pubsub_runtime_attach_lock() FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    let missing_helper_acl = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION guarded.pubsub_runtime_attach_lock() TO {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        helper_refusal(&missing_helper_acl),
        "missing R helper EXECUTE was accepted: {missing_helper_acl:?}"
    );

    sqlx::query(&format!("ALTER ROLE {api} LOGIN"))
        .execute(admin)
        .await?;
    let role_drift = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query(&format!("ALTER ROLE {api} NOLOGIN"))
        .execute(admin)
        .await?;
    ensure!(
        matches!(
            role_drift,
            Err(PinnedSelectedEntryError::PartialProfile(
                PinnedPartialProfileError::RoleDrift
            ))
        ),
        "A role login drift was accepted: {role_drift:?}"
    );

    let mut wrong_relation = pins.clone();
    wrong_relation.push_relation_oids[3] = u32::MAX;
    let stale_relation = selected.inspect_push_profile_v1(&wrong_relation).await;
    ensure!(
        matches!(
            &stale_relation,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::RelationDrift {
                    relation: "notification_candidates"
                }
            ))
        ),
        "stale Push relation OID returned an unexpected result: {stale_relation:?}"
    );

    sqlx::query(&format!(
        "GRANT TRUNCATE ON guarded.notification_candidates TO {runtime}"
    ))
    .execute(admin)
    .await?;
    let excess_acl = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query(&format!(
        "REVOKE TRUNCATE ON guarded.notification_candidates FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        matches!(
            excess_acl,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::RelationDrift {
                    relation: "notification_candidates"
                }
            ))
        ),
        "excess Push table ACL was accepted: {excess_acl:?}"
    );

    sqlx::query(&format!(
        "REVOKE SELECT ON guarded.push_registrations FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    let missing_push_acl = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query(&format!(
        "GRANT SELECT ON guarded.push_registrations TO {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        matches!(
            missing_push_acl,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::RelationDrift {
                    relation: "push_registrations"
                }
            ))
        ),
        "missing R Push heap SELECT was accepted: {missing_push_acl:?}"
    );

    sqlx::query("GRANT SELECT ON guarded.pubsub_nodes TO pg_monitor")
        .execute(admin)
        .await?;
    let monitor_select = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query("REVOKE SELECT ON guarded.pubsub_nodes FROM pg_monitor")
        .execute(admin)
        .await?;
    ensure!(
        matches!(
            monitor_select,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::RelationDrift {
                    relation: "pubsub_nodes"
                }
            ))
        ),
        "pg_monitor SELECT on a pinned PubSub heap was accepted: {monitor_select:?}"
    );

    // The selected Push entry tries this gate and rolls back on contention
    // instead of waiting with its earlier catalog barriers held.
    let mut gate = admin.begin().await?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(PINNED_PUSH_PUBSUB_GATE_KEY)
        .execute(&mut *gate)
        .await?;
    let gate_contention = selected.inspect_push_profile_v1(&pins).await;
    gate.rollback().await?;
    ensure!(
        matches!(
            gate_contention,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::GateContended
            ))
        ),
        "contended pinned Push gate did not refuse: {gate_contention:?}"
    );

    // The H helper's NOWAIT lock is the fail-closed path for concurrent
    // PubSub DDL. Its refusal must release the selected transaction before
    // the blocker is removed.
    let mut blocker = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded.pubsub_nodes IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let helper_contention = selected.inspect_push_profile_v1(&pins).await;
    blocker.rollback().await?;
    ensure!(
        helper_refusal(&helper_contention),
        "H helper contention did not refuse safely: {helper_contention:?}"
    );
    selected
        .inspect_push_profile_v1(&pins)
        .await
        .context("pinned Push profile after helper contention")?;

    // Cancellation after H has acquired its barriers closes the one-slot
    // session. Wait for PostgreSQL to remove that exact backend before
    // considering the helper locks released.
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (_release, resume) = tokio::sync::oneshot::channel();
    let running = {
        let selected = Arc::clone(&selected);
        let pins = pins.clone();
        tokio::spawn(async move {
            selected
                .inspect_push_profile_v1_paused_for_test(&pins, ready, resume)
                .await
        })
    };
    let cancelled_pid = tokio::time::timeout(Duration::from_secs(15), observed).await??;
    running.abort();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(cancelled_pid)
            .fetch_one(admin)
            .await?;
            if !present {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    // Limit+1 preflight must refuse a large unrelated application catalog
    // before expanding ACLs or invoking the H helper. Keep overflow objects
    // in a disposable schema and remove them immediately after the check.
    let overflow_schema = format!("push_profile_overflow_{}", uuid::Uuid::new_v4().simple());
    let create_overflow = format!(
        "DO $push_profile_overflow$ DECLARE relation_number integer; BEGIN \
         EXECUTE pg_catalog.format('CREATE SCHEMA %I', '{overflow_schema}'); \
         FOR relation_number IN 1..1025 LOOP \
           EXECUTE pg_catalog.format('CREATE TABLE %I.%I (id integer)', \
             '{overflow_schema}', 'relation_' || relation_number); \
         END LOOP; END $push_profile_overflow$"
    );
    sqlx::query(&create_overflow).execute(admin).await?;
    let catalog_overflow = selected.inspect_push_profile_v1(&pins).await;
    sqlx::query(&format!("DROP SCHEMA {overflow_schema} CASCADE"))
        .execute(admin)
        .await?;
    ensure!(
        matches!(
            catalog_overflow,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::GlobalCatalogBounds
            ))
        ),
        "oversized unrelated relation inventory was accepted: {catalog_overflow:?}"
    );

    // Recheck the baseline before the final pg_proc witnesses. ALTER FUNCTION
    // can compress prosrc while changing proconfig, so the bounded short body
    // distinguishes the config limit from a source-compression refusal.
    selected
        .inspect_push_profile_v1(&pins)
        .await
        .context("pinned Push profile before final helper proconfig drift")?;

    let original_body = helper_body()?;
    let temporary_body = "SELECT NULL::void";
    replace_helper_body(admin, temporary_body).await?;
    let (temporary_oid, temporary_source, source_size, source_compressed, baseline_config): (
        Oid,
        String,
        i32,
        bool,
        bool,
    ) = sqlx::query_as(
        "SELECT p.oid, p.prosrc, pg_catalog.pg_column_size(p.prosrc), \
         pg_catalog.pg_column_compression(p.prosrc) IS NOT NULL, \
         p.proconfig=ARRAY['search_path=pg_catalog, pg_temp']::text[] \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid='guarded.pubsub_runtime_attach_lock()'::pg_catalog.regprocedure",
    )
    .fetch_one(admin)
    .await?;
    ensure!(
        temporary_oid.0 == pins.pubsub.helper_function_oid
            && temporary_source == temporary_body
            && source_size <= 8192
            && !source_compressed
            && baseline_config,
        "temporary helper body did not remain short, uncompressed, and pinned"
    );
    let body_drift = selected.inspect_push_profile_v1(&pins).await;
    ensure!(
        matches!(
            body_drift,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::Helper(_)
            ))
        ),
        "unpadded temporary helper body had an unexpected refusal: {body_drift:?}"
    );

    let oversized_search_path = (0..140)
        .map(|index| format!("p21_profile_search_path_{index:03}_{}", "x".repeat(40)))
        .collect::<Vec<_>>()
        .join(", ");
    ensure!(
        oversized_search_path.len() > 4096,
        "oversized helper search_path witness is under the catalog bound"
    );
    sqlx::query(&format!(
        "ALTER FUNCTION guarded.pubsub_runtime_attach_lock() \
         SET search_path TO {oversized_search_path}"
    ))
    .execute(admin)
    .await?;
    let (oversized_config, bounded_uncompressed_source, oversized_oid, oversized_source): (
        bool,
        bool,
        Oid,
        String,
    ) = sqlx::query_as(
        "SELECT pg_catalog.pg_column_size(p.proconfig) > 4096 \
             OR pg_catalog.pg_column_compression(p.proconfig) IS NOT NULL, \
         pg_catalog.pg_column_size(p.prosrc) <= 8192 \
             AND pg_catalog.pg_column_compression(p.prosrc) IS NULL, \
         p.oid, p.prosrc \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid='guarded.pubsub_runtime_attach_lock()'::pg_catalog.regprocedure",
    )
    .fetch_one(admin)
    .await?;
    ensure!(
        oversized_config
            && bounded_uncompressed_source
            && oversized_oid.0 == pins.pubsub.helper_function_oid
            && oversized_source == temporary_body,
        "proconfig witness also changed the pinned function or compressed its short body"
    );
    let config_drift = selected.inspect_push_profile_v1(&pins).await;
    ensure!(
        matches!(
            config_drift,
            Err(PinnedSelectedEntryError::PushProfile(
                PinnedPushProfileError::HelperBounds
            ))
        ),
        "oversized helper proconfig did not return its exact bounded refusal: {config_drift:?}"
    );

    sqlx::query(
        "ALTER FUNCTION guarded.pubsub_runtime_attach_lock() \
         SET search_path = pg_catalog, pg_temp",
    )
    .execute(admin)
    .await?;
    replace_helper_body(admin, &original_body).await?;
    let (restored_oid, restored_body, proconfig_restored): (Oid, String, bool) = sqlx::query_as(
        "SELECT p.oid, p.prosrc, \
             p.proconfig=ARRAY['search_path=pg_catalog, pg_temp']::text[] \
             FROM pg_catalog.pg_proc p \
             WHERE p.oid='guarded.pubsub_runtime_attach_lock()'::pg_catalog.regprocedure",
    )
    .fetch_one(admin)
    .await?;
    ensure!(
        restored_oid.0 == pins.pubsub.helper_function_oid
            && restored_body == original_body
            && proconfig_restored,
        "helper body or search_path was not restored to the installer values"
    );
    if had_monitor_select {
        sqlx::query("GRANT SELECT ON guarded.pubsub_nodes TO pg_monitor")
            .execute(admin)
            .await?;
    }
    Ok(())
}
