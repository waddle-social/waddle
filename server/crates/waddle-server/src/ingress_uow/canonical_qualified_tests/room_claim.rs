//! Room assertions must read and lock the attested claim, not a shadow.

use super::{count, with_database, with_schema, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use crate::ingress_uow::{
    CanonicalMessageRepository, ClaimRepository, IngressUnitOfWork, IngressUowError,
    IngressUowTransaction,
};
use futures::FutureExt;
use jid::BareJid;
use std::time::Duration;
use tokio::task::JoinSet;
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;

#[derive(Clone, Copy)]
enum Assertion {
    ClaimFence,
    NestedAcceptance,
}

fn entity(room: &BareJid) -> String {
    format!("{}:{room}", EntityType::RoomActor.as_db_str())
}

fn fence(room: &BareJid, owner: &NodeIdentity) -> RoomClaimFenceContext {
    RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room.to_string()),
        owner.clone(),
        ClaimEpoch(7),
    )
}

async fn assert_claim(
    assertion: Assertion,
    tx: &mut IngressUowTransaction<'_>,
    room: &BareJid,
    owner: &NodeIdentity,
) -> Result<(), IngressUowError> {
    match assertion {
        Assertion::ClaimFence => ClaimRepository::assert_room_claim(tx, room, owner, ClaimEpoch(7))
            .await
            .map(|_| ()),
        Assertion::NestedAcceptance => {
            CanonicalMessageRepository::protect_room_acceptance(tx, room, &fence(room, owner)).await
        }
    }
}

async fn raw_claim_exists(
    tx: &mut IngressUowTransaction<'_>,
    room: &BareJid,
    owner: &NodeIdentity,
) -> bool {
    let mut rows = tx.transaction_mut().query(
        "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
        crate::db_params![entity(room), owner.node_id.clone(), owner.node_epoch.clone(), 7_i64],
    ).await.expect("unqualified shadow control");
    rows.next().await.expect("shadow row").is_some()
}

