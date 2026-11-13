//! Same-backend partial-profile witnesses on the combined real PG17 fixture.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use sqlx::postgres::types::Oid;
use sqlx::postgres::PgPool;
use waddle_server::db::physical_attestation::{
    DarkPhysicalAttestationError, PinnedPartialProfileError, PinnedPartialProfilePinsV1,
    PinnedSelectedEntry, PinnedSelectedEntryError,
};
use waddle_server::db::DarkLedgerPreflightError;
use waddle_server::ingress_substrate::INGRESS_GUARDED_TABLES;
use waddle_server::pubsub::DatabasePubSubStorage;

use super::pinned_pubsub_inventory::InventoryFixture;

async fn role_oid(admin: &PgPool, name: &str) -> anyhow::Result<u32> {
    let oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1")
        .bind(name)
        .fetch_one(admin)
        .await?;
    Ok(oid.0)
}

async fn relation_oid(admin: &PgPool, name: &str) -> anyhow::Result<u32> {
    let oid: Oid = sqlx::query_scalar("SELECT c.oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='guarded' AND c.relname=$1")
        .bind(name)
        .fetch_one(admin)
        .await?;
    Ok(oid.0)
}

pub(super) async fn pins(
    admin: &PgPool,
    api: &str,
    helper: &str,
    migrator: &str,
) -> anyhow::Result<PinnedPartialProfilePinsV1> {
    let mut ingress = [0; 19];
    for (slot, name) in ingress
        .iter_mut()
        .zip(PinnedPartialProfilePinsV1::ingress_relation_names())
    {
        *slot = relation_oid(admin, name).await?;
    }
    let mut pubsub = [0; 12];
    for (slot, name) in pubsub
        .iter_mut()
        .zip(PinnedPartialProfilePinsV1::pubsub_relation_names())
    {
        *slot = relation_oid(admin, name).await?;
    }
    Ok(PinnedPartialProfilePinsV1 {
        api_role_oid: role_oid(admin, api).await?,
        helper_role_oid: role_oid(admin, helper).await?,
        monitor_role_oid: role_oid(admin, "pg_monitor").await?,
        migrator_role_oid: role_oid(admin, migrator).await?,
        ingress_relation_oids: ingress,
        pubsub_relation_oids: pubsub,
    })
}

async fn refused(
    selected: &PinnedSelectedEntry,
    pins: &PinnedPartialProfilePinsV1,
    reason: &str,
) -> anyhow::Result<()> {
    let result = selected.inspect_partial_profile_v1(pins).await;
    ensure!(
        matches!(
            result,
            Err(PinnedSelectedEntryError::PartialProfile(
                PinnedPartialProfileError::RoleDrift
                    | PinnedPartialProfileError::RelationDrift { .. }
            ))
        ),
        "partial profile did not reject {reason} at its catalog check: {result:?}"
    );
    Ok(())
}

async fn waiting_on(admin: &PgPool, oid: u32) -> anyhow::Result<i32> {
    Ok(tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_locks WHERE locktype='relation' \
                 AND relation=$1 AND mode='AccessShareLock' AND NOT granted LIMIT 1",
            )
            .bind(Oid(oid))
            .fetch_optional(admin)
            .await?;
            if let Some(pid) = pid {
                return Ok::<_, sqlx::Error>(pid);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??)
}

