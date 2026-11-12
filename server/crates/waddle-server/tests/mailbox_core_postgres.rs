//! Durable mailbox core against real PostgreSQL: gap-free allocation under
//! concurrency, operation-key idempotency across lanes, replanning to a new
//! owner, and epoch-guard refusal.
//!
//! Each test owns a schema and independent connections. Lock order is
//! established with explicit barriers and by observing a blocked backend in
//! `pg_stat_activity`, never by sleeping.

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use tokio::sync::Barrier;
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_substrate::{gc_expired_aliases, AliasGcBudget, AliasGcProgress};
use waddle_server::ingress_uow::{
    IngressUnitOfWork, IngressUowError, MailboxAppendOutcome, MailboxError, MailboxRepository,
    MailboxTargetFence,
};
use waddle_xmpp::ingress::mailbox::{
    MailboxAdmissionLimits, MailboxAdmissionRefusal, MailboxDispositionChange,
    MailboxDispositionError, MailboxEntryDisposition, MailboxLaneFence, MailboxLaneIdentity,
    MailboxLaneOrigin, MailboxLaneRecipient, MailboxLaneSequence, MailboxOperationKey,
    MailboxOwnerDecision, MailboxRoomLane,
};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;

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
        let schema = format!("waddle_mailbox_{name}_{}", Uuid::new_v4().simple());
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
            "mailbox-core-test",
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

    /// A canonical parent row for one mailbox operation.
    async fn message(&self) -> MailboxOperationKey {
        let key = MessageKey::new();
        sqlx::query(&format!(
            "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) \
             VALUES ($1::uuid, 1, $2)",
            self.schema
        ))
        .bind(key.to_storage().to_string())
        .bind(vec![0x4a_u8; 32])
        .execute(&self.admin)
        .await
        .expect("canonical message");
        MailboxOperationKey::RoomAcceptance(key)
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(&sql.replace("{schema}", &self.schema))
            .fetch_one(&self.admin)
            .await
            .expect("fixture scalar")
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

    /// Wait until one backend is blocked on a lock while running a statement
    /// against the lane table.
    async fn wait_for_blocked_lane_statement(&self) {
        tokio::time::timeout(WAIT, async {
            loop {
                let blocked: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_catalog.pg_stat_activity \
                     WHERE datname = current_database() AND wait_event_type = 'Lock' \
                       AND query LIKE '%ingress_mailbox_lanes%' AND pid <> pg_backend_pid()",
                )
                .fetch_one(&self.admin)
                .await
                .expect("observe blocked backend");
                if blocked > 0 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a second appender must block on the held lane lock");
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

fn room() -> BareJid {
    "room@conference.example.com".parse().expect("room JID")
}

fn lane(session: &str) -> MailboxLaneIdentity {
    MailboxLaneIdentity {
        origin: MailboxLaneOrigin::SmSession(SmSessionId::new(session)),
        recipient: MailboxLaneRecipient::Room {
            room: room(),
            lane: MailboxRoomLane::MucStanza,
        },
    }
}

fn fence(origin: i64, target: i64, node: &str) -> MailboxLaneFence {
    MailboxLaneFence {
        origin_epoch: ClaimEpoch(origin),
        target_epoch: ClaimEpoch(target),
        target_owner: NodeIdentity::new(node, format!("{node}-incarnation")),
    }
}

fn target(epoch: i64, node: &str) -> MailboxTargetFence {
    MailboxTargetFence {
        target_epoch: ClaimEpoch(epoch),
        target_owner: NodeIdentity::new(node, format!("{node}-incarnation")),
    }
}

async fn append(
    uow: &IngressUnitOfWork,
    identity: &MailboxLaneIdentity,
    presented: &MailboxLaneFence,
    operation: MailboxOperationKey,
) -> Result<MailboxAppendOutcome, IngressUowError> {
    let mut tx = uow.begin_with_timeouts(WAIT, WAIT).await?;
    let mut locked = MailboxRepository::lock_lane(&mut tx, identity, presented).await?;
    let outcome = MailboxRepository::append(&mut tx, &mut locked, operation, &LIMITS).await?;
    tx.commit().await?;
    Ok(outcome)
}

#[tokio::test]
async fn allocator_is_gap_free_and_monotonic_under_concurrency() {
    let Some(fixture) = Fixture::open("alloc").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");

    // A rolled-back allocation leaves no committed gap.
    let rolled_back = fixture.message().await;
    {
        let mut tx = fixture
            .uow
            .begin_with_timeouts(WAIT, WAIT)
            .await
            .expect("begin");
        let mut locked = MailboxRepository::lock_lane(&mut tx, &identity, &presented)
            .await
            .expect("lock lane");
        let outcome = MailboxRepository::append(&mut tx, &mut locked, rolled_back, &LIMITS)
            .await
            .expect("append");
        assert_eq!(outcome.entry().sequence, MailboxLaneSequence::new(1));
        drop(tx);
    }

    // The holder of the lane lock commits first; a second appender waits.
    let first = fixture.message().await;
    let second = fixture.message().await;
    let mut holder = fixture
        .uow
        .begin_with_timeouts(WAIT, WAIT)
        .await
        .expect("begin holder");
    let mut held = MailboxRepository::lock_lane(&mut holder, &identity, &presented)
        .await
        .expect("holder locks lane");
    let waiter = {
        let uow = fixture.uow.clone();
        let identity = identity.clone();
        let presented = presented.clone();
        tokio::spawn(async move { append(&uow, &identity, &presented, second).await })
    };
    fixture.wait_for_blocked_lane_statement().await;
    assert!(!waiter.is_finished(), "the waiter cannot commit first");
    let outcome = MailboxRepository::append(&mut holder, &mut held, first, &LIMITS)
        .await
        .expect("holder appends");
    assert_eq!(outcome.entry().sequence, MailboxLaneSequence::new(1));
    holder.commit().await.expect("holder commits");
    let waited = tokio::time::timeout(WAIT, waiter)
        .await
        .expect("waiter finishes")
        .expect("waiter task")
        .expect("waiter appends");
    assert_eq!(waited.entry().sequence, MailboxLaneSequence::new(2));

    // Concurrent appenders of one lane take unique, contiguous sequences.
    const WRITERS: usize = 12;
    let start = Arc::new(Barrier::new(WRITERS));
    let mut tasks = Vec::new();
    for _ in 0..WRITERS {
        let operation = fixture.message().await;
        let uow = fixture.uow.clone();
        let identity = identity.clone();
        let presented = presented.clone();
        let start = Arc::clone(&start);
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            append(&uow, &identity, &presented, operation).await
        }));
    }
    let mut sequences = Vec::new();
    for task in tasks {
        let outcome = tokio::time::timeout(WAIT, task)
            .await
            .expect("writer finishes")
            .expect("writer task")
            .expect("writer appends");
        assert!(matches!(outcome, MailboxAppendOutcome::Appended(_)));
        sequences.push(outcome.entry().sequence.get());
    }
    sequences.sort_unstable();
    let expected: Vec<u64> = (3..3 + WRITERS as u64).collect();
    assert_eq!(sequences, expected);
    assert_eq!(
        fixture
            .scalar("SELECT last_sequence::bigint FROM {schema}.ingress_mailbox_lanes")
            .await,
        2 + WRITERS as i64
    );

    // A different lane allocates independently from its own counter.
    let other = append(
        &fixture.uow,
        &lane("stream-b"),
        &presented,
        fixture.message().await,
    )
    .await
    .expect("other lane appends");
    assert_eq!(other.entry().sequence, MailboxLaneSequence::new(1));
    fixture.close().await;
}

#[tokio::test]
async fn lane_counter_covers_the_unsigned_range_and_reports_exhaustion() {
    let Some(fixture) = Fixture::open("range").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");
    append(&fixture.uow, &identity, &presented, fixture.message().await)
        .await
        .expect("first append");
    // Move the counter and its cursor to one below the maximum. The guard is
    // inert at epoch zero, so the fixture can write the row directly.
    sqlx::query(&format!(
        "UPDATE {}.ingress_mailbox_lanes SET last_sequence = 18446744073709551614, \
         last_dispositioned = 18446744073709551614",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("move counter near its maximum");
    let last = append(&fixture.uow, &identity, &presented, fixture.message().await)
        .await
        .expect("append at the maximum");
    assert_eq!(last.entry().sequence, MailboxLaneSequence::MAX);
    let exhausted = append(&fixture.uow, &identity, &presented, fixture.message().await).await;
    assert!(matches!(
        exhausted,
        Err(IngressUowError::Mailbox(MailboxError::Sequence(_)))
    ));
    fixture.close().await;
}

#[tokio::test]
async fn operation_key_is_idempotent_across_lanes() {
    let Some(fixture) = Fixture::open("idem").await else {
        return;
    };
    let presented = fence(3, 5, "node-a");
    let operation = fixture.message().await;
    let first = append(&fixture.uow, &lane("stream-a"), &presented, operation)
        .await
        .expect("first append");
    assert!(matches!(first, MailboxAppendOutcome::Appended(_)));

    // The same operation arriving on another lane reads the first
    // responsibility; it neither moves it nor allocates on the new lane.
    let again = append(&fixture.uow, &lane("stream-b"), &presented, operation)
        .await
        .expect("cross-lane duplicate");
    assert_eq!(again, MailboxAppendOutcome::Existing(*first.entry()));
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COALESCE(SUM(last_sequence), 0)::bigint FROM {schema}.ingress_mailbox_lanes"
            )
            .await,
        1
    );

    // Racing the same fresh operation across two lanes commits one entry.
    let raced = fixture.message().await;
    let start = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for session in ["stream-c", "stream-d"] {
        let uow = fixture.uow.clone();
        let presented = presented.clone();
        let start = Arc::clone(&start);
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            append(&uow, &lane(session), &presented, raced).await
        }));
    }
    let mut appended = 0;
    let mut entries = Vec::new();
    for task in tasks {
        let outcome = tokio::time::timeout(WAIT, task)
            .await
            .expect("racer finishes")
            .expect("racer task")
            .expect("racer resolves");
        if matches!(outcome, MailboxAppendOutcome::Appended(_)) {
            appended += 1;
        }
        entries.push(*outcome.entry());
    }
    assert_eq!(appended, 1, "exactly one lane takes the responsibility");
    assert_eq!(entries[0], entries[1]);

    // Distinct operations stay distinct.
    let distinct = append(
        &fixture.uow,
        &lane("stream-a"),
        &presented,
        fixture.message().await,
    )
    .await
    .expect("distinct operation");
    assert!(matches!(distinct, MailboxAppendOutcome::Appended(_)));
    assert_ne!(distinct.entry().sequence, first.entry().sequence);
    fixture.close().await;
}

