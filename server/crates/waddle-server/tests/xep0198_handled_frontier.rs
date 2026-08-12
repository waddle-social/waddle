//! Dedicated XEP-0198 suite for the durable handled-frontier semantics
//! introduced by the #1654 ingress unit-of-work seam (XEP custom
//! test-suite hard rule).
//!
//! The XEP-0198 inbound handled counter is a wrapping unsigned 32-bit value
//! ("in the case where the value of 'h' exceeds 2^32-1 ... reset to zero");
//! `sm_sessions.inbound_count` stores it full-width in a BIGINT but the
//! domain stays mod-2^32. This suite pins the fenced repository's contract
//! against that wire semantic: equality is idempotent, exactly one wrapping
//! step advances (including `u32::MAX -> 0`), anything else is a typed stale
//! rejection, and no advance happens without the exact SM ownership claim
//! fence minted under current node authority in the same transaction.
//!
//! Postgres-gated on `WADDLE_TEST_POSTGRES_URL` (skips cleanly otherwise);
//! isolated per-test schemas, so no shared-table serialization is needed.

#![cfg(feature = "clustering")]

use uuid::Uuid;
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::config::LineageConfig;
use waddle_server::db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_uow::{
    ClaimRepository, HandledFrontierOutcome, HandledFrontierRepository, IngressUowError,
    PostgresIngressUnitOfWork, SmClaimFence,
};
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;

struct Fixture {
    db: Database,
    uow: PostgresIngressUnitOfWork,
    node_identity: SharedNodeIdentity,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    admin: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn open(test_name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (xep0198 handled frontier)");
            return None;
        };
        let schema = format!(
            "waddle_test_x0198_frontier_{test_name}_{}",
            Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");
        let schema_url = url_with_search_path(&database_url, &schema);
        let config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
        let db = Database::from_config("xep0198-frontier-test", &config)
            .await
            .expect("open isolated postgres database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply migrations to isolated schema");
        PostgresClaimStore::new(db.clone())
            .ensure_schema()
            .await
            .expect("initialize claims schema");
        DatabaseSmPersistence::open(Some(&schema_url))
            .await
            .expect("initialize SM persistence schema");
        let lineage_config = LineageConfig {
            deployment_uuid: Some(
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a90198"
                    .parse()
                    .expect("valid fixture deployment UUID"),
            ),
            action: None,
        };
        lineage::enroll(&db, &lineage_config)
            .await
            .expect("enroll fixture lineage");
        let uow = PostgresIngressUnitOfWork::open(db.clone(), lineage_config)
            .expect("open postgres ingress unit of work");
        let owner = NodeIdentity::new("node-0198", "node-0198-epoch");
        Some(Self {
            db,
            uow,
            node_identity: SharedNodeIdentity::new(owner.clone()),
            owner,
            claim_epoch: ClaimEpoch(198),
            admin,
            schema,
        })
    }

    async fn seed_claim_and_session(&self, stream_id: &SmSessionId, inbound_count: u32) {
        let conn = self.db.guard().await.expect("database guard");
        conn.execute(
            "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
            waddle_server::db_params![
                format!(
                    "{}:{}",
                    EntityType::SmSession.as_db_str(),
                    stream_id.as_str()
                ),
                EntityType::SmSession.as_db_str().to_string(),
                self.owner.node_id.clone(),
                self.owner.node_epoch.clone(),
                self.claim_epoch.0,
            ],
        )
        .await
        .expect("seed exact SM claim");
        conn.execute(
            r#"INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count, last_acked,
                detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested,
                blocklist_interested, presence_available, presence_priority, bare_jid,
                auth_context_id, auth_context_version, principal_auth_epoch
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            waddle_server::db_params![
                stream_id.as_str().to_string(),
                "romeo".to_string(),
                "romeo@example.com/phone".to_string(),
                i64::from(inbound_count),
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                "romeo@example.com".to_string(),
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a91980".to_string(),
                1_i64,
                1_i64,
            ],
        )
        .await
        .expect("seed SM session");
    }

