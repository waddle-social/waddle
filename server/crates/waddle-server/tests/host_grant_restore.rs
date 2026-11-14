//! Restoring the canonical host-grant store (ADR-0020 D5 condition B7, gate
//! G13): the rows the restore slice owns in the host-grant design's
//! acceptance matrix.
//!
//! A restore of PostgreSQL brings back older grants, an older generation and
//! the `active` flag of every scope. These cases show that such a restore
//! cannot reopen authority that a later revoke or ban closed:
//!
//! - an undeclared restore is found by the fence a replica passes before it
//!   admits a managed room: the epoch, the lineage and the PostgreSQL
//!   position refuse the whole authority, and a projection marker or journal
//!   witness the restored journal does not record closes its scope;
//! - a declared restore closes every scope under a new incarnation, with no
//!   standing an initiator could use, so old commands, old activations, old
//!   projection passes and replicas attached before it admit nothing;
//! - a closed scope is reconstructed from the canonical rows, keeps the
//!   revokes and bans the projection shows the restore lost, grants nothing
//!   from the projection, and admits again only after an operator reopens
//!   it; an operator's revoke applies what SpiceDB cannot show;
//! - a replica that finds the authority changed verifies the fence again in
//!   place, and reports itself not ready while that fails.
//!
//! A restore is simulated by copying every host-grant relation into a
//! schema of its own (the backup) and later rewriting the relations from
//! the copy in one transaction (the restore). The rest of the database and
//! its lineage stay as they are, as after a restore within the same cluster.
//! The simulation does not restart PostgreSQL or move its WAL position back,
//! so a changed server start is stood in for by an attachment bound to
//! another one, and a regressed position by a position witness written
//! ahead of the server.
//!
//! The cases without SpiceDB need the PostgreSQL test cluster; where they
//! verify the restore fence they use the local permission backend, which
//! holds no projection and no witness. With `WADDLE_TEST_REQUIRE_POSTGRES=1`
//! they fail rather than skip when the cluster is not configured. The
//! `live_spicedb_` cases are `#[ignore]`d like the other live suites: they
//! need `WADDLE_TEST_SPICEDB_ENDPOINT`, `WADDLE_TEST_SPICEDB_PRESHARED_KEY`
//! and `WADDLE_TEST_SPICEDB_DISPOSABLE=1` as well as the PostgreSQL test
//! cluster, and each empties the SpiceDB store first.
#![cfg(feature = "test-support")]

mod host_grant_support;
mod ingress_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use host_grant_support::{
    room_submission, served, subject, GrantFixture, OWNER, PEER, PEER_RESOURCE, SENDER,
};
use ingress_support::IngressFixture;
use kameo::actor::{ActorRef, Spawn};
use prescience::{
    Client, Consistency, ObjectReference, Relationship, RelationshipFilter, RelationshipUpdate,
    SubjectReference,
};
use tokio::sync::Mutex;
use uuid::Uuid;
use waddle_server::config::SpiceDbConfig;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress::IngressDecisionClass;
use waddle_server::permissions::host_grant::{
    head_witness_resource, position_standing, position_witness_resource, reconstruction_targets,
    room_witness_digest, verify_restore_fence, with_host_grant_commit_fault, Activation,
    AdmissionState, AuthorityIncarnation, ClosedReason, GateState, GrantGeneration,
    GrantOperationId, HostGrantAction, HostGrantAttachment, HostGrantAuthority,
    HostGrantCommitFault, HostGrantError, HostGrantStore, MutationReceipt, OperationKind,
    OperationOutcome, OperatorRevoke, PositionStanding, PositionWitness, PostgresPosition,
    ProjectedGrant, ProjectedRelationship, ProjectionContradiction, ProjectionIntent,
    ProjectionMarker, ProjectionWrite, Reconciliation, ReconstructionTarget, ReopenReport,
    RestoreDeclaration, RestoreEpoch, RestoreFenceRefusal, RetireScope, RoomAffiliation,
    ScopeLifecycleId, HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    guarded_resource_definitions, CheckPermission, EnsureAuthzCodecIncarnation, EnsureSchema,
    Object, ObjectType, Permission, PermissionActor, ReadHostGrantWitness,
    RecordAuthzSchemaVersion, Relation, ReplaceHostGrantWitness, Subject, Tuple,
    AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_DEFINITION,
};
use waddle_server::test_support::{activate_host_grant_marker, spicedb_wire};
use waddle_server::ServerConfig;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const CAROL: &str = "carol@example.com";
const DAVE: &str = "dave@example.com";
const EVE: &str = "eve@example.com";

const SERVICE_UNAVAILABLE: (ErrorType, DefinedCondition) =
    (ErrorType::Cancel, DefinedCondition::ServiceUnavailable);

/// Every relation of the canonical host-grant store: what a restore of the
/// database brings back.
const AUTHORITY_RELATIONS: [&str; 7] = [
    "host_grant_authority",
    "host_grant_scope_lifecycles",
    "host_grant_scopes",
    "host_grant_rooms",
    "host_grants",
    "host_grant_operations",
    "host_grant_operation_changes",
];

// Live tests share one SpiceDB with the other `spicedb-live` binaries, which
// empty it, so they run one at a time: this lock serializes them within the
// binary, and the nextest group serializes them across binaries.
static LIVE_SPICEDB: Mutex<()> = Mutex::const_new(());

/// A copy of the canonical host-grant store, standing in for a backup.
struct AuthorityBackup {
    schema: String,
}

impl AuthorityBackup {
    /// Copy every host-grant relation into a schema of its own, in one
    /// transaction.
    async fn take(fixture: &GrantFixture) -> Self {
        let schema = format!("backup_{}", Uuid::new_v4().simple());
        let pool = owner_pool(fixture).await;
        let mut tx = pool.begin().await.expect("backup transaction");
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .expect("snapshot isolation");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut *tx)
            .await
            .expect("backup schema");
        for relation in AUTHORITY_RELATIONS {
            sqlx::query(&format!(
                "CREATE TABLE {schema}.{relation} AS SELECT * FROM public.{relation}"
            ))
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|error| panic!("back up {relation}: {error}"));
        }
        tx.commit().await.expect("backup commit");
        pool.close().await;
        Self { schema }
    }

    /// Rewrite every host-grant relation from the copy in one transaction,
    /// as a restore of the database to the point of the copy leaves them.
    async fn restore(&self, fixture: &GrantFixture) {
        let pool = owner_pool(fixture).await;
        let mut tx = pool.begin().await.expect("restore transaction");
        for relation in AUTHORITY_RELATIONS {
            sqlx::query(&format!("DELETE FROM public.{relation}"))
                .execute(&mut *tx)
                .await
                .unwrap_or_else(|error| panic!("clear {relation}: {error}"));
            sqlx::query(&format!(
                "INSERT INTO public.{relation} SELECT * FROM {}.{relation}",
                self.schema
            ))
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|error| panic!("restore {relation}: {error}"));
        }
        tx.commit().await.expect("restore commit");
        pool.close().await;
    }
}

/// A raw pool on the fixture's database under its owner login.
async fn owner_pool(fixture: &GrantFixture) -> sqlx::PgPool {
    sqlx::PgPool::connect(fixture.ingress.db.database_url())
        .await
        .expect("owner connection")
}

/// A permission actor on the local backend, which holds no projection: the
/// restore fence then compares only the epoch and the lineage.
fn local_permissions(fixture: &GrantFixture) -> ActorRef<PermissionActor> {
    PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
        fixture.ingress.db.clone(),
    )))
}

async fn sql_admits(fixture: &GrantFixture, account: &str, action: HostGrantAction) -> bool {
    fixture
        .store
        .candidate_admission(&fixture.scope.room, &subject(account), action, &served())
        .await
        .expect("candidate admission")
        .admitted
}

async fn declare(fixture: &GrantFixture, epoch: u32) -> RestoreDeclaration {
    fixture
        .store
        .prepare_restore_incarnation(RestoreEpoch::new(epoch))
        .await
        .expect("restore declaration")
}

/// An operator's revoke (or ban) of `account` in the fixture's first room.
async fn operator_revoke(
    fixture: &GrantFixture,
    account: &str,
    ban: bool,
) -> Result<MutationReceipt, HostGrantError> {
    fixture
        .store
        .operator_revoke(
            &OperatorRevoke {
                operation: GrantOperationId::generate(),
                lifecycle: fixture.scope.lifecycle,
                room: fixture.scope.room.clone(),
                subject: subject(account),
                ban,
            },
            &served(),
        )
        .await
}

async fn reopen(fixture: &GrantFixture, lifecycle: ScopeLifecycleId) -> ReopenReport {
    fixture
        .store
        .reopen_scopes(Some(lifecycle))
        .await
        .expect("reopen")
}

fn epoch_mismatch(error: HostGrantError) -> (RestoreEpoch, RestoreEpoch) {
    match error {
        HostGrantError::RestoreFence(RestoreFenceRefusal::EpochMismatch { configured, stored }) => {
            (configured, stored)
        }
        other => panic!("expected an epoch mismatch, got {other:?}"),
    }
}

fn fence_refusal(error: HostGrantError) -> RestoreFenceRefusal {
    match error {
        HostGrantError::RestoreFence(refusal) => refusal,
        other => panic!("expected a restore-fence refusal, got {other:?}"),
    }
}

/// The kind of the operation that recorded a scope's current generation.
async fn current_kind(fixture: &GrantFixture, lifecycle: ScopeLifecycleId) -> OperationKind {
    let state = fixture
        .store
        .scope(lifecycle)
        .await
        .expect("read scope")
        .expect("scope");
    match fixture
        .store
        .operation_outcome(state.operation)
        .await
        .expect("readback")
    {
        OperationOutcome::Committed(record) => record.kind,
        OperationOutcome::Absent => panic!("the current operation is recorded"),
    }
}

