//! Direct requests against real PostgreSQL: a key that names one request,
//! the target a bound request is addressed to, a decision that moves once,
//! a lane that moves to another target only once it is drained, retention
//! after the origin settles, and epoch-guard refusal.
//!
//! Each test owns a schema.
#![cfg(feature = "clustering")]

use std::time::Duration;

use jid::{FullJid, Jid};
use uuid::Uuid;
use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::ordered_relay::OrderedRelayPayload;
use waddle_server::config::LineageConfig;
use waddle_server::db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_uow::{
    DirectDecision, DirectRefusal, DirectRequestError, DirectRequestRepository, DirectTarget,
    IngressUnitOfWork, IngressUowError, MailboxError, MailboxRepository, MailboxTargetFence,
    NewDirectRequest,
};
use waddle_xmpp::ingress::mailbox::{
    DirectRequestKey, MailboxAdmissionLimits, MailboxLaneFence, MailboxLaneIdentity,
    MailboxLaneOrigin, MailboxLaneRecipient, MailboxOperationKey,
};
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, Entity, EntityType, NodeIdentity, NodeLeaseIdentity,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const WAIT: Duration = Duration::from_secs(10);
const LIMITS: MailboxAdmissionLimits = MailboxAdmissionLimits {
    lane_pending: 1024,
    node_pending: 4096,
};

struct Fixture {
    admin: sqlx::PgPool,
    db: Database,
    uow: IngressUnitOfWork,
    deployment: Uuid,
    schema: String,
}

impl Fixture {
    async fn open(name: &str) -> Option<Self> {
        let database_url = waddle_ws_test_support::require_test_postgres_url()?;
        let schema = format!("waddle_direct_request_{name}_{}", Uuid::new_v4().simple());
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
            "direct-request-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
        )
        .await
        .expect("fixture database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        let deployment = Uuid::new_v4();
        let lineage = LineageConfig {
            deployment_uuid: Some(lineage::DeploymentUuid(deployment)),
            action: None,
        };
        lineage::enroll(&db, &lineage).await.expect("lineage");
        let uow = IngressUnitOfWork::open(db.clone(), lineage).expect("ingress unit of work");
        Some(Self {
            admin,
            db,
            uow,
            deployment,
            schema,
        })
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(&sql.replace("{schema}", &self.schema))
            .fetch_one(&self.admin)
            .await
            .expect("fixture scalar")
    }

    async fn execute(&self, sql: &str) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
        sqlx::query(&sql.replace("{schema}", &self.schema))
            .execute(&self.admin)
            .await
    }

    /// Advance the ingress protocol epoch by one activation step.
    async fn advance_epoch(&self) {
        sqlx::query(&format!(
            "UPDATE {}.ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), \
             lineage_uuid = $1::uuid WHERE id = 1",
            self.schema
        ))
        .bind(self.deployment.to_string())
        .execute(&self.admin)
        .await
        .expect("advance ingress protocol epoch");
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

fn resource() -> FullJid {
    "juliet@example.com/balcony".parse().expect("resource JID")
}

fn node(name: &str) -> NodeIdentity {
    NodeIdentity::new(name, format!("{name}-incarnation"))
}

fn binding_on(name: &str) -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
        &node(name),
    )))
}

fn target_on(name: &str) -> DirectTarget {
    DirectTarget {
        node: node(name),
        binding: binding_on(name),
    }
}

/// The lane a bound request travels on: the account to one of its
/// resources.
fn bound_lane() -> MailboxLaneIdentity {
    MailboxLaneIdentity {
        origin: MailboxLaneOrigin::Entity(Entity::new(
            EntityType::UserActor,
            resource().to_bare().to_string(),
        )),
        recipient: MailboxLaneRecipient::FullJid(resource()),
    }
}

fn bound_fence(epoch: i64, name: &str) -> MailboxLaneFence {
    MailboxLaneFence {
        origin_epoch: ClaimEpoch(epoch),
        target_epoch: ClaimEpoch(epoch),
        target_owner: node(name),
    }
}

fn presence_request(target: Option<DirectTarget>) -> NewDirectRequest {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some("romeo@example.net/orchard".parse().expect("sender JID"));
    presence.to = Some(Jid::from(resource()));
    NewDirectRequest {
        key: DirectRequestKey::mint(),
        recipient: Jid::from(resource()),
        sender_entity: Entity::new(EntityType::UserActor, "romeo@example.net".to_owned()),
        payload: OrderedRelayPayload::Presence {
            recipient: Jid::from(resource()),
            stanza: RemoteStanza(Stanza::Presence(presence)),
        },
        message_key: None,
        target,
    }
}

