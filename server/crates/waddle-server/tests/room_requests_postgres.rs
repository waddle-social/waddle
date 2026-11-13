//! Canonical room requests, frozen responses and durable occupancy against
//! real PostgreSQL: one entry per request across lanes, a decision that
//! moves once with its responses, a single hand-over to the origin,
//! occupancy as the owner writes it and a successor reads it, retention
//! after settlement, and epoch-guard refusal.
//!
//! Each test owns a schema.

use std::time::Duration;

use jid::{BareJid, FullJid};
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::{lineage, Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::ingress_uow::{
    IngressUnitOfWork, IngressUowError, MailboxAppendOutcome, MailboxError, MailboxRepository,
    MailboxTargetFence, NewRoomRequest, RoomDecisionLatch, RoomOccupantLookup, RoomOccupantRecord,
    RoomOccupantRepository, RoomRequestError, RoomRequestInsert, RoomRequestRepository,
    RoomRequestSource, RoomResponseRepository, ROOM_REQUEST_SETTLED_RETENTION,
};
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ingress::mailbox::{
    MailboxAdmissionLimits, MailboxDispositionChange, MailboxEntryDisposition, MailboxLaneFence,
    MailboxLaneIdentity, MailboxLaneOrigin, MailboxLaneRecipient, MailboxOperationKey,
    MailboxOwnerDecision, MailboxRoomLane, RoomRequestKey, RoomRequestKind,
};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::{Affiliation, OccupancySeatId, OccupancySessionGeneration, Role};
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
        let schema = format!("waddle_room_request_{name}_{}", Uuid::new_v4().simple());
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
            "room-request-test",
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

fn room() -> BareJid {
    "room@conference.example.com".parse().expect("room JID")
}

fn occupant() -> FullJid {
    "juliet@example.com/balcony".parse().expect("occupant JID")
}

fn generation(value: u128) -> OccupancySessionGeneration {
    OccupancySessionGeneration::from_uuid(Uuid::from_u128(value))
}

fn lane(generation: OccupancySessionGeneration, lane: MailboxRoomLane) -> MailboxLaneIdentity {
    MailboxLaneIdentity {
        origin: MailboxLaneOrigin::BoundResource {
            jid: occupant(),
            generation,
        },
        recipient: MailboxLaneRecipient::Room { room: room(), lane },
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

fn source() -> RoomRequestSource {
    RoomRequestSource {
        principal: AuthenticatedPrincipalRef::new(
            occupant().to_bare(),
            AuthContextId::new(Uuid::from_u128(0x51)),
            AuthContextVersion::INITIAL,
            PrincipalAuthEpoch::INITIAL,
        ),
        sm_occurrence: None,
    }
}

fn presence_to(nick: &str, status: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(occupant().into());
    presence.to = Some(
        room()
            .with_resource_str(nick)
            .expect("occupant room JID")
            .into(),
    );
    if !status.is_empty() {
        presence.set_status(String::new(), status.to_owned());
    }
    presence
}

fn request(
    kind: RoomRequestKind,
    generation: OccupancySessionGeneration,
    stanza: Stanza,
) -> NewRoomRequest {
    NewRoomRequest {
        key: RoomRequestKey::mint(),
        kind,
        room: room(),
        occupant: occupant(),
        generation,
        source: source(),
        stanza,
    }
}

fn join(generation: OccupancySessionGeneration) -> NewRoomRequest {
    request(
        RoomRequestKind::Join,
        generation,
        Stanza::Presence(presence_to("juliet", "")),
    )
}

fn record(
    generation: OccupancySessionGeneration,
    join_request: Option<RoomRequestKey>,
) -> RoomOccupantRecord {
    RoomOccupantRecord {
        room: room(),
        occupant: occupant(),
        nick: "juliet".to_owned(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
        generation,
        seat: None,
        join_request,
        presence: Some(presence_to("juliet", "here")),
        room_epoch: ClaimEpoch(5),
    }
}

/// Commit a request and its pending entry the way an origin does.
async fn commit_request(
    uow: &IngressUnitOfWork,
    identity: &MailboxLaneIdentity,
    presented: &MailboxLaneFence,
    request: &NewRoomRequest,
) -> Result<MailboxAppendOutcome, IngressUowError> {
    let mut tx = uow.begin_with_timeouts(WAIT, WAIT).await?;
    let mut locked = MailboxRepository::lock_lane(&mut tx, identity, presented).await?;
    RoomRequestRepository::insert(&mut tx, request).await?;
    let outcome = MailboxRepository::append(
        &mut tx,
        &mut locked,
        MailboxOperationKey::for_request(request.kind, request.key),
        &LIMITS,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Decide a request as the room owner: latch, freeze the responses, write
/// occupancy and move the lane's cursor, in one transaction.
async fn decide_request(
    fixture: &Fixture,
    request: &NewRoomRequest,
    decision: MailboxOwnerDecision,
    responses: &[Stanza],
) {
    let operation = MailboxOperationKey::for_request(request.kind, request.key);
    let mut tx = fixture.uow.begin().await.expect("begin owner");
    let entry = MailboxRepository::lookup(&mut tx, operation)
        .await
        .expect("lookup")
        .expect("entry");
    let mut locked =
        MailboxRepository::lock_lane_for_target(&mut tx, entry.lane, &target(5, "node-a"))
            .await
            .expect("owner locks lane");
    RoomRequestRepository::lock(&mut tx, request.key)
        .await
        .expect("lock request")
        .expect("request");
    let RoomDecisionLatch::Applied(latched) =
        RoomRequestRepository::latch_decision(&mut tx, request.key, decision)
            .await
            .expect("latch")
    else {
        panic!("first decision must apply");
    };
    RoomResponseRepository::freeze(&mut tx, &latched, responses)
        .await
        .expect("freeze responses");
    if decision == MailboxOwnerDecision::Accepted && request.kind == RoomRequestKind::Join {
        // The room actor records the seat before the join's projection
        // commits; the decision completes that row.
        RoomOccupantRepository::record_seat(&mut tx, &record(request.generation, None))
            .await
            .expect("record seat");
        assert!(RoomOccupantRepository::confirm_join(
            &mut tx,
            &record(request.generation, Some(request.key))
        )
        .await
        .expect("confirm join"));
    }
    MailboxRepository::decide(&mut tx, &mut locked, operation, decision)
        .await
        .expect("decide entry");
    tx.commit().await.expect("owner commits");
}

/// Consume a decided request as its origin and return the frozen responses.
async fn consume_request(
    fixture: &Fixture,
    request: &NewRoomRequest,
) -> (MailboxDispositionChange, bool, Vec<Stanza>) {
    let operation = MailboxOperationKey::for_request(request.kind, request.key);
    let mut tx = fixture.uow.begin().await.expect("begin origin");
    let entry = MailboxRepository::lookup(&mut tx, operation)
        .await
        .expect("lookup")
        .expect("entry");
    let locked = MailboxRepository::lock_lane_for_origin(&mut tx, entry.lane, ClaimEpoch(3))
        .await
        .expect("origin locks lane");
    let responses = RoomResponseRepository::read(&mut tx, request.key)
        .await
        .expect("read responses");
    let change = MailboxRepository::consume(&mut tx, &locked, operation)
        .await
        .expect("consume");
    let settled = RoomRequestRepository::settle(&mut tx, request.key)
        .await
        .expect("settle");
    tx.commit().await.expect("origin commits");
    (change, settled, responses)
}

#[tokio::test]
async fn request_has_one_entry_across_lanes_and_names_only_its_request() {
    let Some(fixture) = Fixture::open("parent").await else {
        return;
    };
    let first = generation(1);
    let request = join(first);
    let presented = fence(3, 5, "node-a");
    let appended = commit_request(
        &fixture.uow,
        &lane(first, MailboxRoomLane::MucStanza),
        &presented,
        &request,
    )
    .await
    .expect("origin commit");
    assert!(matches!(appended, MailboxAppendOutcome::Appended(_)));
    assert_eq!(
        appended.entry().operation,
        MailboxOperationKey::RoomJoin(request.key)
    );

    // A retry of the same attempt, planned onto another lane, finds the
    // first entry and the stored request.
    let mut tx = fixture.uow.begin().await.expect("begin retry");
    let mut other = MailboxRepository::lock_lane(
        &mut tx,
        &lane(generation(2), MailboxRoomLane::MucStanza),
        &presented,
    )
    .await
    .expect("lock other lane");
    assert!(matches!(
        RoomRequestRepository::insert(&mut tx, &request)
            .await
            .expect("repeat insert"),
        RoomRequestInsert::Existing(_)
    ));
    let repeated = MailboxRepository::append(
        &mut tx,
        &mut other,
        MailboxOperationKey::RoomJoin(request.key),
        &LIMITS,
    )
    .await
    .expect("repeat append");
    assert!(matches!(repeated, MailboxAppendOutcome::Existing(_)));
    assert_eq!(repeated.entry().lane, appended.entry().lane);
    // The key cannot be reused for a different request.
    let mut reused = join(first);
    reused.key = request.key;
    reused.kind = RoomRequestKind::Leave;
    assert!(matches!(
        RoomRequestRepository::insert(&mut tx, &reused).await,
        Err(IngressUowError::RoomRequest(RoomRequestError::KeyReuse))
    ));
    // An operation whose request does not exist has no parent to refer to.
    assert!(matches!(
        MailboxRepository::append(
            &mut tx,
            &mut other,
            MailboxOperationKey::RoomIq(RoomRequestKey::mint()),
            &LIMITS,
        )
        .await,
        Err(IngressUowError::Mailbox(MailboxError::ParentMissing))
    ));
    drop(tx);

    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_mailbox_entries \
                 WHERE request_key IS NOT NULL AND message_key IS NULL AND operation_kind = 2"
            )
            .await,
        1
    );
    // The schema refuses an entry with the wrong parent for its kind, with
    // both parents, and a second entry for one request.
    let lane_id = appended.entry().lane.as_uuid();
    for statement in [
        format!(
            "INSERT INTO {{schema}}.ingress_mailbox_entries (lane_id, sequence, operation_kind, \
             request_key) VALUES ('{lane_id}', 7, 1, '{}')",
            request.key.to_storage()
        ),
        format!(
            "INSERT INTO {{schema}}.ingress_mailbox_entries (lane_id, sequence, operation_kind) \
             VALUES ('{lane_id}', 7, 3)"
        ),
        format!(
            "INSERT INTO {{schema}}.ingress_mailbox_entries (lane_id, sequence, operation_kind, \
             request_key) VALUES ('{lane_id}', 7, 4, '{}')",
            request.key.to_storage()
        ),
        format!(
            "INSERT INTO {{schema}}.ingress_mailbox_entries (lane_id, sequence, operation_kind, \
             request_key) VALUES ('{lane_id}', 7, 6, '{}')",
            request.key.to_storage()
        ),
    ] {
        assert!(
            fixture.execute(&statement).await.is_err(),
            "malformed entry must be refused: {statement}"
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn decision_freezes_ordered_responses_and_the_origin_consumes_them_once() {
    let Some(fixture) = Fixture::open("custody").await else {
        return;
    };
    let first = generation(1);
    let request = join(first);
    commit_request(
        &fixture.uow,
        &lane(first, MailboxRoomLane::MucStanza),
        &fence(3, 5, "node-a"),
        &request,
    )
    .await
    .expect("origin commit");

    // Before the decision there is nothing to hand over.
    let mut tx = fixture.uow.begin().await.expect("begin early read");
    assert!(RoomResponseRepository::read(&mut tx, request.key)
        .await
        .expect("read")
        .is_empty());
    assert!(!RoomRequestRepository::settle(&mut tx, request.key)
        .await
        .expect("settle before decision"));
    drop(tx);

    let responses = [
        Stanza::Presence(presence_to("romeo", "other occupant")),
        Stanza::Presence(presence_to("juliet", "self")),
        Stanza::Presence(presence_to("juliet", "third")),
    ];
    decide_request(
        &fixture,
        &request,
        MailboxOwnerDecision::Accepted,
        &responses,
    )
    .await;

    let (change, settled, frozen) = consume_request(&fixture, &request).await;
    assert_eq!(
        change,
        MailboxDispositionChange::Applied(MailboxEntryDisposition::Consumed(
            MailboxOwnerDecision::Accepted
        ))
    );
    assert!(settled);
    let statuses: Vec<String> = frozen
        .iter()
        .map(|stanza| match stanza {
            Stanza::Presence(presence) => presence
                .statuses
                .values()
                .next()
                .cloned()
                .unwrap_or_default(),
            Stanza::Message(_) | Stanza::Iq(_) => panic!("presence expected"),
        })
        .collect();
    assert_eq!(statuses, ["other occupant", "self", "third"]);

    // A second reader finds the entry consumed and the request settled.
    let (change, settled, _) = consume_request(&fixture, &request).await;
    assert!(matches!(change, MailboxDispositionChange::Unchanged(_)));
    assert!(!settled);
    fixture.close().await;
}

#[tokio::test]
async fn decision_moves_once_and_responses_need_the_latching_transaction() {
    let Some(fixture) = Fixture::open("latch").await else {
        return;
    };
    let first = generation(1);
    let request = join(first);
    commit_request(
        &fixture.uow,
        &lane(first, MailboxRoomLane::MucStanza),
        &fence(3, 5, "node-a"),
        &request,
    )
    .await
    .expect("origin commit");

    let mut tx = fixture.uow.begin().await.expect("begin latch");
    let RoomDecisionLatch::Applied(latched) =
        RoomRequestRepository::latch_decision(&mut tx, request.key, MailboxOwnerDecision::Rejected)
            .await
            .expect("latch")
    else {
        panic!("first decision must apply");
    };
    // The proof belongs to the transaction that latched.
    let mut other = fixture.uow.begin().await.expect("begin other");
    assert!(matches!(
        RoomResponseRepository::freeze(
            &mut other,
            &latched,
            &[Stanza::Presence(presence_to("juliet", "error"))]
        )
        .await,
        Err(IngressUowError::RoomRequest(
            RoomRequestError::DecisionNotLatchedHere
        ))
    ));
    drop(other);
    RoomResponseRepository::freeze(
        &mut tx,
        &latched,
        &[Stanza::Presence(presence_to("juliet", "error"))],
    )
    .await
    .expect("freeze refusal");
    tx.commit().await.expect("commit latch");

    let mut tx = fixture.uow.begin().await.expect("begin replay");
    assert!(matches!(
        RoomRequestRepository::latch_decision(&mut tx, request.key, MailboxOwnerDecision::Rejected)
            .await
            .expect("repeat"),
        RoomDecisionLatch::Unchanged(MailboxOwnerDecision::Rejected)
    ));
    assert!(matches!(
        RoomRequestRepository::latch_decision(&mut tx, request.key, MailboxOwnerDecision::Accepted)
            .await,
        Err(IngressUowError::RoomRequest(
            RoomRequestError::ConflictingDecision { .. }
        ))
    ));
    drop(tx);
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_room_responses")
            .await,
        1
    );
    // A request stanza must fit its kind.
    let mut tx = fixture.uow.begin().await.expect("begin mismatch");
    let mismatched = request_with_kind(RoomRequestKind::Iq, first);
    assert!(matches!(
        RoomRequestRepository::insert(&mut tx, &mismatched).await,
        Err(IngressUowError::RoomRequest(
            RoomRequestError::StanzaKindMismatch {
                kind: RoomRequestKind::Iq
            }
        ))
    ));
    drop(tx);
    fixture.close().await;
}

fn request_with_kind(
    kind: RoomRequestKind,
    generation: OccupancySessionGeneration,
) -> NewRoomRequest {
    request(
        kind,
        generation,
        Stanza::Presence(presence_to("juliet", "")),
    )
}

#[tokio::test]
async fn occupancy_is_written_by_generation_and_recovered_by_a_successor() {
    let Some(fixture) = Fixture::open("occupancy").await else {
        return;
    };
    let first = generation(1);
    let second = generation(2);
    let mut tx = fixture.uow.begin().await.expect("begin owner");
    assert!(matches!(
        RoomOccupantRepository::lookup(&mut tx, &room(), &occupant())
            .await
            .expect("lookup"),
        RoomOccupantLookup::Absent
    ));
    assert!(
        !RoomOccupantRepository::bare_jid_occupies(&mut tx, &room(), &occupant().to_bare())
            .await
            .expect("occupancy probe")
    );
    // A join's decision completes only a row the actor recorded.
    assert!(
        !RoomOccupantRepository::confirm_join(&mut tx, &record(first, None))
            .await
            .expect("confirm without a seat")
    );
    RoomOccupantRepository::record_seat(&mut tx, &record(first, None))
        .await
        .expect("seat");
    assert!(
        RoomOccupantRepository::bare_jid_occupies(&mut tx, &room(), &occupant().to_bare())
            .await
            .expect("occupancy probe")
    );
    // A bare JID that merely shares a prefix is not an occupant.
    let prefix: BareJid = "juliet@example.co".parse().expect("prefix JID");
    assert!(
        !RoomOccupantRepository::bare_jid_occupies(&mut tx, &room(), &prefix)
            .await
            .expect("prefix probe")
    );

    // A presence update or leave from another generation is stale.
    let mut stale = record(second, None);
    stale.nick = "impostor".to_owned();
    assert!(!RoomOccupantRepository::update_presence(&mut tx, &stale)
        .await
        .expect("stale update"));
    assert!(
        !RoomOccupantRepository::remove(&mut tx, &room(), &occupant(), Some(second))
            .await
            .expect("stale leave")
    );
    let mut renamed = record(first, None);
    renamed.nick = "capulet".to_owned();
    renamed.presence = Some(presence_to("capulet", "renamed"));
    renamed.room_epoch = ClaimEpoch(6);
    assert!(RoomOccupantRepository::update_presence(&mut tx, &renamed)
        .await
        .expect("current update"));
    // A join from a later connection replaces the row.
    RoomOccupantRepository::record_seat(&mut tx, &record(second, None))
        .await
        .expect("rejoin");
    // The earlier connection's presence does not survive the replacement,
    // and a decision for the earlier connection no longer finds its row.
    assert!(matches!(
        RoomOccupantRepository::lookup(&mut tx, &room(), &occupant()).await.expect("lookup"),
        RoomOccupantLookup::Seated(row) if row.presence.is_none() && row.generation == second
    ));
    assert!(
        !RoomOccupantRepository::confirm_join(&mut tx, &record(first, None))
            .await
            .expect("stale confirm")
    );
    assert!(
        RoomOccupantRepository::confirm_join(&mut tx, &record(second, None))
            .await
            .expect("confirm rejoin")
    );
    // A join of the same connection after it left is another occupancy: its
    // seat has another identity, and the earlier occupancy's presence and
    // join request do not survive it. Recording the same occupancy again
    // (a role change) keeps both.
    let seat_of = |identity| RoomOccupantRecord {
        seat: Some(identity),
        ..record(second, None)
    };
    let (earlier, later) = (OccupancySeatId::mint(), OccupancySeatId::mint());
    RoomOccupantRepository::record_seat(&mut tx, &seat_of(earlier))
        .await
        .expect("seat with an identity");
    assert!(
        RoomOccupantRepository::confirm_join(&mut tx, &record(second, None))
            .await
            .expect("confirm the identified seat")
    );
    RoomOccupantRepository::record_seat(&mut tx, &seat_of(earlier))
        .await
        .expect("same occupancy recorded again");
    assert!(matches!(
        RoomOccupantRepository::lookup(&mut tx, &room(), &occupant()).await.expect("lookup"),
        RoomOccupantLookup::Seated(row) if row.presence.is_some() && row.seat == Some(earlier)
    ));
    RoomOccupantRepository::record_seat(&mut tx, &seat_of(later))
        .await
        .expect("same connection joins again");
    assert!(matches!(
        RoomOccupantRepository::lookup(&mut tx, &room(), &occupant()).await.expect("lookup"),
        RoomOccupantLookup::Seated(row)
            if row.presence.is_none() && row.generation == second && row.seat == Some(later)
    ));
    assert!(
        RoomOccupantRepository::confirm_join(&mut tx, &record(second, None))
            .await
            .expect("confirm the later occupancy")
    );
    tx.commit().await.expect("owner commits");

    // A second resource whose row cannot be parsed is counted, not seated.
    fixture
        .execute(
            "INSERT INTO {schema}.ingress_room_occupants (room_jid, occupant_jid, nick, role, \
             affiliation, occupancy_generation, room_epoch) \
             VALUES ('room@conference.example.com', 'romeo@example.com/orchard', 'romeo', 2, 2, \
             'not-a-generation', 5)",
        )
        .await
        .expect("unparseable row");

    let mut tx = fixture.uow.begin().await.expect("begin successor");
    let load = RoomOccupantRepository::load_room(&mut tx, &room())
        .await
        .expect("load room");
    assert_eq!(load.unparseable, 1);
    assert_eq!(load.seated.len(), 1);
    let seated = &load.seated[0];
    assert_eq!(seated.occupant, occupant());
    assert_eq!(seated.nick, "juliet");
    assert_eq!(seated.role, Role::Participant);
    assert_eq!(seated.affiliation, Affiliation::Member);
    assert_eq!(seated.generation, second);
    assert_eq!(
        seated.seat,
        Some(later),
        "a successor recovers the occupancy's identity"
    );
    assert!(seated.presence.is_some());
    let romeo: FullJid = "romeo@example.com/orchard".parse().expect("romeo");
    assert!(matches!(
        RoomOccupantRepository::lookup(&mut tx, &room(), &romeo)
            .await
            .expect("lookup unparseable"),
        RoomOccupantLookup::Unparseable
    ));
    // The owner's own removal ignores the generation.
    assert!(
        RoomOccupantRepository::remove(&mut tx, &room(), &romeo, None)
            .await
            .expect("remove ghost")
    );
    assert!(
        RoomOccupantRepository::remove(&mut tx, &room(), &occupant(), Some(second))
            .await
            .expect("leave")
    );
    tx.commit().await.expect("successor commits");
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_room_occupants")
            .await,
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn collector_takes_only_consumed_requests_settled_for_the_retention() {
    let Some(fixture) = Fixture::open("retention").await else {
        return;
    };
    let presented = fence(3, 5, "node-a");
    let first = generation(1);
    let identity = lane(first, MailboxRoomLane::MucStanza);
    let settled = join(first);
    let unsettled = request_with_kind(RoomRequestKind::Presence, first);
    let undecided = request_with_kind(RoomRequestKind::Leave, first);
    for request in [&settled, &unsettled, &undecided] {
        commit_request(&fixture.uow, &identity, &presented, request)
            .await
            .expect("origin commit");
    }
    let response = [Stanza::Presence(presence_to("juliet", "self"))];
    decide_request(
        &fixture,
        &settled,
        MailboxOwnerDecision::Accepted,
        &response,
    )
    .await;
    decide_request(
        &fixture,
        &unsettled,
        MailboxOwnerDecision::Accepted,
        &response,
    )
    .await;
    consume_request(&fixture, &settled).await;

    // Settled a moment ago: inside the retention, nothing is collected.
    let mut tx = fixture.uow.begin().await.expect("begin early collect");
    assert_eq!(
        RoomRequestRepository::collect_settled(&mut tx, ROOM_REQUEST_SETTLED_RETENTION, 16)
            .await
            .expect("early collect"),
        0
    );
    assert_eq!(
        RoomRequestRepository::count_unsettled_decisions(&mut tx, Duration::ZERO)
            .await
            .expect("count unsettled"),
        1
    );
    // With the retention elapsed only the consumed, settled request leaves,
    // taking its entry and responses and clearing the occupant's reference.
    assert_eq!(
        RoomRequestRepository::collect_settled(&mut tx, Duration::ZERO, 16)
            .await
            .expect("collect"),
        1
    );
    tx.commit().await.expect("commit collect");
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_room_requests")
            .await,
        2
    );
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_mailbox_entries")
            .await,
        2
    );
    assert_eq!(
        fixture
            .scalar("SELECT count(*) FROM {schema}.ingress_room_responses")
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_room_occupants WHERE join_request IS NULL"
            )
            .await,
        1
    );
    // The lane keeps its counters; collected entries leave no gap to reuse.
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_mailbox_lanes \
                 WHERE last_sequence = 3 AND last_dispositioned = 2"
            )
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn guarded_room_tables_reject_unproven_writes() {
    let Some(fixture) = Fixture::open("epoch").await else {
        return;
    };
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM {schema}.ingress_epoch_guard_manifest \
                 WHERE table_name IN ('ingress_room_requests', 'ingress_room_responses', \
                 'ingress_room_occupants')"
            )
            .await,
        3
    );
    assert_eq!(
        fixture
            .scalar(
                "SELECT count(*) FROM pg_catalog.pg_trigger t \
                 JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = '{schema}' AND NOT t.tgisinternal AND t.tgenabled = 'A' \
                   AND c.relname IN ('ingress_room_requests', 'ingress_room_responses', \
                   'ingress_room_occupants') AND t.tgtype IN (30, 34)"
            )
            .await,
        6
    );

    fixture.advance_epoch().await;
    for statement in [
        "INSERT INTO {schema}.ingress_room_occupants (room_jid, occupant_jid, nick, role, \
         affiliation, occupancy_generation, room_epoch) \
         VALUES ('r@m', 'a@b/c', 'n', 2, 2, 'g', 1)",
        "UPDATE {schema}.ingress_room_requests SET owner_decision = 1",
        "DELETE FROM {schema}.ingress_room_responses",
        "TRUNCATE {schema}.ingress_room_occupants",
        "TRUNCATE {schema}.ingress_room_responses",
        "TRUNCATE {schema}.ingress_room_requests CASCADE",
    ] {
        assert!(
            fixture.execute(statement).await.is_err(),
            "unproven statement must be refused: {statement}"
        );
    }

    // The typed entry proves the live epoch, so repository writes succeed.
    let first = generation(1);
    let request = join(first);
    let appended = commit_request(
        &fixture.uow,
        &lane(first, MailboxRoomLane::MujiSignaling),
        &fence(3, 5, "node-a"),
        &request,
    )
    .await
    .expect("proven commit at the activated epoch");
    assert!(matches!(appended, MailboxAppendOutcome::Appended(_)));
    fixture.close().await;
}