fn generation(value: i64) -> GrantGeneration {
    GrantGeneration::new(value).expect("generation")
}

#[test]
fn a_restore_epoch_is_spelled_in_decimal_digits_only() {
    assert_eq!(RestoreEpoch::parse("0"), Some(RestoreEpoch::INITIAL));
    assert_eq!(RestoreEpoch::parse("7"), Some(RestoreEpoch::new(7)));
    assert_eq!(
        RestoreEpoch::parse("4294967295"),
        Some(RestoreEpoch::new(u32::MAX))
    );
    for refused in ["", "-1", "+1", " 1", "1 ", "0x1", "4294967296", "1.0"] {
        assert_eq!(RestoreEpoch::parse(refused), None, "{refused:?}");
    }
    assert!(RestoreEpoch::INITIAL < RestoreEpoch::new(1));
}

#[test]
fn witness_resources_never_take_a_lifecycle_identity() {
    let lifecycle = ScopeLifecycleId::generate();
    let head = head_witness_resource(lifecycle);
    assert_eq!(head, format!("head_{}", lifecycle.to_storage()));
    assert!(ScopeLifecycleId::from_storage(&head).is_none());
    let digest = room_witness_digest("channel-1");
    assert_eq!(digest.len(), 64);
    assert_ne!(digest, room_witness_digest("channel-2"));
    let position = position_witness_resource(&"a".repeat(32));
    assert!(ScopeLifecycleId::from_storage(&position).is_none());
}

#[test]
fn the_position_witness_detects_regressions_and_unverified_timelines() {
    let at = |epoch: u32, system: i64, timeline: u32, lsn: u64| PositionWitness {
        epoch: RestoreEpoch::new(epoch),
        position: PostgresPosition {
            system_identifier: system,
            timeline,
            lsn,
        },
    };
    // Its spelling is canonical, a negative system identifier included.
    for witness in [at(0, 7, 1, 0), at(3, -42, 9, u64::MAX)] {
        assert_eq!(PositionWitness::parse(&witness.stamp()), Some(witness));
    }
    for refused in ["", "0_7_1", "0_7_1_00", "0_7_1_1_1", "x_7_1_1", "0_+7_1_1"] {
        assert_eq!(PositionWitness::parse(refused), None, "{refused:?}");
    }

    let current = at(1, 7, 2, 1_000);
    let standing = |witness: Option<PositionWitness>, acknowledged| {
        position_standing(witness.as_ref(), &current, acknowledged)
    };
    assert_eq!(standing(None, None), PositionStanding::Unrecorded);
    // The same system and timeline: the position only moves forward.
    assert_eq!(
        standing(Some(at(1, 7, 2, 1_000)), None),
        PositionStanding::Current
    );
    assert_eq!(
        standing(Some(at(1, 7, 2, 999)), None),
        PositionStanding::Advanced
    );
    assert_eq!(
        standing(Some(at(1, 7, 2, 1_001)), None),
        PositionStanding::Regressed
    );
    // A declared restore since the witness replaces it; a witness of a
    // later declared epoch belongs to a database the current one predates.
    assert_eq!(
        standing(Some(at(0, 7, 2, 5_000)), None),
        PositionStanding::Rebased
    );
    assert_eq!(
        standing(Some(at(2, 7, 2, 0)), None),
        PositionStanding::Regressed
    );
    // Another timeline: below the witnessed position it cannot contain it;
    // otherwise the switch point decides, which only an operator can read.
    assert_eq!(
        standing(Some(at(1, 7, 1, 2_000)), None),
        PositionStanding::Regressed
    );
    assert_eq!(
        standing(Some(at(1, 7, 1, 500)), None),
        PositionStanding::Unverified
    );
    assert_eq!(
        standing(Some(at(1, 8, 2, 500)), None),
        PositionStanding::Unverified
    );
    assert_eq!(
        standing(Some(at(1, 7, 1, 2_000)), Some((7, 2))),
        PositionStanding::Rebased
    );
    assert_eq!(
        standing(Some(at(1, 8, 2, 500)), Some((7, 2))),
        PositionStanding::Rebased
    );
    // An acknowledgement of another timeline does not count, and none
    // excuses a regression on the witnessed timeline.
    assert_eq!(
        standing(Some(at(1, 7, 1, 500)), Some((7, 1))),
        PositionStanding::Unverified
    );
    assert_eq!(
        standing(Some(at(1, 7, 2, 1_001)), Some((7, 2))),
        PositionStanding::Regressed
    );
}