#[tokio::test]
async fn replan_from_owner_a_to_owner_b_keeps_one_responsibility() {
    let Some(fixture) = Fixture::open("replan").await else {
        return;
    };
    let identity = lane("stream-a");
    let operation = fixture.message().await;
    let planned_for_a = append(&fixture.uow, &identity, &fence(3, 5, "node-a"), operation)
        .await
        .expect("append for owner A");

    // The room moves to B. The origin replans the same operation; the lane
    // adopts B's later claim and the first entry is returned unchanged.
    let planned_for_b = append(&fixture.uow, &identity, &fence(3, 9, "node-b"), operation)
        .await
        .expect("replan for owner B");
    assert_eq!(
        planned_for_b,
        MailboxAppendOutcome::Existing(*planned_for_a.entry())
    );
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        1
    );
    let lane_id = planned_for_a.entry().lane;

    // Owner A's claim is superseded: it can neither lock the lane as target
    // nor present its fence as origin-side authority.
    let mut stale = fixture.uow.begin().await.expect("begin stale owner");
    assert!(matches!(
        MailboxRepository::lock_lane_for_target(&mut stale, lane_id, &target(5, "node-a")).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));
    drop(stale);
    assert!(matches!(
        append(&fixture.uow, &identity, &fence(3, 5, "node-a"), operation).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));

    // Owner B decides once. A repeat changes nothing, the opposite decision
    // is refused, and the lookup answers the same outcome on any plan.
    let mut owner = fixture.uow.begin().await.expect("begin owner B");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut owner, lane_id, &target(9, "node-b"))
            .await
            .expect("owner B locks lane");
    let decided = MailboxRepository::decide(
        &mut owner,
        &mut locked,
        operation,
        MailboxOwnerDecision::Accepted,
    )
    .await
    .expect("owner B accepts");
    assert_eq!(
        decided,
        MailboxDispositionChange::Applied(MailboxEntryDisposition::OwnerAccepted)
    );
    assert_eq!(locked.pending(), 0);
    owner.commit().await.expect("owner commits");

    let mut retry = fixture.uow.begin().await.expect("begin retry");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut retry, lane_id, &target(9, "node-b"))
            .await
            .expect("owner B relocks lane");
    assert_eq!(
        MailboxRepository::decide(
            &mut retry,
            &mut locked,
            operation,
            MailboxOwnerDecision::Accepted
        )
        .await
        .expect("repeated acceptance"),
        MailboxDispositionChange::Unchanged(MailboxEntryDisposition::OwnerAccepted)
    );
    assert!(matches!(
        MailboxRepository::decide(
            &mut retry,
            &mut locked,
            operation,
            MailboxOwnerDecision::Rejected
        )
        .await,
        Err(IngressUowError::Mailbox(MailboxError::Disposition(
            MailboxDispositionError::ConflictingDecision { .. }
        )))
    ));
    let found = MailboxRepository::lookup(&mut retry, operation)
        .await
        .expect("lookup")
        .expect("entry exists");
    assert_eq!(found.disposition, MailboxEntryDisposition::OwnerAccepted);
    assert_eq!(found.sequence, planned_for_a.entry().sequence);
    drop(retry);

    // The origin consumes the decided outcome; the decision stays readable.
    let mut origin = fixture.uow.begin().await.expect("begin origin");
    let locked = MailboxRepository::lock_lane_for_origin(&mut origin, lane_id, ClaimEpoch(3))
        .await
        .expect("origin locks lane");
    assert_eq!(
        MailboxRepository::consume(&mut origin, &locked, operation)
            .await
            .expect("origin consumes"),
        MailboxDispositionChange::Applied(MailboxEntryDisposition::Consumed(
            MailboxOwnerDecision::Accepted
        ))
    );
    origin.commit().await.expect("origin commits");
    fixture.close().await;
}