pub(super) async fn exercise(
    admin: &PgPool,
    fixture: &InventoryFixture,
    roles: (&str, &str, &str, &str, &str),
) -> anyhow::Result<()> {
    let (runtime, api, helper, migrator, owner) = roles;
    ensure!(
        PinnedPartialProfilePinsV1::ingress_relation_names() == &INGRESS_GUARDED_TABLES,
        "V1 ingress names differ from the fixed guarded list"
    );
    ensure!(
        PinnedPartialProfilePinsV1::pubsub_relation_names().as_slice()
            == DatabasePubSubStorage::runtime_attach_relation_names_for_test().as_slice(),
        "V1 PubSub names differ from the fixed attachment list"
    );
    // The trusted fixture administrator records these before running R's
    // diagnostic. The inspected runtime connection is not the pin source.
    let pins = pins(admin, api, helper, migrator).await?;
    let selected = Arc::new(fixture.selected()?);
    selected.inspect_partial_profile_v1(&pins).await?;

    // M SELECT is permitted but not required. Exercise both states while
    // restoring the fixture's original grant for the remaining tests.
    let had_monitor_select: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
         CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) acl \
         WHERE c.oid=$1 AND acl.grantee=$2 AND acl.privilege_type='SELECT')",
    )
    .bind(Oid(pins.pubsub_relation_oids[6]))
    .bind(Oid(pins.monitor_role_oid))
    .fetch_one(admin)
    .await?;
    sqlx::query("REVOKE SELECT ON guarded.pubsub_nodes FROM pg_monitor")
        .execute(admin)
        .await?;
    selected.inspect_partial_profile_v1(&pins).await?;
    sqlx::query("GRANT SELECT ON guarded.pubsub_nodes TO pg_monitor")
        .execute(admin)
        .await?;
    selected.inspect_partial_profile_v1(&pins).await?;
    if !had_monitor_select {
        sqlx::query("REVOKE SELECT ON guarded.pubsub_nodes FROM pg_monitor")
            .execute(admin)
            .await?;
    }

    let (ready, observed) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let running = {
        let selected = Arc::clone(&selected);
        tokio::spawn(async move {
            selected
                .inspect_partial_profile_v1_paused_for_test(&pins, ready, resume)
                .await
        })
    };
    let pid = tokio::time::timeout(Duration::from_secs(15), observed).await??;
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
         WHERE pid=$1 AND usename=$2 AND xact_start IS NOT NULL)",
    )
    .bind(pid)
    .bind(runtime)
    .fetch_one(admin)
    .await?;
    ensure!(
        live,
        "profile transaction not live on its reported R backend"
    );
    for name in [
        "_lineage",
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
    ] {
        let oid = relation_oid(admin, name).await?;
        let held: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
             AND locktype='relation' AND relation=$2 AND mode='AccessShareLock' AND granted)",
        )
        .bind(pid)
        .bind(Oid(oid))
        .fetch_one(admin)
        .await?;
        ensure!(held, "missing {name} barrier on the reported backend");
    }
    for oid in pins.ingress_relation_oids {
        let locked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
             AND locktype='relation' AND relation=$2 AND granted)",
        )
        .bind(pid)
        .bind(Oid(oid))
        .fetch_one(admin)
        .await?;
        ensure!(!locked, "R unexpectedly locked a guarded ingress heap");
    }
    release
        .send(())
        .map_err(|_| anyhow::anyhow!("release profile"))?;
    tokio::time::timeout(Duration::from_secs(8), running)
        .await??
        .context("partial profile")?;
    let idle: bool = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_one(admin)
    .await?;
    ensure!(
        idle,
        "success did not acknowledge rollback on the same backend"
    );

    let mut wrong = pins;
    wrong.api_role_oid = u32::MAX;
    refused(&selected, &wrong, "wrong API role pin").await?;
    wrong = pins;
    wrong.monitor_role_oid = u32::MAX;
    refused(&selected, &wrong, "wrong monitor role pin").await?;
    wrong = pins;
    wrong.ingress_relation_oids[0] = u32::MAX;
    refused(&selected, &wrong, "wrong ingress heap pin").await?;
    wrong = pins;
    wrong.pubsub_relation_oids.swap(0, 1);
    refused(&selected, &wrong, "swapped PubSub heap pins").await?;
    wrong = pins;
    wrong.helper_role_oid = wrong.api_role_oid;
    ensure!(
        matches!(
            selected.inspect_partial_profile_v1(&wrong).await,
            Err(PinnedSelectedEntryError::PartialProfile(
                PinnedPartialProfileError::InvalidPins
            ))
        ),
        "duplicate principal pins were not rejected before checkout"
    );

    for (change, restore) in [
        (
            format!("GRANT pg_read_all_data TO {api}"),
            format!("REVOKE pg_read_all_data FROM {api}"),
        ),
        (
            format!("ALTER ROLE {helper} INHERIT"),
            format!("ALTER ROLE {helper} NOINHERIT"),
        ),
        (
            format!("GRANT SELECT ON guarded.ingress_messages TO {runtime}"),
            format!("REVOKE SELECT ON guarded.ingress_messages FROM {runtime}"),
        ),
        (
            "GRANT SELECT ON guarded.ingress_messages TO PUBLIC".to_owned(),
            "REVOKE SELECT ON guarded.ingress_messages FROM PUBLIC".to_owned(),
        ),
        (
            "GRANT UPDATE ON guarded.ingress_messages TO PUBLIC".to_owned(),
            "REVOKE UPDATE ON guarded.ingress_messages FROM PUBLIC".to_owned(),
        ),
        (
            "GRANT SELECT (message_key) ON guarded.ingress_messages TO PUBLIC".to_owned(),
            "REVOKE SELECT (message_key) ON guarded.ingress_messages FROM PUBLIC".to_owned(),
        ),
        (
            "GRANT SELECT ON guarded.pubsub_nodes TO PUBLIC".to_owned(),
            "REVOKE SELECT ON guarded.pubsub_nodes FROM PUBLIC".to_owned(),
        ),
        (
            "GRANT UPDATE ON guarded.pubsub_nodes TO pg_monitor".to_owned(),
            "REVOKE UPDATE ON guarded.pubsub_nodes FROM pg_monitor".to_owned(),
        ),
        (
            format!("GRANT UPDATE ON guarded.pubsub_nodes TO {helper} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR UPDATE ON guarded.pubsub_nodes FROM {helper}"),
        ),
        (
            format!("GRANT UPDATE (epoch) ON guarded.ingress_protocol_epoch TO {api}"),
            format!("REVOKE UPDATE (epoch) ON guarded.ingress_protocol_epoch FROM {api}"),
        ),
        (
            "ALTER TABLE guarded.ingress_messages ENABLE ROW LEVEL SECURITY".to_owned(),
            "ALTER TABLE guarded.ingress_messages DISABLE ROW LEVEL SECURITY".to_owned(),
        ),
    ] {
        sqlx::query(&change).execute(admin).await?;
        refused(&selected, &pins, &change).await?;
        sqlx::query(&restore).execute(admin).await?;
    }
    selected.inspect_partial_profile_v1(&pins).await?;

    // Replace the name while keeping the original OID alive. The independent
    // pin must fail before any store table is accessed by name.
    sqlx::query("ALTER TABLE guarded.pubsub_nodes RENAME TO partial_profile_old_nodes")
        .execute(admin)
        .await?;
    let columns = (0..65)
        .map(|index| format!("column_{index} integer"))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(&format!("CREATE TABLE guarded.pubsub_nodes ({columns})"))
        .execute(admin)
        .await?;
    refused(&selected, &pins, "replaced PubSub relation name").await?;
    sqlx::query(&format!(
        "ALTER TABLE guarded.pubsub_nodes OWNER TO {owner}"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.pubsub_nodes TO {runtime}"
    ))
    .execute(admin)
    .await?;
    sqlx::query(&format!("GRANT UPDATE ON guarded.pubsub_nodes TO {helper}"))
        .execute(admin)
        .await?;
    let mut oversized = pins;
    oversized.pubsub_relation_oids[6] = relation_oid(admin, "pubsub_nodes").await?;
    ensure!(
        matches!(
            selected.inspect_partial_profile_v1(&oversized).await,
            Err(PinnedSelectedEntryError::PartialProfile(
                PinnedPartialProfileError::RelationDrift {
                    relation: "pubsub_nodes"
                }
            ))
        ),
        "oversized attribute inventory was not rejected"
    );
    sqlx::query("DROP TABLE guarded.pubsub_nodes")
        .execute(admin)
        .await?;
    sqlx::query("ALTER TABLE guarded.partial_profile_old_nodes RENAME TO pubsub_nodes")
        .execute(admin)
        .await?;

    // Pause only after the first joint preflight on R's selected transaction,
    // then force a lineage relation wait and commit hostile ACL drift while
    // it waits. The same backend must recheck before reading lineage.
    let lineage_oid = relation_oid(admin, "_lineage").await?;
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let waiter = {
        let selected = Arc::clone(&selected);
        tokio::spawn(async move {
            selected
                .inspect_partial_profile_v1_before_attestor_paused_for_test(&pins, ready, resume)
                .await
        })
    };
    let pid = tokio::time::timeout(Duration::from_secs(15), observed).await??;
    let mut blocker = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded._lineage IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    release
        .send(())
        .map_err(|_| anyhow::anyhow!("release before-attestor pause"))?;
    let waiting_pid = waiting_on(admin, lineage_oid).await?;
    ensure!(waiting_pid == pid, "lineage waiter changed backend");
    sqlx::query(&format!(
        "GRANT SELECT ON guarded.ingress_messages TO {runtime}"
    ))
    .execute(&mut *blocker)
    .await?;
    blocker.commit().await?;
    let result = tokio::time::timeout(Duration::from_secs(10), waiter).await??;
    ensure!(matches!(result, Err(PinnedSelectedEntryError::Attestation(
        DarkPhysicalAttestationError::PartialProfile(PinnedPartialProfileError::RelationDrift { relation: "ingress_messages" })
    ))), "lineage waiter {waiting_pid} did not reject the changed ACL before the lineage read: {result:?}");
    sqlx::query(&format!(
        "REVOKE SELECT ON guarded.ingress_messages FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    selected.inspect_partial_profile_v1(&pins).await?;

    // Violating the external quiescence assumption here is a hostile witness:
    // the newly committed ACL becomes visible after the ledger wait and must
    // be rejected by the fresh check before the protected ledger read.
    let ledger_oid = relation_oid(admin, "_migrations").await?;
    let mut blocker = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded._migrations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let waiter = {
        let selected = Arc::clone(&selected);
        tokio::spawn(async move { selected.inspect_partial_profile_v1(&pins).await })
    };
    let waiting_pid = waiting_on(admin, ledger_oid).await?;
    sqlx::query(&format!(
        "GRANT SELECT ON guarded.ingress_messages TO {runtime}"
    ))
    .execute(&mut *blocker)
    .await?;
    blocker.commit().await?;
    let result = tokio::time::timeout(Duration::from_secs(10), waiter).await??;
    ensure!(
        matches!(result, Err(PinnedSelectedEntryError::Ledger(
            DarkLedgerPreflightError::PartialProfile(PinnedPartialProfileError::RelationDrift { relation: "ingress_messages" })
        ))),
        "ledger waiter {waiting_pid} did not reject the changed ACL before the ledger read: {result:?}"
    );
    sqlx::query(&format!(
        "REVOKE SELECT ON guarded.ingress_messages FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    selected.inspect_partial_profile_v1(&pins).await?;

    let epoch_oid = relation_oid(admin, "ingress_protocol_epoch").await?;
    let mut blocker = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let waiter = {
        let selected = Arc::clone(&selected);
        tokio::spawn(async move { selected.inspect_partial_profile_v1(&pins).await })
    };
    let waiting_pid = waiting_on(admin, epoch_oid).await?;
    sqlx::query(&format!(
        "GRANT SELECT ON guarded.ingress_messages TO {runtime}"
    ))
    .execute(&mut *blocker)
    .await?;
    blocker.commit().await?;
    let result = tokio::time::timeout(Duration::from_secs(10), waiter).await??;
    ensure!(
        matches!(result, Err(PinnedSelectedEntryError::PartialProfile(
            PinnedPartialProfileError::RelationDrift { relation: "ingress_messages" }
        ))),
        "epoch waiter {waiting_pid} did not reject the changed ACL before the epoch read: {result:?}"
    );
    sqlx::query(&format!(
        "REVOKE SELECT ON guarded.ingress_messages FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    selected.inspect_partial_profile_v1(&pins).await?;

    // Cancellation does not itself establish rollback: observe this exact
    // backend disappear before asserting that its transaction is gone.
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (_release, resume) = tokio::sync::oneshot::channel();
    let running = {
        let selected = Arc::clone(&selected);
        tokio::spawn(async move {
            selected
                .inspect_partial_profile_v1_paused_for_test(&pins, ready, resume)
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
    Ok(())
}