#[test]
fn reconstruction_lowers_only_and_records_subjects_only_the_projection_held() {
    let lifecycle = ScopeLifecycleId::generate();
    let marker = ProjectionMarker::parse(
        lifecycle,
        &format!(
            "{}_3_{}_{}",
            AuthorityIncarnation::generate().to_storage(),
            GrantOperationId::generate().to_storage(),
            "0".repeat(64)
        ),
    )
    .expect("marker");
    let grant = |subject_jid: &str, affiliation| ProjectedGrant {
        channel_id: "room".to_owned(),
        subject: subject(subject_jid),
        affiliation,
    };
    let intent = ProjectionIntent {
        marker,
        channels: vec!["room".to_owned()],
        grants: vec![
            grant(OWNER, RoomAffiliation::Owner),
            grant(SENDER, RoomAffiliation::Member),
            grant(PEER, RoomAffiliation::Member),
            grant(CAROL, RoomAffiliation::Member),
        ],
    };
    let held = |subject: &str, relation: &str| ProjectedRelationship {
        channel_id: "room".to_owned(),
        relation: relation.to_owned(),
        subject: subject.to_owned(),
    };
    // The projection of another state: the owner as the canonical rows hold
    // it, the sender and Carol holding nothing, the peer banned, and Dave
    // banned and Eve granted, whom the canonical rows do not name.
    let observed: BTreeSet<ProjectedRelationship> = [
        held(OWNER, "owner"),
        held(PEER, "outcast"),
        held(DAVE, "outcast"),
        held(EVE, "member"),
    ]
    .into_iter()
    .collect();
    let target = |subject: &str, affiliation| ReconstructionTarget {
        channel_id: "room".to_owned(),
        subject: subject.to_owned(),
        affiliation,
    };

    // The projection reflects operations the restore lost: every grant is
    // lowered to it, the subjects only it holds are recorded at `outcast` or
    // `none`, and nothing is raised.
    let mut lost = vec![
        target(CAROL, RoomAffiliation::None),
        target(DAVE, RoomAffiliation::Outcast),
        target(EVE, RoomAffiliation::None),
        target(PEER, RoomAffiliation::Outcast),
        target(SENDER, RoomAffiliation::None),
    ];
    lost.sort();
    assert_eq!(reconstruction_targets(&intent, &observed, true), lost);

    // The canonical rows are at least as new: they are kept, and only the
    // subjects the canonical rows do not name are recorded.
    let mut current = vec![
        target(DAVE, RoomAffiliation::Outcast),
        target(EVE, RoomAffiliation::None),
    ];
    current.sort();
    assert_eq!(reconstruction_targets(&intent, &observed, false), current);

    // A subject held at several relations counts at the lowest.
    let doubled: BTreeSet<ProjectedRelationship> = [
        held(OWNER, "owner"),
        held(OWNER, "outcast"),
        held(SENDER, "member"),
        held(PEER, "member"),
        held(CAROL, "member"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        reconstruction_targets(&intent, &doubled, true),
        vec![target(OWNER, RoomAffiliation::Outcast)]
    );
    assert!(reconstruction_targets(&intent, &doubled, false).is_empty());
}

#[tokio::test]
async fn a_declared_restore_closes_every_scope_under_a_new_incarnation() {
    let Some(fixture) = GrantFixture::active("hg_restore_transition").await else {
        return;
    };
    let waiting = fixture.enroll_another().await;
    let retired = fixture.enroll_another().await;
    fixture
        .store
        .retire_scope(&RetireScope {
            operation: GrantOperationId::generate(),
            lifecycle: retired.lifecycle,
            expected_generation: fixture.state(&retired).await.generation,
        })
        .await
        .expect("retirement");
    let active_before = fixture.state(&fixture.scope).await;
    let waiting_before = fixture.state(&waiting).await;
    let retired_before = fixture.state(&retired).await;
    let previous = fixture
        .store
        .authority_incarnation()
        .await
        .expect("incarnation");
    assert!(sql_admits(&fixture, SENDER, HostGrantAction::SendMessage).await);

    // The first owner phase binds the incarnation to the lineage; the same
    // epoch again changes nothing.
    assert_eq!(declare(&fixture, 0).await, RestoreDeclaration::LineageBound);
    assert_eq!(declare(&fixture, 0).await, RestoreDeclaration::Unchanged);
    assert_eq!(
        fixture.store.authority_incarnation().await.expect("read"),
        previous
    );

    // A raised epoch declares a restore.
    let RestoreDeclaration::Transitioned {
        previous: replaced,
        incarnation,
        epoch,
        restored_scopes,
    } = declare(&fixture, 1).await
    else {
        panic!("a raised epoch is a transition");
    };
    assert_eq!(replaced, previous);
    assert_ne!(incarnation, previous);
    assert_eq!(epoch, RestoreEpoch::new(1));
    assert_eq!(restored_scopes, 2);
    for (scope, before) in [
        (&fixture.scope, &active_before),
        (&waiting, &waiting_before),
    ] {
        let after = fixture.state(scope).await;
        assert_eq!(after.authority_incarnation, incarnation);
        assert_eq!(Some(after.generation), before.generation.next());
        assert_eq!(after.admission, AdmissionState::Closed);
        assert_eq!(after.closed_reason, Some(ClosedReason::Restored));
        assert_eq!(after.projected_generation, before.projected_generation);
        assert_eq!(
            current_kind(&fixture, scope.lifecycle).await,
            OperationKind::Restore
        );
    }
    // A retired scope admits nothing either way and is left as it was.
    assert_eq!(fixture.state(&retired).await, retired_before);
    let binding = fixture.store.authority_binding().await.expect("binding");
    assert_eq!(binding.incarnation, incarnation);
    assert_eq!(binding.restore_epoch, RestoreEpoch::new(1));

    // Nothing admits. The recorded grants are kept and no effective standing
    // above `none` is left that an initiator could use.
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    assert_eq!(
        fixture.affiliations(OWNER).await,
        Some((RoomAffiliation::Owner, RoomAffiliation::None))
    );
    for account in [OWNER, SENDER, PEER] {
        assert!(!sql_admits(&fixture, account, HostGrantAction::Read).await);
    }

    // The declared epoch is recorded: a rerun with it changes nothing, and a
    // configuration from before it is refused without a change.
    assert_eq!(declare(&fixture, 1).await, RestoreDeclaration::Unchanged);
    let (configured, stored) = epoch_mismatch(
        fixture
            .store
            .prepare_restore_incarnation(RestoreEpoch::INITIAL)
            .await
            .expect_err("an older epoch"),
    );
    assert_eq!(
        (configured, stored),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    assert_eq!(
        fixture.store.authority_incarnation().await.expect("read"),
        incarnation
    );

    // Both restored scopes wait for reconstruction; the retired one does not.
    let pending = fixture.store.pending_scopes(None).await.expect("pending");
    assert!(pending.contains(&fixture.scope.lifecycle));
    assert!(pending.contains(&waiting.lifecycle));
    assert!(!pending.contains(&retired.lifecycle));
    fixture.close().await;
}

#[tokio::test]
async fn a_transition_that_stopped_is_finished_by_the_next_owner_phase() {
    let Some(fixture) = GrantFixture::active("hg_restore_resume").await else {
        return;
    };
    let other = fixture.enroll_another().await;
    let previous = fixture.store.authority_incarnation().await.expect("read");

    // The new incarnation commits and the owner phase stops before any
    // scope is closed: the reply of the first commit is lost.
    assert!(matches!(
        with_host_grant_commit_fault(
            HostGrantCommitFault::ReplyLostAfterCommit,
            fixture
                .store
                .prepare_restore_incarnation(RestoreEpoch::new(1)),
        )
        .await,
        Err(HostGrantError::CommitOutcomeUnknown)
    ));
    let incarnation = fixture.store.authority_incarnation().await.expect("read");
    assert_ne!(incarnation, previous);
    // A scope still bound to the earlier incarnation admits nothing and
    // takes no mutation.
    let state = fixture.state(&fixture.scope).await;
    assert_eq!(state.authority_incarnation, previous);
    assert!(!sql_admits(&fixture, SENDER, HostGrantAction::Read).await);
    assert!(matches!(
        fixture.set(OWNER, PEER, RoomAffiliation::None).await,
        Err(HostGrantError::IncarnationChanged)
    ));

    // The rerun with the declared epoch closes what is left.
    assert_eq!(
        declare(&fixture, 1).await,
        RestoreDeclaration::Resumed { restored_scopes: 2 }
    );
    for scope in [&fixture.scope, &other] {
        let state = fixture.state(scope).await;
        assert_eq!(state.authority_incarnation, incarnation);
        assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    }
    assert_eq!(declare(&fixture, 1).await, RestoreDeclaration::Unchanged);
    fixture.close().await;
}

#[tokio::test]
async fn work_from_before_a_declared_restore_admits_nothing() {
    let Some(fixture) = GrantFixture::active("hg_restore_old_work").await else {
        return;
    };
    let lifecycle = fixture.scope.lifecycle;
    let permissions = local_permissions(&fixture);
    // A replica attached before the restore, and work prepared then: a
    // command decided at the current generation and the plan of a pass.
    let early = fixture.ingress.authority().await;
    assert!(early.attach_host_grants(
        HostGrantAttachment::fenced_for_test(
            &fixture.store,
            &permissions,
            served(),
            RestoreEpoch::INITIAL
        )
        .await
        .expect("attachment before the restore"),
    ));
    let before = fixture.room_submission("restore-before", "admitted before");
    assert_eq!(
        early.commit(&before).await.class,
        IngressDecisionClass::Accepted
    );
    let stale_command = fixture.command(OWNER, CAROL, RoomAffiliation::Member).await;
    let stale_plan = fixture
        .store
        .projection_plan(lifecycle)
        .await
        .expect("plan");

    assert!(matches!(
        declare(&fixture, 1).await,
        RestoreDeclaration::Transitioned { .. }
    ));

    // The command is stale and the activation is superseded: neither
    // changes anything.
    assert!(matches!(
        fixture
            .store
            .set_affiliation(&stale_command, &fixture.policy)
            .await,
        Err(HostGrantError::StaleGeneration { .. })
    ));
    assert_eq!(
        activate_host_grant_marker(&fixture.store, stale_plan.marker())
            .await
            .expect("activation"),
        Activation::Superseded
    );
    assert_eq!(
        fixture.state(&fixture.scope).await.closed_reason,
        Some(ClosedReason::Restored)
    );

    // The early replica reads another incarnation than it verified and
    // admits nothing, its own members included.
    let early_refusal = fixture.room_submission("restore-early", "not admitted");
    let decision = early.commit(&early_refusal).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);

    // A replica configured for the earlier epoch cannot attach. One
    // configured for the declared epoch attaches and finds the scope closed.
    assert_eq!(
        epoch_mismatch(
            HostGrantAttachment::fenced_for_test(
                &fixture.store,
                &permissions,
                served(),
                RestoreEpoch::INITIAL
            )
            .await
            .expect_err("an older epoch")
        ),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    let fresh = fixture.ingress.authority().await;
    assert!(fresh.attach_host_grants(
        HostGrantAttachment::fenced_for_test(
            &fixture.store,
            &permissions,
            served(),
            RestoreEpoch::new(1)
        )
        .await
        .expect("attachment after the restore"),
    ));
    let closed = fixture.room_submission("restore-closed", "not yet");
    let decision = fresh.commit(&closed).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);

    // While the restore is handled no initiator's standing is honoured: the
    // owner the restored rows name cannot change anything, a revoke
    // included. An operator's revoke takes effect at its commit.
    let before_revoke = fixture.state(&fixture.scope).await;
    assert!(matches!(
        fixture.set(OWNER, PEER, RoomAffiliation::None).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert!(matches!(
        fixture.set(OWNER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));
    assert_eq!(fixture.state(&fixture.scope).await, before_revoke);
    operator_revoke(&fixture, PEER, false)
        .await
        .expect("operator revoke while restored");
    assert_eq!(
        fixture.affiliations(PEER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );

    // The restored scope's projection is verified (here in place of a
    // reconstruction): it is `reconstructed`, and still admits nothing.
    let plan = fixture
        .store
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    assert!(plan.is_restored());
    assert_ne!(plan.marker().incarnation, stale_plan.marker().incarnation);
    assert_eq!(
        activate_host_grant_marker(&fixture.store, plan.marker())
            .await
            .expect("activation"),
        Activation::Reconstructed
    );
    let state = fixture.state(&fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Reconstructed));
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    let waiting = fixture.room_submission("restore-reconstructed", "not yet either");
    assert_eq!(
        fresh.commit(&waiting).await.class,
        IngressDecisionClass::PolicyDenied
    );
    assert!(matches!(
        fixture.set(OWNER, CAROL, RoomAffiliation::Member).await,
        Err(HostGrantError::InitiatorNotAuthorized)
    ));

    // An operator reopens it: the fresh replica admits and the early one
    // does not.
    assert_eq!(
        reopen(&fixture, lifecycle).await,
        ReopenReport {
            reopened: 1,
            not_reconstructed: 0
        }
    );
    assert_eq!(
        current_kind(&fixture, lifecycle).await,
        OperationKind::Reopen
    );
    let reopened = fixture.room_submission("restore-reopened", "admitted again");
    assert_eq!(
        fresh.commit(&reopened).await.class,
        IngressDecisionClass::Accepted
    );
    let still_early = fixture.room_submission("restore-still-early", "still not");
    assert_eq!(
        early.commit(&still_early).await.class,
        IngressDecisionClass::PolicyDenied
    );
    assert!(!sql_admits(&fixture, PEER, HostGrantAction::Read).await);
    // The owner's standing is honoured again.
    fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant after the reopen");
    assert!(early.drain_and_join(Duration::from_secs(5)).await);
    assert!(fresh.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

#[tokio::test]
async fn a_restored_scope_reopens_only_once_its_reconstruction_is_verified() {
    let Some(fixture) = GrantFixture::active("hg_restore_reopen").await else {
        return;
    };
    let lifecycle = fixture.scope.lifecycle;
    let never_activated = fixture.enroll_another().await;
    declare(&fixture, 1).await;

    // Nothing is reconstructed yet: an operator's reopen leaves every scope
    // closed, named or not.
    assert_eq!(
        reopen(&fixture, lifecycle).await,
        ReopenReport {
            reopened: 0,
            not_reconstructed: 1
        }
    );
    assert_eq!(
        fixture.store.reopen_scopes(None).await.expect("reopen all"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 2
        }
    );
    assert!(matches!(
        fixture
            .store
            .reopen_scopes(Some(ScopeLifecycleId::generate()))
            .await,
        Err(HostGrantError::UnknownScope)
    ));

    // One scope is reconstructed; a mutation after it (an operator's revoke)
    // leaves its latest operation unverified, and it is not reopened until
    // that is verified as well.
    let plan = fixture
        .store
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    assert_eq!(
        activate_host_grant_marker(&fixture.store, plan.marker())
            .await
            .expect("activation"),
        Activation::Reconstructed
    );
    operator_revoke(&fixture, PEER, true)
        .await
        .expect("operator ban");
    assert_eq!(
        fixture.store.reopen_scopes(None).await.expect("reopen all"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 2
        }
    );
    let plan = fixture
        .store
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    assert!(!plan.is_restored());
    assert_eq!(
        activate_host_grant_marker(&fixture.store, plan.marker())
            .await
            .expect("activation"),
        Activation::Reconstructed
    );
    let before = fixture.state(&fixture.scope).await;
    assert_eq!(
        fixture.store.reopen_scopes(None).await.expect("reopen all"),
        ReopenReport {
            reopened: 1,
            not_reconstructed: 1
        }
    );
    let after = fixture.state(&fixture.scope).await;
    assert_eq!(after.admission, AdmissionState::Active);
    assert_eq!(Some(after.generation), before.generation.next());
    assert_eq!(after.projected_generation, before.projected_generation);
    assert!(sql_admits(&fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(!sql_admits(&fixture, PEER, HostGrantAction::Read).await);
    // The reopen's own operation waits for its projection like any other.
    assert!(fixture
        .store
        .pending_scopes(None)
        .await
        .expect("pending")
        .contains(&lifecycle));
    assert_eq!(
        fixture.state(&never_activated).await.closed_reason,
        Some(ClosedReason::Restored)
    );
    fixture.close().await;
}

#[tokio::test]
async fn an_operator_revoke_never_raises_and_keeps_an_owner() {
    let Some(fixture) = GrantFixture::active("hg_restore_operator").await else {
        return;
    };
    declare(&fixture, 1).await;

    // A ban of an account without a row records one; a later revoke does
    // not lift it.
    operator_revoke(&fixture, DAVE, true)
        .await
        .expect("ban without a row");
    assert_eq!(
        fixture.affiliations(DAVE).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    operator_revoke(&fixture, DAVE, false)
        .await
        .expect("revoke of a banned account");
    assert_eq!(
        fixture.affiliations(DAVE).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );

    // The room keeps a recorded owner of a served account.
    assert!(matches!(
        operator_revoke(&fixture, OWNER, false).await,
        Err(HostGrantError::LastOwner)
    ));

    // An exact retry returns the recorded outcome; the same identity with
    // other input is a conflict.
    let command = OperatorRevoke {
        operation: GrantOperationId::generate(),
        lifecycle: fixture.scope.lifecycle,
        room: fixture.scope.room.clone(),
        subject: subject(SENDER),
        ban: false,
    };
    let recorded = fixture
        .store
        .operator_revoke(&command, &served())
        .await
        .expect("revoke");
    assert!(matches!(recorded, MutationReceipt::Recorded(_)));
    assert_eq!(
        fixture
            .store
            .operator_revoke(&command, &served())
            .await
            .expect("retry"),
        MutationReceipt::Replayed(recorded.record().clone())
    );
    assert!(matches!(
        fixture
            .store
            .operator_revoke(
                &OperatorRevoke {
                    ban: true,
                    ..command
                },
                &served()
            )
            .await,
        Err(HostGrantError::OperationConflict)
    ));

    // A retired scope takes none.
    fixture
        .store
        .retire_scope(&RetireScope {
            operation: GrantOperationId::generate(),
            lifecycle: fixture.scope.lifecycle,
            expected_generation: fixture.generation().await,
        })
        .await
        .expect("retirement");
    assert!(matches!(
        operator_revoke(&fixture, PEER, false).await,
        Err(HostGrantError::ScopeRetired)
    ));
    fixture.close().await;
}

#[tokio::test]
async fn a_replica_verifies_a_changed_authority_in_place() {
    let Some(fixture) = GrantFixture::active("hg_restore_reverify").await else {
        return;
    };
    let permissions = local_permissions(&fixture);
    let replica = fixture.ingress.authority().await;
    let gate = replica.host_grant_gate();
    assert_eq!(gate.state(), GateState::Unattached);
    assert!(gate.ready());
    gate.install_verifier_for_test(
        fixture.store.clone(),
        permissions.clone(),
        served(),
        RestoreEpoch::INITIAL,
    );

    // The replica attached to a PostgreSQL server that has since restarted:
    // its binding names another server start.
    let mut stale = fixture.store.authority_binding().await.expect("binding");
    stale.server_started_at_us -= 1;
    assert!(
        replica.attach_host_grants(HostGrantAttachment::with_binding_for_test(
            served(),
            stale.clone()
        ))
    );
    assert_eq!(gate.state(), GateState::Attached);

    // The first acceptance after the change is refused and starts one
    // verification in place, which binds the authority as it is now.
    let refused = fixture.room_submission("reverify-refused", "refused once");
    let decision = replica.commit(&refused).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    gate.settled().await;
    assert_eq!(gate.state(), GateState::Attached);
    assert!(gate.ready());
    let current = gate.current().expect("attachment");
    assert_ne!(current.binding(), &stale);
    assert_eq!(
        current.binding(),
        &fixture.store.authority_binding().await.expect("binding")
    );
    let admitted = fixture.room_submission("reverify-admitted", "admitted again");
    assert_eq!(
        replica.commit(&admitted).await.class,
        IngressDecisionClass::Accepted
    );

    // A logical restore recreates the host-grant relations without a
    // restart of PostgreSQL: the authority relation's OID changes, and the
    // replica verifies again.
    let before = fixture.store.authority_binding().await.expect("binding");
    let pool = owner_pool(&fixture).await;
    for statement in [
        "CREATE TABLE host_grant_authority_next (LIKE host_grant_authority INCLUDING ALL)",
        "INSERT INTO host_grant_authority_next SELECT * FROM host_grant_authority",
        "DROP TABLE host_grant_authority",
        "ALTER TABLE host_grant_authority_next RENAME TO host_grant_authority",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    pool.close().await;
    let after = fixture.store.authority_binding().await.expect("binding");
    assert_ne!(after.authority_relation_oid, before.authority_relation_oid);
    assert_eq!(after.incarnation, before.incarnation);
    let refused = fixture.room_submission("reverify-recreated", "refused once");
    assert_eq!(
        replica.commit(&refused).await.class,
        IngressDecisionClass::PolicyDenied
    );
    gate.settled().await;
    assert_eq!(gate.current().expect("attachment").binding(), &after);
    let admitted = fixture.room_submission("reverify-recreated-admitted", "admitted");
    assert_eq!(
        replica.commit(&admitted).await.class,
        IngressDecisionClass::Accepted
    );

    // A restore is declared under the running replica, whose configuration
    // still names the earlier epoch: the verification fails, every managed
    // room stays refused and the replica reports itself not ready.
    declare(&fixture, 1).await;
    let refused = fixture.room_submission("reverify-declared", "refused");
    assert_eq!(
        replica.commit(&refused).await.class,
        IngressDecisionClass::PolicyDenied
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while gate.state() != GateState::ReverifyFailed {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the verification after the declared restore did not fail"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!gate.ready());
    let still = fixture.room_submission("reverify-still", "still refused");
    assert_eq!(
        replica.commit(&still).await.class,
        IngressDecisionClass::PolicyDenied
    );
    // A room no scope manages is not affected.
    let unmanaged = room_submission(
        &fixture.ingress,
        &host_grant_support::bare("open@muc.example.com"),
        "reverify-unmanaged",
        "local traffic",
        &[PEER_RESOURCE],
    );
    assert_eq!(
        replica.commit(&unmanaged).await.class,
        IngressDecisionClass::Accepted
    );
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);

    // A replica whose first verification is refused stays unattached and
    // ready: it never admitted a managed room.
    let starting = fixture.ingress.authority().await;
    let gate = starting.host_grant_gate();
    gate.install_verifier_for_test(
        fixture.store.clone(),
        permissions,
        served(),
        RestoreEpoch::INITIAL,
    );
    assert_eq!(
        epoch_mismatch(gate.verify_once().await.expect_err("an older epoch")),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    assert_eq!(gate.state(), GateState::Unattached);
    assert!(gate.ready());
    assert!(starting.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

#[tokio::test]
async fn an_attached_replica_runs_the_fence_again_periodically() {
    let Some(fixture) = GrantFixture::active("hg_restore_watch").await else {
        return;
    };
    let replica = Arc::new(fixture.ingress.authority().await);
    let gate = replica.host_grant_gate();
    gate.install_verifier_for_test(
        fixture.store.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::INITIAL,
    );
    gate.verify_once().await.expect("first verification");
    gate.watch(Duration::from_millis(100));
    let admitted = fixture.room_submission("watch-admitted", "admitted");
    assert_eq!(
        replica.commit(&admitted).await.class,
        IngressDecisionClass::Accepted
    );

    // No acceptance reaches the replica while a restore is declared under
    // it; the periodic pass finds the authority refused, drops the
    // attachment and reports the replica not ready.
    declare(&fixture, 1).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while gate.state() != GateState::ReverifyFailed {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the periodic fence did not refuse the declared restore"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gate.current().is_none());
    assert!(!gate.ready());
    let refused = fixture.room_submission("watch-refused", "refused");
    let decision = replica.commit(&refused).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

#[tokio::test]
async fn an_adopted_database_is_not_served_until_its_restore_is_declared() {
    let Some(fixture) = GrantFixture::active("hg_restore_lineage").await else {
        return;
    };
    let permissions = local_permissions(&fixture);
    assert_eq!(declare(&fixture, 0).await, RestoreDeclaration::LineageBound);
    let report = verify_restore_fence(&fixture.store, &permissions, RestoreEpoch::INITIAL)
        .await
        .expect("fence of the bound lineage");
    assert_eq!(report.markers, 0);
    assert_eq!(
        report.binding,
        Some(fixture.store.authority_binding().await.expect("binding"))
    );

    // A restore into another database is adopted, which rotates the lineage.
    let pool = owner_pool(&fixture).await;
    sqlx::query("UPDATE _lineage SET lineage_uuid = $1 WHERE id = 1")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .expect("adoption");
    pool.close().await;
    let incarnation = fixture
        .store
        .authority_incarnation()
        .await
        .expect("incarnation");

    // Neither a replica nor the owner phase serves it as it is.
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&fixture.store, &permissions, RestoreEpoch::INITIAL)
                .await
                .expect_err("an adopted lineage")
        ),
        RestoreFenceRefusal::LineageChanged
    );
    assert_eq!(
        fence_refusal(
            fixture
                .store
                .prepare_restore_incarnation(RestoreEpoch::INITIAL)
                .await
                .expect_err("an undeclared restore")
        ),
        RestoreFenceRefusal::LineageChanged
    );
    assert_eq!(
        fixture.store.authority_incarnation().await.expect("read"),
        incarnation
    );

    // Declared, it is bound to the new lineage under a new incarnation, and
    // only a replica configured for the declared epoch serves it.
    assert!(matches!(
        declare(&fixture, 1).await,
        RestoreDeclaration::Transitioned {
            restored_scopes: 1,
            ..
        }
    ));
    let report = verify_restore_fence(&fixture.store, &permissions, RestoreEpoch::new(1))
        .await
        .expect("fence after the declaration");
    assert_eq!(report.awaiting_reconstruction, 1);
    assert_eq!(
        epoch_mismatch(
            verify_restore_fence(&fixture.store, &permissions, RestoreEpoch::INITIAL)
                .await
                .expect_err("an older epoch")
        ),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_database_without_a_scope_is_not_refused() {
    let Some(ingress) = IngressFixture::postgres("hg_restore_no_scope").await else {
        return;
    };
    let store = HostGrantStore::open(ingress.db.clone()).expect("host-grant store");
    let permissions =
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(ingress.db.clone())));
    assert_eq!(
        store
            .prepare_restore_incarnation(RestoreEpoch::INITIAL)
            .await
            .expect("first owner phase"),
        RestoreDeclaration::LineageBound
    );

    // With no scope and no projection marker there is no authority a
    // restore could reopen: any configured epoch starts, and an adopted
    // lineage is bound again rather than refused.
    for configured in [RestoreEpoch::INITIAL, RestoreEpoch::new(5)] {
        let report = verify_restore_fence(&store, &permissions, configured)
            .await
            .expect("fence without a scope");
        assert!(report.binding.is_some());
        assert_eq!((report.markers, report.fenced, report.orphaned), (0, 0, 0));
    }
    let pool = sqlx::PgPool::connect(ingress.db.database_url())
        .await
        .expect("owner connection");
    sqlx::query("UPDATE _lineage SET lineage_uuid = $1 WHERE id = 1")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .expect("adoption");
    pool.close().await;
    verify_restore_fence(&store, &permissions, RestoreEpoch::INITIAL)
        .await
        .expect("fence of an adopted database without a scope");
    assert_eq!(
        store
            .prepare_restore_incarnation(RestoreEpoch::INITIAL)
            .await
            .expect("owner phase after the adoption"),
        RestoreDeclaration::LineageBound
    );
    // The epoch still never decreases.
    assert!(matches!(
        store
            .prepare_restore_incarnation(RestoreEpoch::new(2))
            .await,
        Ok(RestoreDeclaration::Transitioned {
            restored_scopes: 0,
            ..
        })
    ));
    assert_eq!(
        epoch_mismatch(
            store
                .prepare_restore_incarnation(RestoreEpoch::new(1))
                .await
                .expect_err("a lowered epoch")
        ),
        (RestoreEpoch::new(1), RestoreEpoch::new(2))
    );
    drop(store);
    ingress.close().await;
}

#[tokio::test]
async fn the_runtime_role_reads_the_position_and_cannot_declare_a_restore() {
    let Some(fixture) = GrantFixture::active("hg_restore_runtime").await else {
        return;
    };
    let admin = owner_pool(&fixture).await;
    let role = format!("hg_restore_runtime_{}", Uuid::new_v4().simple());
    let password = Uuid::new_v4().simple().to_string();
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&admin)
        .await
        .expect("database name");
    // The owner phase's grant matrix for the host-grant relations, and what
    // a guarded store transaction needs besides. No grant on a catalog
    // function: the position is read with PostgreSQL's default privileges.
    for statement in [
        format!(
            "CREATE ROLE {role} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB \
             NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        ),
        format!("GRANT CONNECT ON DATABASE {database} TO {role}"),
        format!("GRANT USAGE ON SCHEMA public TO {role}"),
        format!(
            "GRANT SELECT ON host_grant_authority, host_grant_scope_lifecycles, \
             host_grant_rooms, _lineage TO {role}"
        ),
        format!("GRANT SELECT, UPDATE ON host_grant_scopes TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE ON host_grants TO {role}"),
        format!(
            "GRANT SELECT, INSERT ON host_grant_operations, host_grant_operation_changes \
             TO {role}"
        ),
        format!("GRANT SELECT ON ingress_protocol_epoch TO {role}"),
        format!("GRANT UPDATE (id) ON ingress_protocol_epoch TO {role}"),
    ] {
        sqlx::query(&statement)
            .execute(&admin)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    let mut runtime_url = url::Url::parse(fixture.ingress.db.database_url()).expect("URL");
    runtime_url.set_username(&role).expect("runtime user");
    runtime_url
        .set_password(Some(&password))
        .expect("runtime password");
    let runtime_db = Database::from_config(
        "host-grant-restore-runtime",
        &DatabaseConfig::new(DatabaseDriver::Postgres, runtime_url.to_string()),
    )
    .await
    .expect("runtime database");
    let runtime = HostGrantStore::open(runtime_db.clone()).expect("runtime store");
    let before = fixture.state(&fixture.scope).await;
    let binding = fixture.store.authority_binding().await.expect("binding");
    let database_oid: i64 = sqlx::query_scalar(
        "SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname = current_database()",
    )
    .fetch_one(&admin)
    .await
    .expect("database OID");
    assert_eq!(binding.database_oid, database_oid);
    assert_eq!(binding.database_name, database);

    // The runtime reads the binding and the PostgreSQL position.
    assert_eq!(
        runtime.authority_binding().await.expect("runtime read"),
        binding
    );
    let position = runtime.postgres_position().await.expect("runtime position");
    let owner_position = fixture.store.postgres_position().await.expect("position");
    assert_eq!(
        (position.system_identifier, position.timeline),
        (owner_position.system_identifier, owner_position.timeline)
    );
    assert!(position.timeline >= 1);

    // It can neither declare a restore nor acknowledge a position, and
    // nothing changes.
    assert!(matches!(
        runtime
            .prepare_restore_incarnation(RestoreEpoch::new(1))
            .await,
        Err(HostGrantError::OwnerPhaseRequired)
    ));
    assert!(matches!(
        runtime.acknowledge_position().await,
        Err(HostGrantError::OwnerPhaseRequired)
    ));
    assert_eq!(
        fixture.store.authority_binding().await.expect("binding"),
        binding
    );
    assert_eq!(fixture.state(&fixture.scope).await, before);
    // The owner records an acknowledgement of the system and timeline.
    assert_eq!(
        fixture.store.acknowledge_position().await.expect("ack"),
        (owner_position.system_identifier, owner_position.timeline)
    );

    drop(runtime);
    drop(runtime_db);
    for statement in [
        format!(
            "SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
             WHERE usename = '{role}'"
        ),
        format!("DROP OWNED BY {role}"),
        format!("DROP ROLE {role}"),
    ] {
        sqlx::query(&statement)
            .execute(&admin)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    admin.close().await;
    fixture.close().await;
}

/// One enrolled scope in a private PostgreSQL database, and the disposable
/// SpiceDB, emptied and started as a server starts it.
struct Live {
    fixture: GrantFixture,
    config: ServerConfig,
    raw: Client,
}

impl Live {
    async fn start(label: &str) -> Self {
        let (endpoint, preshared_key) = disposable_spicedb_env();
        let fixture = GrantFixture::enrolled(label).await.expect(
            "the host-grant restore cases need the PostgreSQL test cluster as well as SpiceDB; \
             set the WADDLE_TEST_POSTGRES_URL variable",
        );
        let raw = Client::builder(&endpoint, &preshared_key)
            .insecure(true)
            .build()
            .await
            .expect("raw SpiceDB client");
        // Empty the store, markers and witnesses included: the restore fence
        // reads every one, and a lifecycle another suite left behind would
        // count as a lost enrollment. A definition the stored schema lacks
        // makes its delete fail; that is the state wanted.
        for definition in guarded_resource_definitions().chain([
            AUTHZ_CODEC_MARKER_DEFINITION,
            AUTHZ_SCHEMA_MARKER_DEFINITION,
        ]) {
            let _ = raw
                .delete_relationships(RelationshipFilter::new(definition))
                .await;
        }
        let mut config = ServerConfig::test_homeserver();
        config.spicedb = Some(SpiceDbConfig {
            endpoint,
            preshared_key,
            insecure: true,
        });
        Self {
            fixture,
            config,
            raw,
        }
    }

    /// A replica's permission actor: the production backend, started with
    /// the server's startup steps and guarding `store`'s scopes.
    async fn actor(&self, store: &HostGrantStore) -> ActorRef<PermissionActor> {
        let backend = PermissionActor::from_server_config(&self.config)
            .await
            .expect("production SpiceDB backend");
        let actor = PermissionActor::spawn(backend.with_host_grant_store(store.clone()));
        actor.ask(EnsureSchema).await.expect("built-in schema");
        actor
            .ask(EnsureAuthzCodecIncarnation)
            .await
            .expect("codec incarnation");
        actor
            .ask(RecordAuthzSchemaVersion)
            .await
            .expect("schema version");
        actor
    }

    /// A replica: a store on its own pool and its own permission actor.
    async fn replica(&self) -> (HostGrantAuthority, ActorRef<PermissionActor>) {
        let db = Database::from_config(
            "host-grant-restore-replica",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                self.fixture.ingress.db.database_url().to_owned(),
            ),
        )
        .await
        .expect("replica database");
        let store = HostGrantStore::open(db).expect("replica store");
        let actor = self.actor(&store).await;
        (
            HostGrantAuthority::new(store, actor.clone(), self.fixture.policy.clone()),
            actor,
        )
    }

    fn channel(&self) -> Object {
        Object::new(ObjectType::Channel, self.fixture.scope.channel_id.as_str())
    }

    /// The stamps SpiceDB holds under one resource of the marker definition.
    async fn stamps_of(&self, resource: &str) -> Vec<String> {
        let mut stream = self
            .raw
            .read_relationships(
                RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION).resource_id(resource),
            )
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .expect("read markers");
        let mut stamps = Vec::new();
        while let Some(item) = stream.next().await {
            let relationship = item.expect("marker row").relationship;
            assert_eq!(relationship.relation, HOST_GRANT_PROJECTION_MARKER_RELATION);
            stamps.push(relationship.subject.object().object_id().to_owned());
        }
        stamps.sort();
        stamps
    }

    /// The marker stamps SpiceDB holds for the fixture's lifecycle.
    async fn stamps(&self) -> Vec<String> {
        self.stamps_of(&self.fixture.scope.lifecycle.to_storage())
            .await
    }

    /// The marker the canonical store expects in the projection now.
    async fn current_marker(&self) -> ProjectionMarker {
        self.fixture
            .store
            .projection_plan(self.fixture.scope.lifecycle)
            .await
            .expect("projection plan")
            .marker()
            .clone()
    }

    /// Remove the fixture scope's projection marker, as a SpiceDB that lost
    /// it holds it.
    async fn delete_marker(&self) {
        self.raw
            .delete_relationships(
                RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION)
                    .resource_id(self.fixture.scope.lifecycle.to_storage()),
            )
            .await
            .expect("delete the marker");
    }

    /// Write `accounts` as members of the fixture's room beside the
    /// projection, in one request, as a holder of the key without the scope
    /// guard could.
    async fn write_strays(&self, accounts: &[String]) {
        let updates: Vec<RelationshipUpdate> = accounts
            .iter()
            .map(|account| {
                RelationshipUpdate::touch(wire_relationship(&Tuple::new(
                    self.channel(),
                    Relation::new("member"),
                    Subject::user(account.as_str()),
                )))
            })
            .collect();
        self.raw
            .write_relationships(updates)
            .await
            .expect("stray relationships");
    }

    /// The fixed identity the authority's position witness is filed under.
    async fn authority_id(&self) -> String {
        let pool = owner_pool(&self.fixture).await;
        let id: String =
            sqlx::query_scalar("SELECT authority_id FROM host_grant_authority WHERE singleton")
                .fetch_one(&pool)
                .await
                .expect("authority id");
        pool.close().await;
        id
    }

    /// The authority's position witness as SpiceDB holds it.
    async fn position_witness(&self, actor: &ActorRef<PermissionActor>) -> Option<PositionWitness> {
        let listed = actor
            .ask(ReadHostGrantWitness {
                resource: position_witness_resource(&self.authority_id().await),
            })
            .await
            .expect("read the position witness")
            .expect("a SpiceDB backend");
        match listed.stamps.as_slice() {
            [] => None,
            [stamp] => Some(PositionWitness::parse(stamp).expect("well-formed witness")),
            _ => panic!("one position witness"),
        }
    }

    /// Replace the authority's position witness with `next`.
    async fn write_position_witness(
        &self,
        actor: &ActorRef<PermissionActor>,
        next: PositionWitness,
    ) {
        let previous = self.position_witness(actor).await;
        let written = actor
            .ask(ReplaceHostGrantWitness {
                resource: position_witness_resource(&self.authority_id().await),
                previous: previous.map(|witness| witness.stamp()),
                next: next.stamp(),
            })
            .await
            .expect("write the position witness")
            .expect("a SpiceDB backend");
        assert_eq!(written, ProjectionWrite::Applied);
    }

    async fn close(self) {
        self.fixture.close().await;
    }
}

/// `tuple` as a SpiceDB relationship, through the production codec.
fn wire_relationship(tuple: &Tuple) -> Relationship {
    let wire = spicedb_wire::relationship(tuple).expect("wire relationship");
    Relationship::new(
        ObjectReference::new(wire.resource.object_type, wire.resource.object_id).expect("resource"),
        wire.relation,
        SubjectReference::new(
            ObjectReference::new(wire.subject.object_type, wire.subject.object_id)
                .expect("subject"),
            wire.subject.relation,
        )
        .expect("subject reference"),
    )
}

/// What SpiceDB itself answers for a channel permission.
async fn spicedb_allows(
    actor: &ActorRef<PermissionActor>,
    account: &str,
    permission: Permission,
    channel: &Object,
) -> bool {
    actor
        .ask(CheckPermission {
            subject: Subject::user(account),
            permission,
            object: channel.clone(),
        })
        .await
        .expect("permission check")
        .allowed
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_restore_cannot_reopen_a_projected_revoke() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_revoke").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("first pass"),
        Reconciliation::Activated(GrantGeneration::FIRST)
    );
    let backup = AuthorityBackup::take(&live.fixture).await;

    // The sender is revoked, and the revoke is projected.
    live.fixture
        .set(OWNER, SENDER, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("revoke pass"),
        Reconciliation::Activated(generation(2))
    );
    let revoked = live.current_marker().await;

    // The database is restored to before the revoke: the sender is a member
    // again and the scope active, under the same incarnation.
    backup.restore(&live.fixture).await;
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);

    // Undeclared, the fence closes the scope: the projection records the
    // revoke, which the journal does not. The authority as a whole is served.
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence of an undeclared restore");
    assert_eq!(report.fenced, 1);
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Fenced));
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Fence
    );
    for account in [OWNER, SENDER, PEER] {
        assert!(!sql_admits(&live.fixture, account, HostGrantAction::Read).await);
    }
    // A fenced scope is not projected again, nor changed by an initiator.
    assert!(matches!(
        authority.reconcile(lifecycle).await,
        Err(HostGrantError::ScopeFenced)
    ));
    assert!(matches!(
        live.fixture.set(OWNER, PEER, RoomAffiliation::None).await,
        Err(HostGrantError::ScopeFenced)
    ));
    // A second pass finds it closed and closes nothing.
    assert_eq!(
        verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
            .await
            .expect("second fence")
            .fenced,
        0
    );

    // Declared, the scope waits for reconstruction under a new incarnation.
    assert!(matches!(
        declare(&live.fixture, 1).await,
        RestoreDeclaration::Transitioned {
            restored_scopes: 1,
            ..
        }
    ));
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence of the declared restore");
    assert_eq!(report.awaiting_reconstruction, 1);
    assert_eq!(report.fenced, 0);

    // Reconstruction keeps the revoke the restore lost and rebuilds the
    // projection from the canonical rows; the scope admits once an operator
    // reopens it.
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(4))
    );
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::SendMessage).await);
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence after reconstruction");
    assert_eq!(
        (report.awaiting_reconstruction, report.awaiting_reopen),
        (0, 1)
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(sql_admits(&live.fixture, PEER, HostGrantAction::SendMessage).await);
    assert!(sql_admits(&live.fixture, OWNER, HostGrantAction::Manage).await);
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.admission, AdmissionState::Active);
    assert_ne!(state.authority_incarnation, revoked.incarnation);
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("reopen pass"),
        Reconciliation::Activated(generation(5))
    );
    let marker = live.current_marker().await;
    assert_eq!(marker.incarnation, state.authority_incarnation);
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    let channel = live.channel();
    assert!(!spicedb_allows(&actor, SENDER, Permission::Read, &channel).await);
    assert!(spicedb_allows(&actor, PEER, Permission::Read, &channel).await);
    // Once reopened, the journal witness follows the new incarnation.
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    assert_eq!(
        live.stamps_of(&head_witness_resource(lifecycle)).await,
        vec![marker.stamp()]
    );
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence after the reopen");
    assert_eq!((report.fenced, report.awaiting_reopen), (0, 0));
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_restore_under_a_running_replica_is_found_without_a_restart() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_running").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let replica = live.fixture.ingress.authority().await;
    let gate = replica.host_grant_gate();
    gate.install_verifier_for_test(
        live.fixture.store.clone(),
        actor.clone(),
        served(),
        RestoreEpoch::INITIAL,
    );
    gate.verify_once().await.expect("first verification");
    let backup = AuthorityBackup::take(&live.fixture).await;
    live.fixture
        .set(OWNER, SENDER, RoomAffiliation::None)
        .await
        .expect("revoke");
    authority.reconcile(lifecycle).await.expect("revoke pass");

    // The rows are rewritten from the backup while the replica runs, with
    // neither a restart of PostgreSQL nor a new relation: the binding is
    // the same, and the sender is a member again.
    backup.restore(&live.fixture).await;
    assert_eq!(
        gate.current().expect("attachment").binding(),
        &live
            .fixture
            .store
            .authority_binding()
            .await
            .expect("binding")
    );
    // The periodic fence finds the projection ahead and closes the scope,
    // which every replica's acceptances read from PostgreSQL.
    gate.watch(Duration::from_millis(100));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while live.fixture.state(&live.fixture.scope).await.closed_reason != Some(ClosedReason::Fenced)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the periodic fence did not close the scope"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let refused = live.fixture.room_submission("running-refused", "refused");
    let decision = replica.commit(&refused).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_the_journal_witness_finds_a_lost_revoke_the_projection_never_saw() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_journal").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    let backup = AuthorityBackup::take(&live.fixture).await;

    // The sender is revoked through the authority: the revoke commits, and
    // its journal witness follows; its projection is never written.
    let command = live
        .fixture
        .command(OWNER, SENDER, RoomAffiliation::None)
        .await;
    let receipt = authority.set_affiliation(&command).await.expect("revoke");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses after the revoke");
    let record = receipt.record();
    let head = live.stamps_of(&head_witness_resource(lifecycle)).await;
    assert_eq!(head.len(), 1);
    assert!(head[0].contains(&record.operation.to_storage()));
    let projected = live.stamps().await;
    assert!(!projected[0].contains(&record.operation.to_storage()));

    // Restored to before the revoke, the projection agrees with the journal;
    // the journal witness does not, and the fence closes the scope.
    backup.restore(&live.fixture).await;
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence");
    assert_eq!(report.fenced, 1);
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Fenced)
    );
    // The witness is evidence: no writer moves it while the journal
    // contradicts it.
    assert_eq!(
        authority
            .sync_witnesses(lifecycle)
            .await
            .expect("witness writer"),
        waddle_server::permissions::host_grant::WitnessSync::Kept
    );
    assert_eq!(
        live.stamps_of(&head_witness_resource(lifecycle)).await,
        head
    );

    // Declared and reconstructed, the projection cannot show the revoke, so
    // reconstruction keeps the sender's grant; the operator applies it again
    // before reopening, which takes effect at once.
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(3))
    );
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    operator_revoke(&live.fixture, SENDER, false)
        .await
        .expect("operator revoke");
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("revoke pass"),
        Reconciliation::Reconstructed(generation(4))
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    assert!(sql_admits(&live.fixture, PEER, HostGrantAction::SendMessage).await);
    // The reopen lets the witness move to the new incarnation.
    assert_eq!(
        authority
            .sync_witnesses(lifecycle)
            .await
            .expect("witness after the reopen"),
        waddle_server::permissions::host_grant::WitnessSync::Written
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_the_position_witness_refuses_a_rolled_back_database() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_position").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    let written = live
        .position_witness(&actor)
        .await
        .expect("position witness");
    assert_eq!(written.epoch, RestoreEpoch::INITIAL);
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence");
    assert!(matches!(
        report.position,
        Some(PositionStanding::Current | PositionStanding::Advanced)
    ));

    // Another timeline: a promotion the witness cannot verify on its own
    // refuses the authority until an operator acknowledges it.
    let other_timeline = PositionWitness {
        position: PostgresPosition {
            timeline: written.position.timeline + 1,
            lsn: 0,
            ..written.position
        },
        ..written
    };
    live.write_position_witness(&actor, other_timeline).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("an unverified timeline")
        ),
        RestoreFenceRefusal::PositionUnverified
    );
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(live.position_witness(&actor).await, Some(other_timeline));
    live.fixture
        .store
        .acknowledge_position()
        .await
        .expect("acknowledgement");
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence after the acknowledgement");
    assert_eq!(report.position, Some(PositionStanding::Rebased));
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    let rebased = live
        .position_witness(&actor)
        .await
        .expect("rebased witness");
    assert_eq!(rebased.position.timeline, written.position.timeline);

    // The witness is ahead of the server on its own timeline: the database
    // went back in place.
    let ahead = PositionWitness {
        position: PostgresPosition {
            lsn: rebased.position.lsn + (1 << 40),
            ..rebased.position
        },
        ..rebased
    };
    live.write_position_witness(&actor, ahead).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a regressed position")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    // No writer moves it back, and an acknowledgement does not excuse it.
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(live.position_witness(&actor).await, Some(ahead));
    live.fixture
        .store
        .acknowledge_position()
        .await
        .expect("acknowledgement");
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("still regressed")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    // A replica's attachment is refused with it.
    assert_eq!(
        fence_refusal(
            HostGrantAttachment::fenced_for_test(
                &live.fixture.store,
                &actor,
                served(),
                RestoreEpoch::INITIAL
            )
            .await
            .expect_err("no attachment")
        ),
        RestoreFenceRefusal::PositionRegressed
    );

    // A declared restore is the acknowledgement of what was lost.
    declare(&live.fixture, 1).await;
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence of the declared restore");
    assert_eq!(report.position, Some(PositionStanding::Rebased));
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(
        live.position_witness(&actor)
            .await
            .expect("witness of the declared epoch")
            .epoch,
        RestoreEpoch::new(1)
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_reconstruction_keeps_lost_bans_and_grants_nothing_from_the_projection() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_bans").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let backup = AuthorityBackup::take(&live.fixture).await;

    // After the backup: the peer is banned, Carol granted, and Dave, who
    // held nothing, banned. All of it is projected.
    for (account, affiliation) in [
        (PEER, RoomAffiliation::Outcast),
        (CAROL, RoomAffiliation::Member),
        (DAVE, RoomAffiliation::Outcast),
    ] {
        live.fixture
            .set(OWNER, account, affiliation)
            .await
            .expect("change after the backup");
    }
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(generation(4))
    );

    backup.restore(&live.fixture).await;
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(3))
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);

    // Both bans the restore lost hold; Carol's grant, which it also lost, is
    // not recreated from the projection.
    assert_eq!(
        live.fixture.affiliations(PEER).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    assert_eq!(
        live.fixture.affiliations(DAVE).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    assert_eq!(
        live.fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    for account in [PEER, DAVE, CAROL] {
        assert!(!sql_admits(&live.fixture, account, HostGrantAction::Read).await);
    }
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);

    // The rebuilt projection is exactly the canonical rows of the
    // reconstruction; the reopen's operation follows with the next pass.
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("reopen pass"),
        Reconciliation::Activated(generation(4))
    );
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let observation = authority.observe(&plan).await.expect("observation");
    assert!(HostGrantAuthority::verified(&plan, &observation));
    let channel = live.channel();
    for account in [PEER, DAVE, CAROL] {
        assert!(!spicedb_allows(&actor, account, Permission::Read, &channel).await);
    }
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_reconstruction_lowers_a_remote_privileged_grant_to_none() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_remote").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    // A remote account of a trusted home holds an admin grant, which this
    // deployment honours as no grant at all.
    let remote = "frank@trusted.example.net";
    let trusted = host_grant_support::issuance_policy(&["trusted.example.net"]);
    let command = live
        .fixture
        .command(OWNER, remote, RoomAffiliation::Admin)
        .await;
    live.fixture
        .store
        .set_affiliation(&command, &trusted)
        .await
        .expect("trusted-home admin");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(generation(2))
    );
    let backup = AuthorityBackup::take(&live.fixture).await;
    // The grant is demoted to member and projected; the restore loses that.
    let command = live
        .fixture
        .command(OWNER, remote, RoomAffiliation::Member)
        .await;
    live.fixture
        .store
        .set_affiliation(&command, &trusted)
        .await
        .expect("demotion");
    authority.reconcile(lifecycle).await.expect("demotion pass");
    backup.restore(&live.fixture).await;
    declare(&live.fixture, 1).await;
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(_)
    ));
    // Lowering the admin grant to the projection's `member` would raise
    // what admission honours for the remote account, from nothing to
    // member: it is recorded at `none`.
    assert_eq!(
        live.fixture.affiliations(remote).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_reconstruction_of_a_current_projection_keeps_recorded_grants_and_drops_strays(
) {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_current").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");

    // A grant recorded and not yet projected, and a relationship written
    // beside the projection by a holder of the key.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    live.write_strays(&[EVE.to_owned()]).await;

    // A restore is declared for a database that lost nothing (a clone that
    // was adopted, say): the projection's marker is in the journal.
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(4))
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);

    // The recorded grant is effective once the rebuilt projection is
    // verified; the stray is recorded as holding nothing and dropped.
    assert_eq!(
        live.fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::SendMessage).await);
    assert_eq!(
        live.fixture.affiliations(EVE).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    let channel = live.channel();
    assert!(!spicedb_allows(&actor, EVE, Permission::Read, &channel).await);
    assert!(spicedb_allows(&actor, CAROL, Permission::Read, &channel).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_projection_pass_from_before_a_restore_cannot_reopen() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_old_pass").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let projected = live.current_marker().await;

    // A pass reads its plan for a grant, and the restore is declared before
    // it writes.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    let old_plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    declare(&live.fixture, 1).await;

    // The old plan is no takeover: no plan of a scope that was not restored
    // can be made into one. Its write, applied against the marker it saw,
    // cannot be activated: the scope is bound to another incarnation now.
    assert!(old_plan.clone().into_restored().is_none());
    assert_eq!(
        authority
            .write(&old_plan, Some(&projected))
            .await
            .expect("old write"),
        ProjectionWrite::Applied
    );
    assert_eq!(
        activate_host_grant_marker(&live.fixture.store, old_plan.marker())
            .await
            .expect("old activation"),
        Activation::Superseded
    );
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);

    // A takeover conditioned on a marker the projection does not hold
    // writes nothing.
    let restored = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan")
        .into_restored()
        .expect("a restored plan");
    let stale = ProjectionMarker {
        incarnation: AuthorityIncarnation::generate(),
        ..projected.clone()
    };
    assert_eq!(
        authority
            .take_over(&restored, &stale)
            .await
            .expect("conditioned takeover"),
        ProjectionWrite::PreconditionFailed
    );
    assert_eq!(live.stamps().await, vec![old_plan.marker().stamp()]);

    // Reconstruction takes the projection over from the old write.
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(3))
    );
    assert_eq!(
        live.current_marker().await.incarnation,
        live.fixture
            .store
            .authority_incarnation()
            .await
            .expect("incarnation")
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_restore_to_before_a_transition_is_refused() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_twice").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let backup = AuthorityBackup::take(&live.fixture).await;

    // A first restore is declared, reconstructed and reopened, and the
    // witnesses follow it.
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(2))
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    authority.reconcile(lifecycle).await.expect("reopen pass");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");

    // The database is restored again, to before that transition.
    backup.restore(&live.fixture).await;
    // The configuration holds the declared epoch, which the database no
    // longer records.
    assert_eq!(
        epoch_mismatch(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
                .await
                .expect_err("a database from before the transition")
        ),
        (RestoreEpoch::new(1), RestoreEpoch::INITIAL)
    );
    // With the configuration rolled back as well, the position witness was
    // written under the later epoch.
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a witness of a later epoch")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    // Without that witness, the scope's marker names an incarnation the
    // journal does not record, and the scope is closed.
    live.raw
        .delete_relationships(
            RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION)
                .resource_id(position_witness_resource(&live.authority_id().await)),
        )
        .await
        .expect("drop the position witness");
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence without a position witness");
    assert_eq!(report.fenced, 1);
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Fenced)
    );

    // A second declaration closes the scope again, and reconstruction takes
    // over the projection of the lost incarnation.
    declare(&live.fixture, 2).await;
    verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(2))
        .await
        .expect("fence of the second declaration");
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(generation(3))
    );
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_lost_enrollment_refuses_only_its_own_rooms() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_enrollment").await;
    let (authority, actor) = live.replica().await;
    authority
        .reconcile(live.fixture.scope.lifecycle)
        .await
        .expect("first pass");
    let backup = AuthorityBackup::take(&live.fixture).await;

    // A scope enrolled and projected after the backup.
    let later = live.fixture.enroll_another().await;
    assert_eq!(
        authority.reconcile(later.lifecycle).await.expect("pass"),
        Reconciliation::Activated(GrantGeneration::FIRST)
    );
    assert_eq!(
        verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
            .await
            .expect("fence before the restore")
            .orphaned,
        0
    );

    // Restored, the database does not know the later scope or its room.
    backup.restore(&live.fixture).await;
    assert!(live
        .fixture
        .store
        .scope(later.lifecycle)
        .await
        .expect("read")
        .is_none());
    // The authority is served; the lost scope's room is refused rather
    // than served as a room no scope manages.
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence of a lost enrollment");
    assert_eq!(report.orphaned, 1);
    assert_eq!(report.fenced, 0);
    let attachment = HostGrantAttachment::fenced_for_test(
        &live.fixture.store,
        &actor,
        served(),
        RestoreEpoch::INITIAL,
    )
    .await
    .expect("attachment");
    assert!(attachment.refuses_unmanaged(&later.room));
    assert!(!attachment.refuses_unmanaged(&live.fixture.scope.room));
    let replica = live.fixture.ingress.authority().await;
    assert!(replica.attach_host_grants(attachment));
    let lost = room_submission(
        &live.fixture.ingress,
        &later.room,
        "lost-enrollment",
        "refused",
        &[PEER_RESOURCE],
    );
    let decision = replica.commit(&lost).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    // The surviving scope admits, and a room no scope ever managed is
    // unaffected.
    let kept = live.fixture.room_submission("kept-scope", "admitted");
    assert_eq!(
        replica.commit(&kept).await.class,
        IngressDecisionClass::Accepted
    );
    let unmanaged = room_submission(
        &live.fixture.ingress,
        &host_grant_support::bare("open@muc.example.com"),
        "never-managed",
        "local traffic",
        &[PEER_RESOURCE],
    );
    assert_eq!(
        replica.commit(&unmanaged).await.class,
        IngressDecisionClass::Accepted
    );
    // A declared restore does not change what the database lost.
    declare(&live.fixture, 1).await;
    assert_eq!(
        verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
            .await
            .expect("fence of a lost enrollment, declared")
            .orphaned,
        1
    );
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_scope_that_admits_without_a_marker_is_closed() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_no_marker").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let other = live.fixture.enroll_another().await;
    authority.reconcile(other.lifecycle).await.expect("pass");

    // SpiceDB lost the first scope's projection marker.
    live.delete_marker().await;
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence");
    assert_eq!(report.fenced, 1);
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Fenced)
    );
    // Only that scope is closed.
    assert_eq!(
        live.fixture.state(&other).await.admission,
        AdmissionState::Active
    );

    // Declared, the restored scope cannot be reconstructed without a
    // marker: it stays closed.
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Contradiction(ProjectionContradiction::MarkerMissing)
    );
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Restored)
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_reconciliation_closes_a_scope_whose_marker_is_ahead() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_ahead").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let backup = AuthorityBackup::take(&live.fixture).await;
    live.fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    authority.reconcile(lifecycle).await.expect("ban pass");
    backup.restore(&live.fixture).await;

    // The restored database records another operation at the generation the
    // projection holds; the pass that would project it finds the marker
    // ahead and closes the scope, as the fence would.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant after the restore");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Contradiction(ProjectionContradiction::AheadOfCanonical)
    );
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Fenced));
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    assert_eq!(
        live.fixture.affiliations(PEER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_projection_older_than_the_database_closes_nothing() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_older").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let first = live.current_marker().await;
    declare(&live.fixture, 1).await;
    authority
        .reconcile(lifecycle)
        .await
        .expect("reconstruction");
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    authority.reconcile(lifecycle).await.expect("reopen pass");

    // SpiceDB goes back to the first incarnation's marker, which the
    // journal records: SpiceDB is older than PostgreSQL, not the reverse.
    let current = live.current_marker().await;
    let replaced = actor
        .ask(ReplaceHostGrantWitness {
            resource: lifecycle.to_storage(),
            previous: Some(current.stamp()),
            next: first.stamp(),
        })
        .await
        .expect("restore the older marker")
        .expect("a SpiceDB backend");
    assert_eq!(replaced, ProjectionWrite::Applied);
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence");
    assert_eq!(report.fenced, 0);
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.admission,
        AdmissionState::Active
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_restored_scope_with_a_malformed_family_stays_closed() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_malformed").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    // More relationships in one relation than a scope can hold: the family
    // cannot be read as a projection.
    let strays: Vec<String> = (0..201)
        .map(|index| format!("stray-{index}@example.com"))
        .collect();
    live.write_strays(&strays).await;
    declare(&live.fixture, 1).await;
    let closed = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Contradiction(ProjectionContradiction::MalformedFamily)
    );
    assert_eq!(live.fixture.state(&live.fixture.scope).await, closed);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_reconstruction_beyond_the_scope_bound_stays_closed() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_bound").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");

    // Beside the three canonical rows, the projection holds 198 members
    // the canonical store never granted: recording each would take the
    // scope to 201 rows, beyond the 200 one projection write carries.
    let strays: Vec<String> = (0..198)
        .map(|index| format!("stray-{index}@example.com"))
        .collect();
    live.write_strays(&strays).await;
    declare(&live.fixture, 1).await;
    let closed = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Contradiction(ProjectionContradiction::ReconstructionTooLarge)
    );

    // Nothing was recorded or written, and the scope stays closed.
    assert_eq!(live.fixture.state(&live.fixture.scope).await, closed);
    assert_eq!(live.fixture.affiliations("stray-0@example.com").await, None);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    assert_ne!(
        live.stamps().await,
        vec![live.current_marker().await.stamp()]
    );
    live.close().await;
}

/// The endpoint and key of the disposable SpiceDB, refusing to proceed unless
/// the operator has declared the store disposable: these tests delete every
/// relationship of the guarded definitions and both markers.
fn disposable_spicedb_env() -> (String, String) {
    assert_eq!(
        std::env::var("WADDLE_TEST_SPICEDB_DISPOSABLE").as_deref(),
        Ok("1"),
        "WADDLE_TEST_SPICEDB_DISPOSABLE=1 is required: the live SpiceDB tests delete every \
         relationship of the guarded definitions and both markers, so they only run against a \
         store that is safe to wipe"
    );
    let endpoint =
        std::env::var("WADDLE_TEST_SPICEDB_ENDPOINT").expect("WADDLE_TEST_SPICEDB_ENDPOINT");
    let preshared_key = std::env::var("WADDLE_TEST_SPICEDB_PRESHARED_KEY")
        .expect("WADDLE_TEST_SPICEDB_PRESHARED_KEY");
    (endpoint, preshared_key)
}