#[tokio::test]
async fn owner_decides_only_the_lane_head_and_the_cursor_is_exclusive() {
    let Some(fixture) = Fixture::open("cursor").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");
    let first = fixture.message().await;
    let second = fixture.message().await;
    let appended = append(&fixture.uow, &identity, &presented, first)
        .await
        .expect("first append");
    append(&fixture.uow, &identity, &presented, second)
        .await
        .expect("second append");
    let lane_id = appended.entry().lane;

    let mut owner = fixture.uow.begin().await.expect("begin owner");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut owner, lane_id, &target(5, "node-a"))
            .await
            .expect("owner locks lane");
    let page = MailboxRepository::read_after_cursor(&mut owner, &locked, 1)
        .await
        .expect("bounded read");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].operation, first);
    // The second entry is not the lane head, so it cannot be dispositioned.
    assert!(matches!(
        MailboxRepository::decide(
            &mut owner,
            &mut locked,
            second,
            MailboxOwnerDecision::Accepted
        )
        .await,
        Err(IngressUowError::Mailbox(MailboxError::Cursor(_)))
    ));
    // Nothing can be consumed before the owner decides.
    assert!(matches!(
        MailboxRepository::consume(&mut owner, &locked, first).await,
        Err(IngressUowError::Mailbox(MailboxError::Disposition(
            MailboxDispositionError::Undecided
        )))
    ));
    MailboxRepository::decide(
        &mut owner,
        &mut locked,
        first,
        MailboxOwnerDecision::Rejected,
    )
    .await
    .expect("owner rejects the head");
    let page = MailboxRepository::read_after_cursor(&mut owner, &locked, 8)
        .await
        .expect("read after the moved cursor");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].operation, second);
    owner.commit().await.expect("owner commits");
    fixture.close().await;
}

