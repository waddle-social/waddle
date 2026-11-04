//! Exact claim assertions must not deadlock with a queued identity transition.
#![cfg(feature = "clustering")]

use std::task::Poll;
use std::time::Duration;

use jid::BareJid;
use uuid::Uuid;
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::config::LineageConfig;
use waddle_server::db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_uow::{
    ClaimRepository, IngressUnitOfWork, IngressUowError, MamArchiveRepository,
    SmIngressStreamRepository,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;

const WAIT: Duration = Duration::from_secs(5);

struct Fixture {
    admin: sqlx::PgPool,
    db: Database,
    uow: IngressUnitOfWork,
    identity: SharedNodeIdentity,
    owner: NodeIdentity,
    schema: String,
}

impl Fixture {
    async fn open() -> Option<Self> {
        let database_url = waddle_ws_test_support::require_test_postgres_url()?;
        let schema = format!("waddle_reentrant_claim_{}", Uuid::new_v4().simple());
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let mut url = url::Url::parse(&database_url).expect("PostgreSQL URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained)
            .append_pair("options", &format!("-c search_path={schema}"));
        let db = Database::from_config(
            "reentrant-claim-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
        )
        .await
        .expect("fixture database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        PostgresClaimStore::new(db.clone())
            .ensure_schema()
            .await
            .expect("claims schema");
        let lineage = LineageConfig {
            deployment_uuid: Some(lineage::DeploymentUuid(Uuid::new_v4())),
            action: None,
        };
        lineage::enroll(&db, &lineage).await.expect("lineage");
        let owner = NodeIdentity::new("node-a", "incarnation-a");
        let identity = SharedNodeIdentity::new(owner.clone());
        let uow = IngressUnitOfWork::open_with_node_identity(db.clone(), lineage, identity.clone())
            .expect("clustered UOW");
        Some(Self {
            admin,
            db,
            uow,
            identity,
            owner,
            schema,
        })
    }

    async fn seed_claim(&self, entity_type: EntityType, id: &str, epoch: ClaimEpoch) {
        sqlx::query(&format!(
            "INSERT INTO {}.clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)",
            self.schema
        ))
        .bind(format!("{}:{id}", entity_type.as_db_str()))
        .bind(entity_type.as_db_str())
        .bind(&self.owner.node_id)
        .bind(&self.owner.node_epoch)
        .bind(epoch.0)
        .execute(&self.admin)
        .await
        .expect("seed claim");
    }

    async fn close(self) {
        drop(self.uow);
        drop(self.db);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop fixture schema");
        self.admin.close().await;
    }
}

async fn queued_transition(disable: bool) {
    let Some(fixture) = Fixture::open().await else {
        return;
    };
    let room: BareJid = "room@conference.example.com".parse().expect("room JID");
    let second_room: BareJid = "other@conference.example.com".parse().expect("room JID");
    let stream = SmSessionId::new("stream-a");
    let epoch = ClaimEpoch(11);
    fixture
        .seed_claim(EntityType::RoomActor, &room.to_string(), epoch)
        .await;
    fixture
        .seed_claim(EntityType::RoomActor, &second_room.to_string(), epoch)
        .await;
    fixture
        .seed_claim(EntityType::SmSession, stream.as_str(), epoch)
        .await;

    // A failed first claim must not leave a newly acquired node guard behind.
    let mut rejected = fixture.uow.begin().await.expect("begin rejected UOW");
    assert!(matches!(
        ClaimRepository::assert_room_claim(&mut rejected, &room, &fixture.owner, ClaimEpoch(12))
            .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    let released = tokio::time::timeout(WAIT, fixture.identity.rotate(fixture.owner.clone())).await;
    drop(rejected);
    released.expect("failed assertion must release its new authority");

    let mut tx = fixture.uow.begin().await.expect("begin UOW");
    let _first = ClaimRepository::assert_room_claim(&mut tx, &room, &fixture.owner, epoch)
        .await
        .expect("first exact room claim");
    let next_identity = fixture.identity.clone();
    let mut transition = Box::pin(async move {
        if disable {
            next_identity.disable().await;
        } else {
            next_identity
                .rotate(NodeIdentity::new("node-a", "incarnation-b"))
                .await;
        }
    });
    assert!(matches!(futures::poll!(&mut transition), Poll::Pending));

    // A queued writer must not deadlock a second assertion in the same UOW.
    let second = tokio::time::timeout(
        WAIT,
        ClaimRepository::assert_room_claim(&mut tx, &room, &fixture.owner, epoch),
    )
    .await
    .expect("repeated assertion must not wait on the queued transition")
    .expect("repeated exact room claim");
    assert!(matches!(futures::poll!(&mut transition), Poll::Pending));

    // Reuse of local authority must never replace each exact claim query.
    assert!(matches!(
        ClaimRepository::assert_room_claim(&mut tx, &room, &fixture.owner, ClaimEpoch(12)).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    assert!(matches!(
        ClaimRepository::assert_room_claim(
            &mut tx,
            &room,
            &NodeIdentity::new("node-a", "wrong-incarnation"),
            epoch,
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    let missing: BareJid = "missing@conference.example.com".parse().expect("room JID");
    assert!(matches!(
        ClaimRepository::assert_room_claim(&mut tx, &missing, &fixture.owner, epoch).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    let _other = ClaimRepository::assert_room_claim(&mut tx, &second_room, &fixture.owner, epoch)
        .await
        .expect("different room still requires its own exact claim");
    let sm_fence = ClaimRepository::assert_sm_claim(&mut tx, &stream, &fixture.owner, epoch)
        .await
        .expect("SM assertion reuses the same node authority");
    assert!(matches!(futures::poll!(&mut transition), Poll::Pending));

    let mut other_tx = fixture.uow.begin().await.expect("another UOW");
    assert!(matches!(
        SmIngressStreamRepository::lock(&mut other_tx, &sm_fence, &stream).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    let archive_message = ArchivedMessage::for_test(
        "sender@example.com/device".parse().expect("sender JID"),
        room.clone().into(),
    );
    assert!(matches!(
        MamArchiveRepository::store_fenced(
            &mut other_tx,
            &second,
            &room,
            &archive_message,
            ArchiveExpectation::Fresh,
        )
        .await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(other_tx);

    // An actual competing claim update must wait on the SQL row lock, even
    // though the node authority was reused by the second assertion.
    {
        let mut takeover_connection = fixture.admin.acquire().await.expect("takeover connection");
        let takeover_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *takeover_connection)
            .await
            .expect("takeover backend PID");
        let takeover_sql = format!(
        "UPDATE {}.clustering_claims SET claim_epoch = 12 WHERE entity = $1 AND claim_epoch = 11",
        fixture.schema
    );
        let takeover = sqlx::query(&takeover_sql)
            .bind(format!("{}:{room}", EntityType::RoomActor.as_db_str()))
            .execute(&mut *takeover_connection);
        tokio::pin!(takeover);
        tokio::time::timeout(WAIT, async {
        loop {
            tokio::select! {
                result = &mut takeover => panic!("takeover passed the held claim: {result:?}"),
                blocked = sqlx::query_scalar::<_, bool>("SELECT cardinality(pg_blocking_pids($1)) > 0")
                    .bind(takeover_pid)
                    .fetch_one(&fixture.admin) => {
                        if blocked.expect("observe takeover") {
                            break;
                        }
                }
            }
        }
        })
        .await
        .expect("takeover must block on the exact claim");
        tx.commit().await.expect("commit held claims");
        let updated = tokio::time::timeout(WAIT, &mut takeover)
            .await
            .expect("takeover resumes after commit")
            .expect("update exact claim");
        assert_eq!(updated.rows_affected(), 1);
    }
    tokio::time::timeout(WAIT, &mut transition)
        .await
        .expect("transition completes after commit");

    let mut stale = fixture.uow.begin().await.expect("begin stale UOW");
    assert!(matches!(
        ClaimRepository::assert_room_claim(&mut stale, &room, &fixture.owner, epoch).await,
        Err(IngressUowError::ClaimFenceMissing)
    ));
    drop(stale);
    drop(transition);
    fixture.close().await;
}

#[tokio::test]
async fn room_claim_reassertion_with_queued_rotation() {
    queued_transition(false).await;
}

#[tokio::test]
async fn room_claim_reassertion_with_queued_disable() {
    queued_transition(true).await;
}