/// Commit a bound request and its pending entry the way an origin does.
async fn commit(
    fixture: &Fixture,
    fence: &MailboxLaneFence,
    request: &NewDirectRequest,
) -> Result<(), IngressUowError> {
    let mut tx = fixture.uow.begin_with_timeouts(WAIT, WAIT).await?;
    let mut lane =
        MailboxRepository::lock_lane_retargeting_drained(&mut tx, &bound_lane(), fence).await?;
    DirectRequestRepository::insert(&mut tx, request).await?;
    MailboxRepository::append(
        &mut tx,
        &mut lane,
        MailboxOperationKey::Direct(request.key),
        &LIMITS,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Decide a request as the lane's target, in one transaction.
async fn decide(
    fixture: &Fixture,
    request: &NewDirectRequest,
    epoch: i64,
    name: &str,
    decision: DirectDecision,
) -> DirectDecision {
    let operation = MailboxOperationKey::Direct(request.key);
    let mut tx = fixture.uow.begin().await.expect("begin target");
    let entry = MailboxRepository::lookup(&mut tx, operation)
        .await
        .expect("lookup")
        .expect("entry");
    let mut lane = MailboxRepository::lock_lane_for_target(
        &mut tx,
        entry.lane,
        &MailboxTargetFence {
            target_epoch: ClaimEpoch(epoch),
            target_owner: node(name),
        },
    )
    .await
    .expect("target locks lane");
    DirectRequestRepository::lock(&mut tx, request.key)
        .await
        .expect("lock request")
        .expect("request");
    let recorded = DirectRequestRepository::latch_decision(&mut tx, request.key, decision)
        .await
        .expect("latch");
    MailboxRepository::decide(&mut tx, &mut lane, operation, recorded.mailbox())
        .await
        .expect("decide entry");
    tx.commit().await.expect("target commits");
    recorded
}

/// Consume a decided request as its origin and settle it.
async fn consume(fixture: &Fixture, request: &NewDirectRequest, epoch: i64) -> bool {
    let operation = MailboxOperationKey::Direct(request.key);
    let mut tx = fixture.uow.begin().await.expect("begin origin");
    let entry = MailboxRepository::lookup(&mut tx, operation)
        .await
        .expect("lookup")
        .expect("entry");
    let lane = MailboxRepository::lock_lane_for_origin(&mut tx, entry.lane, ClaimEpoch(epoch))
        .await
        .expect("origin locks lane");
    MailboxRepository::consume(&mut tx, &lane, operation)
        .await
        .expect("consume");
    let settled = DirectRequestRepository::settle(&mut tx, request.key)
        .await
        .expect("settle");
    tx.commit().await.expect("origin commits");
    settled
}

#[tokio::test]
async fn a_key_names_one_request_with_its_target() {
    let Some(fixture) = Fixture::open("key").await else {
        return;
    };
    let request = presence_request(Some(target_on("node-b")));
    commit(&fixture, &bound_fence(4, "node-b"), &request)
        .await
        .expect("origin commit");

    let mut tx = fixture.uow.begin().await.expect("begin read");
    let stored = DirectRequestRepository::read(&mut tx, request.key)
        .await
        .expect("read")
        .expect("stored request");
    assert_eq!(stored.recipient, request.recipient);
    assert_eq!(stored.sender_entity, request.sender_entity);
    assert_eq!(stored.target, request.target);
    assert_eq!(stored.decision, None);
    assert!(!stored.settled);

    // A retry of the same attempt finds the stored request.
    assert!(!DirectRequestRepository::insert(&mut tx, &request)
        .await
        .expect("repeat insert"));
    // The key cannot name another target, or no target.
    let mut retargeted = request.clone();
    retargeted.target = Some(target_on("node-c"));
    assert!(matches!(
        DirectRequestRepository::insert(&mut tx, &retargeted).await,
        Err(IngressUowError::DirectRequest(DirectRequestError::KeyReuse))
    ));
    let mut untargeted = request.clone();
    untargeted.target = None;
    assert!(matches!(
        DirectRequestRepository::insert(&mut tx, &untargeted).await,
        Err(IngressUowError::DirectRequest(DirectRequestError::KeyReuse))
    ));
    drop(tx);

    // A request without a target stores none.
    let owner_request = presence_request(None);
    let mut tx = fixture.uow.begin().await.expect("begin owner request");
    assert!(DirectRequestRepository::insert(&mut tx, &owner_request)
        .await
        .expect("insert"));
    assert_eq!(
        DirectRequestRepository::read(&mut tx, owner_request.key)
            .await
            .expect("read")
            .expect("stored")
            .target,
        None
    );
    tx.commit().await.expect("commit");

    // The schema refuses a partial target.
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM pg_catalog.pg_constraint c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.connamespace \
                 WHERE n.nspname = '{schema}' \
                   AND c.conname = 'ingress_direct_requests_target_check'"
            )
            .await,
        1
    );
    assert!(fixture
        .execute(&format!(
            "UPDATE {{schema}}.ingress_direct_requests SET target_binding = NULL \
             WHERE request_key = '{}'",
            request.key.to_storage()
        ))
        .await
        .is_err());
    fixture.close().await;
}