#[tokio::test]
async fn full_lane_and_full_node_refuse_before_any_write() {
    let Some(fixture) = Fixture::open("admit").await else {
        return;
    };
    let presented = fence(3, 5, "node-a");
    let tight = MailboxAdmissionLimits {
        lane_pending: 1,
        node_pending: 2,
    };
    async fn admit(
        fixture: &Fixture,
        session: &str,
        presented: &MailboxLaneFence,
        limits: &MailboxAdmissionLimits,
    ) -> Result<MailboxAppendOutcome, IngressUowError> {
        let operation = fixture.message().await;
        let mut tx = fixture.uow.begin().await?;
        let mut locked = MailboxRepository::lock_lane(&mut tx, &lane(session), presented).await?;
        let outcome = MailboxRepository::append(&mut tx, &mut locked, operation, limits).await?;
        tx.commit().await?;
        Ok(outcome)
    }
    admit(&fixture, "stream-a", &presented, &tight)
        .await
        .expect("first lane entry");
    assert!(matches!(
        admit(&fixture, "stream-a", &presented, &tight).await,
        Err(IngressUowError::Mailbox(MailboxError::Admission(
            MailboxAdmissionRefusal::LaneFull { pending: 1, cap: 1 }
        )))
    ));
    admit(&fixture, "stream-b", &presented, &tight)
        .await
        .expect("second lane entry");
    assert!(matches!(
        admit(&fixture, "stream-c", &presented, &tight).await,
        Err(IngressUowError::Mailbox(MailboxError::Admission(
            MailboxAdmissionRefusal::NodeFull { pending: 2, cap: 2 }
        )))
    ));
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        2,
        "a refused append commits nothing"
    );
    fixture.close().await;
}