#[tokio::test]
async fn attested_room_claims_ignore_hostile_shadows_and_lock_trusted_rows() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let result = std::panic::AssertUnwindSafe(postgres_case(&fixture, &url))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn postgres_case(fixture: &Fixture, url: &str) {
    let boundary = with_schema(&with_database(url, &fixture.database_name), &fixture.target);
    let database = Database::from_config(
        "room-claim-owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, boundary),
    )
    .await
    .expect("owner database");
    crate::clustering::claims::PostgresClaimStore::new(database.clone())
        .ensure_schema()
        .await
        .expect("claim owner schema");
    drop(database);
    for sql in [
        format!(
            "GRANT SELECT ON {}.clustering_claims TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE (entity_type) ON {}.clustering_claims TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("claim grants");
    }
    let owner = NodeIdentity::new("room-owner", "room-incarnation");
    let identity = SharedNodeIdentity::new(owner.clone());
    let valid: BareJid = "valid@conference.example.test".parse().expect("valid room");
    let stale: BareJid = "stale@conference.example.test".parse().expect("stale room");
    let missing: BareJid = "missing@conference.example.test"
        .parse()
        .expect("missing room");
    for (room, epoch) in [(&valid, 7_i64), (&stale, 8)] {
        sqlx::query(&format!("INSERT INTO {}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)", fixture.target))
            .bind(entity(room))
            .bind(EntityType::RoomActor.as_db_str())
            .bind(&owner.node_id)
            .bind(&owner.node_epoch)
            .bind(epoch)
            .execute(&fixture.admin).await.expect("trusted room claim");
    }
    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("room claims", attested, 1, 1)
        .await
        .expect("shared pool");
    let uow = IngressUnitOfWork::open_with_node_identity(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
        identity.clone(),
    )
    .expect("clustered UoW");

    for assertion in [Assertion::ClaimFence, Assertion::NestedAcceptance] {
        let mut tx = uow.begin().await.expect("prepared transaction");
        tx.set_local_timeouts(10_000, 15_000)
            .await
            .expect("bounded lock waits");
        assert_eq!(tx.receipt_schema(), Some(fixture.target.as_str()));
        fixture.grant_temp(true).await;
        tx.transaction_mut().execute(
            "CREATE TEMP TABLE clustering_claims (entity text PRIMARY KEY, node_id text NOT NULL, node_epoch text NOT NULL, claim_epoch bigint NOT NULL) ON COMMIT DROP",
            (),
        ).await.expect("create shadow claims");
        fixture.grant_temp(false).await;
        for (room, epoch) in [(&valid, 8_i64), (&stale, 7), (&missing, 7)] {
            tx.transaction_mut().execute(
                "INSERT INTO pg_temp.clustering_claims (entity, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?)",
                crate::db_params![entity(room), owner.node_id.clone(), owner.node_epoch.clone(), epoch],
            ).await.expect("seed shadow claim");
        }
        tx.transaction_mut()
            .execute(
                &format!("SET LOCAL search_path = pg_temp, {}", fixture.target),
                (),
            )
            .await
            .expect("hostile search path");
        let target_oid = count(
            &mut tx,
            &format!(
                "SELECT '{}.clustering_claims'::pg_catalog.regclass::oid::bigint",
                fixture.target
            ),
        )
        .await;
        let namespace_oid = count(&mut tx, &format!("SELECT relnamespace::bigint FROM pg_catalog.pg_class WHERE oid = '{}.clustering_claims'::pg_catalog.regclass", fixture.target)).await;
        assert_eq!(
            namespace_oid,
            i64::from(
                fixture
                    .lineage
                    .postgres_identity
                    .as_ref()
                    .expect("PostgreSQL identity")
                    .schema
                    .oid
            )
        );
        assert_ne!(
            target_oid,
            count(
                &mut tx,
                "SELECT 'clustering_claims'::pg_catalog.regclass::oid::bigint"
            )
            .await,
            "unqualified relation resolves to the hostile TEMP table"
        );

        for room in [&stale, &missing] {
            assert!(
                raw_claim_exists(&mut tx, room, &owner).await,
                "the shadow offers a matching claim"
            );
            assert!(
                matches!(
                    assert_claim(assertion, &mut tx, room, &owner).await,
                    Err(IngressUowError::ClaimFenceMissing)
                ),
                "a stale or absent trusted claim cannot be forged"
            );
        }
        assert!(
            !raw_claim_exists(&mut tx, &valid, &owner).await,
            "the shadow rejects the trusted claim"
        );
        assert_claim(assertion, &mut tx, &valid, &owner)
            .await
            .expect("trusted exact claim");
        if matches!(assertion, Assertion::NestedAcceptance) {
            tokio::time::timeout(
                Duration::from_secs(1),
                identity.with_publications_blocked(|| ()),
            )
            .await
            .expect("nested acceptance does not retain a second identity guard");
        }

        // A concurrent takeover must wait on the trusted row, not merely on
        // the same-named TEMP relation. Observe the actual blocking PID edge.
        let pid = count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
        let admin = fixture.admin.clone();
        let target = fixture.target.clone();
        let valid_entity = entity(&valid);
        let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut connection = admin.acquire().await.expect("takeover connection");
            let waiter: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut *connection)
                .await
                .expect("takeover PID");
            pid_sender.send(waiter).expect("send takeover PID");
            tokio::time::timeout(Duration::from_secs(15), async {
                sqlx::query(&format!(
                    "UPDATE {target}.clustering_claims SET claim_epoch = 8 WHERE entity = $1"
                ))
                .bind(valid_entity)
                .execute(&mut *connection)
                .await
            })
            .await
            .expect("bounded takeover")
            .expect("update trusted claim")
            .rows_affected()
        });
        let waiter = tokio::time::timeout(Duration::from_secs(5), pid_receiver)
            .await
            .expect("takeover starts")
            .expect("takeover PID sent");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar(
                    "SELECT $1::integer = ANY(pg_catalog.pg_blocking_pids($2::integer))",
                )
                .bind(i32::try_from(pid).expect("assertion PID"))
                .bind(waiter)
                .fetch_one(&fixture.admin)
                .await
                .expect("observe lock wait");
                if blocked {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("takeover waits for trusted claim lock");
        tx.commit().await.expect("release claim lock");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), tasks.join_next())
                .await
                .expect("takeover finishes")
                .expect("takeover task exists")
                .expect("takeover task succeeds"),
            1
        );
        sqlx::query(&format!(
            "UPDATE {}.clustering_claims SET claim_epoch = 7 WHERE entity = $1",
            fixture.target
        ))
        .bind(entity(&valid))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted claim");
    }

    // The retirement absence fence takes a table-level SHARE lock. Broaden
    // only this disposable runtime role immediately before the lock behavior
    // case; this does not establish the production role's grants.
    sqlx::query(&format!(
        "GRANT UPDATE ON {}.clustering_claims TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("test-only retirement table-lock grant");

    let retiring = SmSessionId::new("retirement-claim-fence");
    let retiring_entity = format!("sm_session:{}", retiring.as_str());
    sqlx::query(&format!(
        "INSERT INTO {}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)",
        fixture.target
    ))
    .bind(&retiring_entity)
    .bind(EntityType::SmSession.as_db_str())
    .bind(&owner.node_id)
    .bind(&owner.node_epoch)
    .bind(9_i64)
    .execute(&fixture.admin)
    .await
    .expect("trusted SM claim");

    let mut retirement_tx = uow.begin().await.expect("prepared retirement transaction");
    fixture.grant_temp(true).await;
    retirement_tx
        .transaction_mut()
        .execute(
            "CREATE TEMP TABLE clustering_claims (entity text PRIMARY KEY, entity_type text NOT NULL, node_id text NOT NULL, node_epoch text NOT NULL, claim_epoch bigint NOT NULL) ON COMMIT DROP",
            (),
        )
        .await
        .expect("create empty hostile claims shadow");
    fixture.grant_temp(false).await;
    retirement_tx
        .transaction_mut()
        .execute(
            &format!("SET LOCAL search_path = pg_temp, {}", fixture.target),
            (),
        )
        .await
        .expect("install hostile retirement search path");
    let temp_claims_oid = count(
        &mut retirement_tx,
        "SELECT 'pg_temp.clustering_claims'::pg_catalog.regclass::oid::bigint",
    )
    .await;
    let unqualified_claims_oid = count(
        &mut retirement_tx,
        "SELECT 'clustering_claims'::pg_catalog.regclass::oid::bigint",
    )
    .await;
    let trusted_claims_oid = count(
        &mut retirement_tx,
        &format!(
            "SELECT '{}.clustering_claims'::pg_catalog.regclass::oid::bigint",
            fixture.target
        ),
    )
    .await;
    assert_eq!(
        unqualified_claims_oid, temp_claims_oid,
        "the hostile search path makes bare clustering_claims resolve to TEMP"
    );
    assert_ne!(
        unqualified_claims_oid, trusted_claims_oid,
        "the shadow and canonical claims tables are distinct relations"
    );
    assert_eq!(
        retirement_tx.receipt_schema(),
        Some(fixture.target.as_str()),
        "the UoW retains the prepared target schema"
    );
    assert!(
        !crate::ingress_uow::SmIngressStreamRepository::fence_claim_absence_for_retirement(
            &mut retirement_tx,
            &retiring,
        )
        .await
        .expect("trusted claim absence check"),
        "the canonical SM claim must win over the empty TEMP shadow"
    );

    let holder = count(
        &mut retirement_tx,
        "SELECT pg_catalog.pg_backend_pid()::bigint",
    )
    .await;
    let admin = fixture.admin.clone();
    let target = fixture.target.clone();
    let writer_entity = retiring_entity.clone();
    let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut connection = admin.acquire().await.expect("retirement writer connection");
        let waiter: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .expect("retirement writer PID");
        pid_sender.send(waiter).expect("send retirement writer PID");
        tokio::time::timeout(Duration::from_secs(15), async {
            sqlx::query(&format!(
                "UPDATE {target}.clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = $1"
            ))
            .bind(writer_entity)
            .execute(&mut *connection)
            .await
        })
        .await
        .expect("bounded retirement writer")
        .expect("update canonical retirement claim")
        .rows_affected()
    });
    let waiter = tokio::time::timeout(Duration::from_secs(5), pid_receiver)
        .await
        .expect("retirement writer starts")
        .expect("retirement writer PID sent");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT $1::integer = ANY(pg_catalog.pg_blocking_pids($2::integer))",
            )
            .bind(i32::try_from(holder).expect("retirement holder PID"))
            .bind(waiter)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe retirement table-lock wait");
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("canonical claim writer waits for retirement SHARE lock");
    retirement_tx
        .commit()
        .await
        .expect("release retirement claim lock");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), tasks.join_next())
            .await
            .expect("retirement writer completes")
            .expect("retirement writer task exists")
            .expect("retirement writer succeeds"),
        1
    );
    drop(uow);
    drop(pool);
}