#[tokio::test]
async fn a_decision_moves_once_and_hosted_elsewhere_round_trips() {
    let Some(fixture) = Fixture::open("decision").await else {
        return;
    };
    let request = presence_request(Some(target_on("node-b")));
    commit(&fixture, &bound_fence(4, "node-b"), &request)
        .await
        .expect("origin commit");
    let refused = DirectDecision::Rejected(DirectRefusal::HostedElsewhere);
    assert_eq!(
        decide(&fixture, &request, 4, "node-b", refused).await,
        refused
    );

    let mut tx = fixture.uow.begin().await.expect("begin");
    assert_eq!(
        DirectRequestRepository::read(&mut tx, request.key)
            .await
            .expect("read")
            .expect("stored")
            .decision,
        Some(refused)
    );
    // The same decision again is the recorded one; another is refused.
    assert_eq!(
        DirectRequestRepository::latch_decision(&mut tx, request.key, refused)
            .await
            .expect("repeat latch"),
        refused
    );
    assert!(matches!(
        DirectRequestRepository::latch_decision(&mut tx, request.key, DirectDecision::Accepted)
            .await,
        Err(IngressUowError::DirectRequest(
            DirectRequestError::ConflictingDecision { .. }
        ))
    ));
    drop(tx);
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_direct_requests \
                 WHERE owner_decision = 2 AND refusal = 2"
            )
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_lane_moves_to_another_node_only_once_drained() {
    let Some(fixture) = Fixture::open("retarget").await else {
        return;
    };
    let first = presence_request(Some(target_on("node-b")));
    commit(&fixture, &bound_fence(4, "node-b"), &first)
        .await
        .expect("first commit");

    // The resource bound again on node C under the same claim epoch. The
    // lane still owes node B a decision, so it does not move.
    let moved = presence_request(Some(target_on("node-c")));
    assert!(matches!(
        commit(&fixture, &bound_fence(4, "node-c"), &moved).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));
    // The plain lane lock never moves a lane between nodes of one claim.
    let mut tx = fixture.uow.begin().await.expect("begin");
    assert!(matches!(
        MailboxRepository::lock_lane(&mut tx, &bound_lane(), &bound_fence(4, "node-c")).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));
    drop(tx);

    assert_eq!(
        decide(&fixture, &first, 4, "node-b", DirectDecision::Accepted).await,
        DirectDecision::Accepted
    );
    // Drained, the lane moves to node C, and an older epoch is still
    // refused.
    commit(&fixture, &bound_fence(4, "node-c"), &moved)
        .await
        .expect("commit to the new node");
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_mailbox_lanes \
                 WHERE target_node_id = 'node-c' AND last_sequence = 2"
            )
            .await,
        1
    );
    let stale = presence_request(Some(target_on("node-b")));
    assert!(matches!(
        commit(&fixture, &bound_fence(3, "node-b"), &stale).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));
    fixture.close().await;
}

#[tokio::test]
async fn the_collector_takes_only_requests_consumed_and_settled() {
    let Some(fixture) = Fixture::open("retention").await else {
        return;
    };
    let decided = presence_request(Some(target_on("node-b")));
    commit(&fixture, &bound_fence(4, "node-b"), &decided)
        .await
        .expect("commit decided");
    let pending = presence_request(Some(target_on("node-b")));
    commit(&fixture, &bound_fence(4, "node-b"), &pending)
        .await
        .expect("commit pending");

    let mut tx = fixture.uow.begin().await.expect("begin");
    // A request nobody decided cannot be settled.
    assert!(!DirectRequestRepository::settle(&mut tx, pending.key)
        .await
        .expect("settle undecided"));
    drop(tx);

    decide(&fixture, &decided, 4, "node-b", DirectDecision::Accepted).await;
    assert!(consume(&fixture, &decided, 4).await);

    let mut tx = fixture.uow.begin().await.expect("begin collector");
    assert_eq!(
        DirectRequestRepository::collect_settled(&mut tx, Duration::from_secs(3600), 10)
            .await
            .expect("collect within retention"),
        0
    );
    assert_eq!(
        DirectRequestRepository::collect_settled(&mut tx, Duration::ZERO, 10)
            .await
            .expect("collect"),
        1
    );
    tx.commit().await.expect("collector commits");
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_direct_requests")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries WHERE direct_key IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn the_guarded_table_rejects_unproven_writes() {
    let Some(fixture) = Fixture::open("epoch").await else {
        return;
    };
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_epoch_guard_manifest \
                 WHERE table_name = 'ingress_direct_requests'"
            )
            .await,
        1
    );
    fixture.advance_epoch().await;
    for statement in [
        "UPDATE {schema}.ingress_direct_requests SET owner_decision = 1",
        "DELETE FROM {schema}.ingress_direct_requests",
        "TRUNCATE {schema}.ingress_direct_requests CASCADE",
    ] {
        assert!(
            fixture.execute(statement).await.is_err(),
            "unproven statement must be refused: {statement}"
        );
    }
    // The typed entry proves the live epoch, so repository writes succeed.
    commit(
        &fixture,
        &bound_fence(4, "node-b"),
        &presence_request(Some(target_on("node-b"))),
    )
    .await
    .expect("proven commit at the activated epoch");
    fixture.close().await;
}