#[tokio::test]
async fn guarded_tables_reject_unproven_and_unsupported_epoch_writes() {
    let Some(fixture) = Fixture::open("epoch").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");

    // Both tables are enrolled in the guard manifest with both guards.
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_epoch_guard_manifest \
                 WHERE table_name IN ('ingress_mailbox_lanes', 'ingress_mailbox_entries')"
            )
            .await,
        2
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM pg_catalog.pg_trigger t \
                 JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND NOT t.tgisinternal AND t.tgenabled = 'A' \
                   AND c.relname IN ('ingress_mailbox_lanes', 'ingress_mailbox_entries') \
                   AND t.tgtype IN (30, 34)"
            )
            .await,
        4
    );

    fixture.advance_epoch().await;

    // A statement without the transaction-local epoch proof is refused on
    // both tables, and TRUNCATE is refused outright.
    let raw_lane = format!(
        "INSERT INTO {}.ingress_mailbox_lanes (lane_id, digest_version, digest, \
         encoding_version, encoding, origin_epoch, target_epoch, target_node_id, \
         target_node_epoch) VALUES (gen_random_uuid(), 1, decode(repeat('00', 32), 'hex'), 1, \
         '\\x010101', 1, 1, 'node', 'incarnation')",
        fixture.schema
    );
    for statement in [
        raw_lane,
        format!(
            "UPDATE {}.ingress_mailbox_lanes SET origin_epoch = 9",
            fixture.schema
        ),
        format!("DELETE FROM {}.ingress_mailbox_entries", fixture.schema),
        format!("TRUNCATE {}.ingress_mailbox_entries", fixture.schema),
        format!("TRUNCATE {}.ingress_mailbox_lanes CASCADE", fixture.schema),
    ] {
        assert!(
            sqlx::query(&statement)
                .execute(&fixture.admin)
                .await
                .is_err(),
            "unproven statement must be refused: {statement}"
        );
    }
    // A proof for another epoch is as good as none.
    let mut stale = fixture.admin.begin().await.expect("begin stale proof");
    sqlx::query("SELECT set_config('waddle.protocol_epoch', '0', true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)")
        .execute(&mut *stale)
        .await
        .expect("install stale proof");
    assert!(sqlx::query(&format!(
        "DELETE FROM {}.ingress_mailbox_lanes",
        fixture.schema
    ))
    .execute(&mut *stale)
    .await
    .is_err());
    drop(stale);

    // The typed entry proves the live epoch, so repository writes succeed.
    let operation = fixture.message_at_epoch().await;
    let appended = append(&fixture.uow, &identity, &presented, operation)
        .await
        .expect("proven append at the activated epoch");
    assert!(matches!(appended, MailboxAppendOutcome::Appended(_)));

    // An epoch this binary does not implement refuses the transaction before
    // any mailbox statement runs.
    fixture.advance_epoch().await;
    assert!(matches!(
        fixture.uow.begin().await,
        Err(IngressUowError::EpochUnsupported { .. } | IngressUowError::Substrate(_))
    ));
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        1
    );
    fixture.close().await;
}

impl Fixture {
    /// Make `operation`'s canonical message terminal with a retention clock
    /// far in the past, so only retained custody keeps it from collection.
    async fn expire(&self, operation: MailboxOperationKey) {
        sqlx::query(&format!(
            "UPDATE {}.ingress_messages SET terminal_at = '2000-01-01', \
             retention_eligible_at = '2000-01-01' WHERE message_key = $1::uuid",
            self.schema
        ))
        .bind(operation.message_key().to_storage().to_string())
        .execute(&self.admin)
        .await
        .expect("expire canonical message");
    }