    async fn fence<'transaction>(
        &self,
        transaction: &mut waddle_server::ingress_uow::IngressUowTransaction<'transaction>,
        stream_id: &SmSessionId,
    ) -> SmClaimFence<'transaction> {
        ClaimRepository::assert_sm_claim(
            transaction,
            &self.node_identity,
            stream_id,
            &self.owner,
            self.claim_epoch,
        )
        .await
        .expect("mint claim fence under current node authority")
    }

    async fn stored_frontier(&self, stream_id: &SmSessionId) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(
                "SELECT inbound_count FROM sm_sessions WHERE stream_id = ?",
                waddle_server::db_params![stream_id.as_str().to_string()],
            )
            .await
            .expect("read stored frontier");
        rows.next()
            .await
            .expect("read frontier row")
            .expect("frontier row exists")
            .get(0)
            .expect("decode frontier")
    }

    async fn close(self) {
        let Self {
            db,
            uow,
            admin,
            schema,
            ..
        } = self;
        drop(uow);
        drop(db);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated postgres schema");
    }
}

fn url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

/// The handled counter is mod-2^32: equality is idempotent, one wrapping
/// step advances (including `u32::MAX -> 0`), and any other offer is a
/// typed stale rejection carrying the exact stored/offered pair.
#[tokio::test]
async fn handled_frontier_is_a_wrapping_mod_2_32_counter() {
    let Some(fixture) = Fixture::open("wrap").await else {
        return;
    };
    let stream_id = SmSessionId::new("x0198-wrap-stream");
    fixture.seed_claim_and_session(&stream_id, u32::MAX).await;

    let mut transaction = fixture.uow.begin().await.expect("begin unit of work");
    let fence = fixture.fence(&mut transaction, &stream_id).await;
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &stream_id, u32::MAX)
            .await
            .expect("equal handled value is idempotent"),
        HandledFrontierOutcome::Idempotent
    );
    assert_eq!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &stream_id, 0)
            .await
            .expect("u32::MAX wraps to zero"),
        HandledFrontierOutcome::Advanced
    );
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &stream_id, 2).await,
        Err(IngressUowError::FrontierStale {
            stored: 0,
            offered: 2
        })
    ));
    transaction.commit().await.expect("commit wrapped frontier");

    assert_eq!(
        fixture.stored_frontier(&stream_id).await,
        0,
        "the wrapped counter commits as zero, not 2^32"
    );
    fixture.close().await;
}

/// No advance without the exact fenced claim in the same transaction: a
/// fence for another stream is rejected before any row is touched, and a
/// missing session is a typed miss rather than an implicit create.
#[tokio::test]
async fn handled_frontier_requires_exact_fence_and_live_session() {
    let Some(fixture) = Fixture::open("fence").await else {
        return;
    };
    let stream_id = SmSessionId::new("x0198-fence-stream");
    fixture.seed_claim_and_session(&stream_id, 3).await;
    let other_stream = SmSessionId::new("x0198-other-stream");

    let mut transaction = fixture.uow.begin().await.expect("begin unit of work");
    let fence = fixture.fence(&mut transaction, &stream_id).await;
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &fence, &other_stream, 4).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    let ghost = SmSessionId::new("x0198-ghost-stream");
    fixture.seed_claim_and_session(&ghost, 0).await;
    let ghost_fence = fixture.fence(&mut transaction, &ghost).await;
    let conn_deleted = {
        let conn = fixture.db.guard().await.expect("database guard");
        conn.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            waddle_server::db_params![ghost.as_str().to_string()],
        )
        .await
        .expect("delete ghost session")
    };
    assert_eq!(conn_deleted, 1);
    assert!(matches!(
        HandledFrontierRepository::advance(&mut transaction, &ghost_fence, &ghost, 1).await,
        Err(IngressUowError::StreamMissing)
    ));
    drop(transaction);

    assert_eq!(
        fixture.stored_frontier(&stream_id).await,
        3,
        "no fenced write leaked outside the dropped transaction"
    );
    fixture.close().await;
}