    async fn message_exists(&self, operation: MailboxOperationKey) -> bool {
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM {}.ingress_messages WHERE message_key = $1::uuid",
            self.schema
        ))
        .bind(operation.message_key().to_storage().to_string())
        .fetch_one(&self.admin)
        .await
        .expect("count canonical message")
            == 1
    }

    /// One pass of the real message collector.
    async fn collect(&self) -> usize {
        gc_expired_aliases(
            &self.db,
            chrono::Utc::now(),
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + WAIT,
                lock_timeout: Duration::from_millis(250),
                statement_timeout: Duration::from_secs(5),
                scan_timeout: Duration::from_secs(5),
                progress: AliasGcProgress::default(),
            },
        )
        .await
        .expect("collector pass")
        .deleted_messages
    }

    /// Wait until one backend is blocked on a lock while locking a canonical
    /// message.
    async fn wait_for_blocked_message_lock(&self) {
        tokio::time::timeout(WAIT, async {
            loop {
                let blocked: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_catalog.pg_stat_activity \
                     WHERE datname = current_database() AND wait_event_type = 'Lock' \
                       AND query LIKE '%ingress_messages%' AND pid <> pg_backend_pid()",
                )
                .fetch_one(&self.admin)
                .await
                .expect("observe blocked backend");
                if blocked > 0 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the appender must block on the held canonical message");
    }

    /// A canonical parent written with a transaction-local epoch proof, for
    /// use once the guard is active.
    async fn message_at_epoch(&self) -> MailboxOperationKey {
        let key = MessageKey::new();
        let mut tx = self.admin.begin().await.expect("begin proven insert");
        sqlx::query(&format!(
            "SELECT set_config('waddle.protocol_epoch', \
             (SELECT epoch::text FROM {}.ingress_protocol_epoch WHERE id = 1), true), \
             set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
            self.schema
        ))
        .execute(&mut *tx)
        .await
        .expect("install epoch proof");
        sqlx::query(&format!(
            "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) \
             VALUES ($1::uuid, 1, $2)",
            self.schema
        ))
        .bind(key.to_storage().to_string())
        .bind(vec![0x4b_u8; 32])
        .execute(&mut *tx)
        .await
        .expect("canonical message at the activated epoch");
        tx.commit().await.expect("commit proven insert");
        MailboxOperationKey::RoomAcceptance(key)
    }
}

#[tokio::test]
async fn unconsumed_entry_retains_its_message_and_a_consumed_entry_leaves_with_it() {
    let Some(fixture) = Fixture::open("retain").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");
    let operation = fixture.message().await;
    let appended = append(&fixture.uow, &identity, &presented, operation)
        .await
        .expect("append");
    let lane_id = appended.entry().lane;
    fixture.expire(operation).await;

    // Pending, then decided but not consumed: the message stays.
    assert_eq!(fixture.collect().await, 0);
    assert!(fixture.message_exists(operation).await);
    let mut owner = fixture.uow.begin().await.expect("begin owner");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut owner, lane_id, &target(5, "node-a"))
            .await
            .expect("owner locks lane");
    MailboxRepository::decide(
        &mut owner,
        &mut locked,
        operation,
        MailboxOwnerDecision::Accepted,
    )
    .await
    .expect("owner accepts");
    owner.commit().await.expect("owner commits");
    fixture.expire(operation).await;
    assert_eq!(fixture.collect().await, 0);
    assert!(fixture.message_exists(operation).await);

    // Consumed: the message is collected, the entry goes with it through the
    // cascade, and the lane keeps its counter and cursor.
    let mut origin = fixture.uow.begin().await.expect("begin origin");
    let locked = MailboxRepository::lock_lane_for_origin(&mut origin, lane_id, ClaimEpoch(3))
        .await
        .expect("origin locks lane");
    MailboxRepository::consume(&mut origin, &locked, operation)
        .await
        .expect("origin consumes");
    origin.commit().await.expect("origin commits");
    fixture.expire(operation).await;
    assert_eq!(fixture.collect().await, 1);
    assert!(!fixture.message_exists(operation).await);
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_mailbox_lanes \
                 WHERE last_sequence = 1 AND last_dispositioned = 1"
            )
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn collector_skips_a_message_an_appender_holds() {
    let Some(fixture) = Fixture::open("gcskip").await else {
        return;
    };
    // Appender-first schedule: the appender holds its lane and the canonical
    // message while the collector runs. The collector must finish without
    // waiting for either and must leave the message.
    let operation = fixture.message().await;
    fixture.expire(operation).await;
    let mut appender = fixture
        .uow
        .begin_with_timeouts(WAIT, WAIT)
        .await
        .expect("begin appender");
    let mut locked =
        MailboxRepository::lock_lane(&mut appender, &lane("stream-a"), &fence(3, 5, "node-a"))
            .await
            .expect("appender locks lane");
    MailboxRepository::append(&mut appender, &mut locked, operation, &LIMITS)
        .await
        .expect("appender appends under the canonical lock");
    let collected = tokio::time::timeout(WAIT, fixture.collect())
        .await
        .expect("the collector never waits on an appender");
    assert_eq!(collected, 0);
    appender.commit().await.expect("appender commits");
    assert!(fixture.message_exists(operation).await);
    // The committed pending entry now retains the message on its own.
    assert_eq!(fixture.collect().await, 0);
    assert!(fixture.message_exists(operation).await);
    fixture.close().await;
}

#[tokio::test]
async fn appender_behind_a_collector_neither_deadlocks_nor_allocates() {
    let Some(fixture) = Fixture::open("gcfirst").await else {
        return;
    };
    // Collector-first schedule: a connection takes the canonical message the
    // way the collector does, then the appender locks its lane and waits for
    // that message. The collector never asks for a lane, so it deletes the
    // message and commits; the appender then finds no parent.
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");
    let operation = fixture.message().await;
    fixture.expire(operation).await;
    let mut collector = fixture.admin.begin().await.expect("begin collector");
    let held: Option<i32> = sqlx::query_scalar(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1::uuid \
         AND terminal_at IS NOT NULL FOR UPDATE SKIP LOCKED",
        fixture.schema
    ))
    .bind(operation.message_key().to_storage().to_string())
    .fetch_optional(&mut *collector)
    .await
    .expect("collector locks the canonical message");
    assert_eq!(held, Some(1));

    let appender = {
        let uow = fixture.uow.clone();
        let identity = identity.clone();
        let presented = presented.clone();
        tokio::spawn(async move { append(&uow, &identity, &presented, operation).await })
    };
    fixture.wait_for_blocked_message_lock().await;
    assert!(!appender.is_finished());
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE message_key = $1::uuid",
        fixture.schema
    ))
    .bind(operation.message_key().to_storage().to_string())
    .execute(&mut *collector)
    .await
    .expect("collector deletes without a lane lock");
    collector.commit().await.expect("collector commits");

    let refused = tokio::time::timeout(WAIT, appender)
        .await
        .expect("the appender is released, not deadlocked")
        .expect("appender task");
    assert!(matches!(
        refused,
        Err(IngressUowError::Mailbox(MailboxError::ParentMissing))
    ));
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT COALESCE(SUM(last_sequence), 0)::bigint FROM {schema}.ingress_mailbox_lanes"
            )
            .await,
        0,
        "the refused append rolled its lane back"
    );
    fixture.close().await;
}

/// A lane recorded for owner A is invisible to B's backlog scan. Once B
/// presents a later claim for the lane's room, the lane records B and B's
/// scan finds it; an older or equal-epoch claim from another node adopts
/// nothing, and a drained lane is not adopted at all.
#[tokio::test]
async fn successor_owner_adopts_a_backlogged_lane_only_with_a_later_claim() {
    let Some(fixture) = Fixture::open("adopt").await else {
        return;
    };
    let identity = lane("stream-a");
    let operation = fixture.message().await;
    let appended = append(&fixture.uow, &identity, &fence(3, 5, "node-a"), operation)
        .await
        .expect("append for owner A");
    let lane_id = appended.entry().lane;
    let node_a = target(5, "node-a").target_owner;
    let node_b = target(9, "node-b").target_owner;

    let mut tx = fixture.uow.begin().await.expect("begin scan");
    assert_eq!(
        MailboxRepository::scan_backlog(&mut tx, &node_b, None, 16)
            .await
            .expect("B's backlog"),
        Vec::new(),
        "B's backlog scan reads only lanes recorded for B"
    );
    let foreign = MailboxRepository::scan_foreign_backlog(&mut tx, &node_b, None, 16)
        .await
        .expect("lanes recorded for other nodes");
    assert_eq!(foreign.len(), 1);
    assert_eq!(foreign[0].0, lane_id);
    assert_eq!(
        foreign[0].1.encoding().decode().expect("lane identity"),
        identity,
        "the scan returns the lane's identity, from which its room is read"
    );
    assert_eq!(
        MailboxRepository::scan_foreign_backlog(&mut tx, &node_a, None, 16)
            .await
            .expect("A's foreign scan")
            .len(),
        0,
        "a lane recorded for A is not foreign to A"
    );
    // The same claim epoch cannot be held by another node, and an older
    // claim is stale: neither moves the lane.
    assert!(
        !MailboxRepository::adopt_backlogged_lane(&mut tx, lane_id, &target(5, "node-b"))
            .await
            .expect("contradictory claim")
    );
    assert!(
        !MailboxRepository::adopt_backlogged_lane(&mut tx, lane_id, &target(4, "node-b"))
            .await
            .expect("older claim")
    );
    assert!(
        MailboxRepository::adopt_backlogged_lane(&mut tx, lane_id, &target(9, "node-b"))
            .await
            .expect("later claim")
    );
    tx.commit().await.expect("commit adoption");

    let mut tx = fixture.uow.begin().await.expect("begin successor");
    assert_eq!(
        MailboxRepository::scan_backlog(&mut tx, &node_b, None, 16)
            .await
            .expect("B's backlog after adoption"),
        vec![lane_id]
    );
    assert!(matches!(
        MailboxRepository::lock_lane_for_target(&mut tx, lane_id, &target(5, "node-a")).await,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_)))
    ));
    drop(tx);

    let mut owner = fixture.uow.begin().await.expect("begin owner B");
    let mut locked = MailboxRepository::claim_backlogged_lane(&mut owner, lane_id, &node_b)
        .await
        .expect("claim adopted lane")
        .expect("B's poller may work the adopted lane");
    MailboxRepository::decide(
        &mut owner,
        &mut locked,
        operation,
        MailboxOwnerDecision::Accepted,
    )
    .await
    .expect("owner B decides the predecessor's entry");
    owner.commit().await.expect("owner B commits");

    // The entry is the one the origin appended for A: one responsibility.
    let mut tx = fixture.uow.begin().await.expect("begin lookup");
    let entry = MailboxRepository::lookup(&mut tx, operation)
        .await
        .expect("lookup")
        .expect("entry");
    assert_eq!(entry.sequence, appended.entry().sequence);
    assert_eq!(entry.disposition, MailboxEntryDisposition::OwnerAccepted);
    // A drained lane owes nothing and is not adopted by a still later claim.
    assert!(
        !MailboxRepository::adopt_backlogged_lane(&mut tx, lane_id, &target(12, "node-c"))
            .await
            .expect("drained lane")
    );
    drop(tx);
    fixture.close().await;
}

/// The sweep's scan returns decided entries no origin consumed once they are
/// older than the grace period, never a pending or consumed entry, and the
/// lane claim it uses lets the entry be consumed without an origin fence.
#[tokio::test]
async fn unconsumed_decisions_are_found_after_the_grace_and_consumed_once() {
    let Some(fixture) = Fixture::open("sweep").await else {
        return;
    };
    let identity = lane("stream-a");
    let presented = fence(3, 5, "node-a");
    let decided = fixture.message().await;
    let pending = fixture.message().await;
    let lane_id = append(&fixture.uow, &identity, &presented, decided)
        .await
        .expect("first append")
        .entry()
        .lane;
    append(&fixture.uow, &identity, &presented, pending)
        .await
        .expect("second append");
    let mut owner = fixture.uow.begin().await.expect("begin owner");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut owner, lane_id, &target(5, "node-a"))
            .await
            .expect("owner locks lane");
    MailboxRepository::decide(
        &mut owner,
        &mut locked,
        decided,
        MailboxOwnerDecision::Rejected,
    )
    .await
    .expect("owner rejects the head");
    owner.commit().await.expect("owner commits");

    let mut tx = fixture.uow.begin().await.expect("begin sweep");
    assert_eq!(
        MailboxRepository::scan_unconsumed_decisions(&mut tx, Duration::from_secs(3600), None, 16)
            .await
            .expect("scan inside the grace"),
        Vec::new(),
        "an entry younger than the grace is left to its origin"
    );
    let found = MailboxRepository::scan_unconsumed_decisions(&mut tx, Duration::ZERO, None, 16)
        .await
        .expect("scan past the grace");
    assert_eq!(found.len(), 1, "only the decided entry: {found:?}");
    assert_eq!(found[0].operation, decided);
    assert_eq!(found[0].disposition, MailboxEntryDisposition::OwnerRejected);
    assert_eq!(
        MailboxRepository::scan_unconsumed_decisions(
            &mut tx,
            Duration::ZERO,
            Some(decided.message_key()),
            16
        )
        .await
        .expect("scan after the cursor"),
        Vec::new()
    );
    let locked = MailboxRepository::claim_lane_for_sweep(&mut tx, lane_id)
        .await
        .expect("claim lane")
        .expect("lane is free");
    assert_eq!(
        MailboxRepository::consume(&mut tx, &locked, decided)
            .await
            .expect("consume"),
        MailboxDispositionChange::Applied(MailboxEntryDisposition::Consumed(
            MailboxOwnerDecision::Rejected
        ))
    );
    tx.commit().await.expect("commit sweep");

    let mut tx = fixture.uow.begin().await.expect("begin second sweep");
    assert_eq!(
        MailboxRepository::scan_unconsumed_decisions(&mut tx, Duration::ZERO, None, 16)
            .await
            .expect("second scan"),
        Vec::new(),
        "a consumed entry is not swept again and a pending entry never is"
    );
    drop(tx);
    fixture.close().await;
}
