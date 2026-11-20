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
//!   place and refuses managed rooms while that fails; it reports itself not
//!   ready only while its configured restore epoch predates the declared
//!   one, and a runtime role corrected by the owner phase attaches it
//!   without a restart.
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

// The shared host-grant and ingress fixtures serve several suites; this one
// uses a subset of each.
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[path = "host_grant_support/live_spicedb.rs"]
mod live_spicedb;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use host_grant_support::{
    room_submission, served, subject, GrantFixture, OWNER, PEER, PEER_RESOURCE, SENDER,
};
use ingress_support::IngressFixture;
use kameo::actor::{ActorRef, Spawn};
use live_spicedb::{disposable_spicedb_env, empty_disposable_store, mark_test_store};
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
    epoch_witness_resource, head_witness_resource, latch_witness_resource, position_standing,
    position_witness_resource, reconstruction_targets, regression, room_witness_digest,
    room_witness_resource, sync_position_witness, sync_witnesses_after_fence, verify_restore_fence,
    verify_restore_fence_with, with_host_grant_commit_fault, with_host_grant_witness_write_fault,
    Activation, AdmissionState, AuthorityIncarnation, BindSpace, ClosedReason, EnrolledScopes,
    FenceMemory, GateReadiness, GateState, GrantGeneration, GrantOperationId, HostGrantAction,
    HostGrantAttachment, HostGrantAuthority, HostGrantCommitFault, HostGrantError, HostGrantGate,
    HostGrantStore, LatchKind, MutationReceipt, OperationKind, OperationOutcome, OperatorRevoke,
    PositionAcknowledgement, PositionLatch, PositionStanding, PositionWitness, PositionWriter,
    PostgresPosition, PrincipalEvidence, ProjectedGrant, ProjectedRelationship,
    ProjectionContradiction, ProjectionIntent, ProjectionMarker, ProjectionWrite, Reconciliation,
    ReconstructionTarget, ReopenReport, ReopenSelection, ReplicaFailure, RestoreDeclaration,
    RestoreEpoch, RestoreFenceRefusal, RestoreFenceReport, RetireScope, RoomAffiliation, ScopeKey,
    ScopeLifecycleId, WitnessChange, WitnessSync, HOST_GRANT_PROJECTION_DEFINITION,
    HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    CheckPermission, ClearHostGrantWitnesses, EnsureAuthzCodecIncarnation, EnsureSchema, Object,
    ObjectType, Permission, PermissionActor, ReadHostGrantWitness, RecordAuthzSchemaVersion,
    Relation, ReplaceHostGrantWitnesses, Subject, Tuple,
};
use waddle_server::test_support::{
    activate_host_grant_marker, host_grant_readiness_response, spicedb_wire,
};
use waddle_server::ServerConfig;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const CAROL: &str = "carol@example.com";
const DAVE: &str = "dave@example.com";
const EVE: &str = "eve@example.com";

const SERVICE_UNAVAILABLE: (ErrorType, DefinedCondition) =
    (ErrorType::Cancel, DefinedCondition::ServiceUnavailable);

/// Every relation of the canonical host-grant store: what a restore of the
/// database brings back.
const AUTHORITY_RELATIONS: [&str; 10] = [
    "host_grant_authority",
    "host_grant_scope_lifecycles",
    "host_grant_scopes",
    "host_grant_rooms",
    "host_grants",
    "host_grant_operations",
    "host_grant_operation_changes",
    "host_grant_space_bindings",
    "host_grant_space_affiliations",
    "host_grant_space_operation_changes",
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
        for relation in AUTHORITY_RELATIONS.into_iter().rev() {
            sqlx::query(&format!("DELETE FROM public.{relation}"))
                .execute(&mut *tx)
                .await
                .unwrap_or_else(|error| panic!("clear {relation}: {error}"));
        }
        for relation in AUTHORITY_RELATIONS {
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

/// An operator's reopen of one scope, without confirming that what a
/// restore lost in it has been applied again.
async fn reopen(fixture: &GrantFixture, lifecycle: ScopeLifecycleId) -> ReopenReport {
    fixture
        .store
        .reopen_scopes(ReopenSelection::Scope {
            lifecycle,
            lost_operations_reapplied: false,
        })
        .await
        .expect("reopen")
}

/// An operator's reopen of one scope the restore lost operations in: it
/// is left closed without the confirmation, and reopens with it.
async fn reopen_after_reapplying(fixture: &GrantFixture, lifecycle: ScopeLifecycleId) {
    assert_eq!(
        reopen(fixture, lifecycle).await,
        ReopenReport {
            reopened: 0,
            not_reconstructed: 0,
            lost_operations: 1,
        }
    );
    let marked = fixture
        .store
        .scope(lifecycle)
        .await
        .expect("read scope")
        .expect("scope");
    assert!(marked.lost_operations);
    let report = fixture
        .store
        .reopen_scopes(ReopenSelection::Scope {
            lifecycle,
            lost_operations_reapplied: true,
        })
        .await
        .expect("reopen with the operator's confirmation");
    assert_eq!(report.reopened, 1);
}

/// Run the restore fence and, as a replica whose fence passed does, bring
/// the witnesses up to date; the first position witness is written so.
async fn fence_and_witness(
    store: &HostGrantStore,
    actor: &ActorRef<PermissionActor>,
    configured: RestoreEpoch,
) -> RestoreFenceReport {
    let report = verify_restore_fence(store, actor, configured)
        .await
        .expect("fence");
    sync_witnesses_after_fence(store, actor, &report, None).await;
    report
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
    let latch = latch_witness_resource(&"a".repeat(32));
    assert_eq!(latch, format!("latch_{}", "a".repeat(32)));
    assert!(ScopeLifecycleId::from_storage(&latch).is_none());
}

#[test]
fn the_position_witness_detects_regressions_and_unverified_timelines() {
    let at = |epoch: u32, system: u64, timeline: u32, lsn: u64| PositionWitness {
        epoch: RestoreEpoch::new(epoch),
        position: PostgresPosition {
            system_identifier: system,
            timeline,
            lsn,
        },
    };
    // Its spelling is canonical, a system identifier above i64::MAX included.
    for witness in [at(0, 7, 1, 0), at(3, u64::MAX, 9, u64::MAX)] {
        assert_eq!(PositionWitness::parse(&witness.stamp()), Some(witness));
    }
    for refused in [
        "",
        "0_7_1",
        "0_7_1_00",
        "0_7_1_1_1",
        "x_7_1_1",
        "0_+7_1_1",
        "0_-7_1_1",
    ] {
        assert_eq!(PositionWitness::parse(refused), None, "{refused:?}");
    }

    let current = at(1, 7, 2, 1_000);
    let ack = |witness: PositionWitness, system: u64, timeline: u32| PositionAcknowledgement {
        supersedes: witness,
        system_identifier: system,
        timeline,
    };
    let latch = |kind: LatchKind, witness: PositionWitness| PositionLatch { kind, witness };
    let standing = |witness: PositionWitness,
                    acknowledgement: Option<PositionAcknowledgement>,
                    latched: Option<PositionLatch>| {
        position_standing(
            Some(&witness),
            &current,
            acknowledgement.as_ref(),
            latched.as_ref(),
        )
    };
    assert_eq!(
        position_standing(None, &current, None, None),
        PositionStanding::Unrecorded
    );
    // A latch without its witness shows that a witness was written: it is
    // unverified until a restore is declared since the latched witness.
    assert_eq!(
        position_standing(
            None,
            &current,
            None,
            Some(&latch(LatchKind::OnTimeline, at(1, 7, 2, 5_000)))
        ),
        PositionStanding::Unverified
    );
    assert_eq!(
        position_standing(
            None,
            &current,
            None,
            Some(&latch(LatchKind::OnTimeline, at(0, 7, 2, 5_000)))
        ),
        PositionStanding::Rebased
    );
    // The same system and timeline: the position only moves forward.
    assert_eq!(
        standing(at(1, 7, 2, 1_000), None, None),
        PositionStanding::Current
    );
    assert_eq!(
        standing(at(1, 7, 2, 999), None, None),
        PositionStanding::Advanced
    );
    assert_eq!(
        standing(at(1, 7, 2, 1_001), None, None),
        PositionStanding::Regressed
    );
    // A declared restore since the witness replaces it; a witness of a
    // later declared epoch belongs to a database the current one predates.
    assert_eq!(
        standing(at(0, 7, 2, 5_000), None, None),
        PositionStanding::Rebased
    );
    assert_eq!(
        standing(at(2, 7, 2, 0), None, None),
        PositionStanding::Regressed
    );
    // Another timeline: below the witnessed position it cannot contain it;
    // otherwise the switch point decides, which only an operator can read.
    let branched = at(1, 7, 1, 2_000);
    let promoted = at(1, 7, 1, 500);
    let foreign = at(1, 8, 2, 500);
    assert_eq!(standing(branched, None, None), PositionStanding::Regressed);
    assert_eq!(standing(promoted, None, None), PositionStanding::Unverified);
    assert_eq!(standing(foreign, None, None), PositionStanding::Unverified);

    // An acknowledgement is bound to the witness it supersedes and to the
    // system and timeline the server writes on, for a later timeline.
    assert_eq!(
        standing(promoted, Some(ack(promoted, 7, 2)), None),
        PositionStanding::Acknowledged
    );
    assert_eq!(
        standing(promoted, Some(ack(at(1, 7, 1, 499), 7, 2)), None),
        PositionStanding::Unverified
    );
    assert_eq!(
        standing(promoted, Some(ack(promoted, 7, 3)), None),
        PositionStanding::Unverified
    );
    // A witness of a later timeline than the server's, or of another
    // system, takes a declared restore: no acknowledgement of it is
    // admissible.
    let later_timeline = at(1, 7, 3, 500);
    assert!(!ack(later_timeline, 7, 2).admissible(1_000));
    assert_eq!(
        standing(later_timeline, Some(ack(later_timeline, 7, 2)), None),
        PositionStanding::Unverified
    );
    assert!(!ack(foreign, 7, 2).admissible(1_000));
    assert_eq!(
        standing(foreign, Some(ack(foreign, 7, 2)), None),
        PositionStanding::Unverified
    );
    assert!(ack(promoted, 7, 2).admissible(1_000));
    // A later timeline below the witnessed position is a regression across
    // timelines, provable from the two positions: no acknowledgement of it
    // is admissible, and none would clear it.
    assert!(!ack(branched, 7, 2).admissible(1_000));
    assert!(ack(at(1, 7, 2, 1_001), 7, 2).admissible(1_000));
    // No acknowledgement clears a regression across timelines.
    assert_eq!(
        standing(branched, Some(ack(branched, 7, 2)), None),
        PositionStanding::Regressed
    );
    // A regression on the witnessed timeline yields to an acknowledgement
    // bound to that witness, and to no other.
    let rolled_back = at(1, 7, 2, 1_001);
    assert_eq!(
        standing(rolled_back, Some(ack(rolled_back, 7, 2)), None),
        PositionStanding::Acknowledged
    );
    assert_eq!(
        standing(rolled_back, Some(ack(at(1, 7, 2, 1_002), 7, 2)), None),
        PositionStanding::Regressed
    );

    // A latch keeps a regression after the server wrote past the witness
    // again; one on the witnessed timeline yields to an acknowledgement
    // bound to it, one across timelines only to a declared restore.
    let passed = at(1, 7, 2, 999);
    assert_eq!(standing(passed, None, None), PositionStanding::Advanced);
    assert_eq!(
        standing(passed, None, Some(latch(LatchKind::OnTimeline, passed))),
        PositionStanding::Regressed
    );
    assert_eq!(
        standing(
            passed,
            Some(ack(passed, 7, 2)),
            Some(latch(LatchKind::OnTimeline, passed))
        ),
        PositionStanding::Acknowledged
    );
    assert_eq!(
        standing(
            promoted,
            Some(ack(promoted, 7, 2)),
            Some(latch(LatchKind::AcrossTimelines, promoted))
        ),
        PositionStanding::Regressed
    );
    assert_eq!(
        position_standing(
            Some(&promoted),
            &at(2, 7, 2, 1_000),
            None,
            Some(&latch(LatchKind::AcrossTimelines, promoted))
        ),
        PositionStanding::Rebased
    );
    // A latch that names another witness is spent.
    assert_eq!(
        standing(
            passed,
            None,
            Some(latch(LatchKind::OnTimeline, at(1, 7, 2, 5)))
        ),
        PositionStanding::Advanced
    );
    assert_eq!(
        regression(&rolled_back, &current),
        Some(LatchKind::OnTimeline)
    );
    assert_eq!(
        regression(&branched, &current),
        Some(LatchKind::AcrossTimelines)
    );
    assert_eq!(regression(&passed, &current), None);
    assert_eq!(regression(&foreign, &current), None);
    assert_eq!(regression(&at(2, 7, 2, 5_000), &current), None);

    // The acknowledgement and the latch have one spelling each.
    let acknowledgement = ack(promoted, 7, 2);
    assert_eq!(acknowledgement.spelling(), "1_7_1_500/7_2");
    assert_eq!(
        PositionAcknowledgement::parse(&acknowledgement.spelling()),
        Some(acknowledgement)
    );
    for refused in [
        "1_7_1_500",
        "1_7_1_500/7",
        "1_7_1_500/7_02",
        "1_7_1_500/7_2/1",
    ] {
        assert_eq!(PositionAcknowledgement::parse(refused), None, "{refused:?}");
    }
    let across = latch(LatchKind::AcrossTimelines, promoted);
    assert_eq!(across.stamp(), "branch_1_7_1_500");
    for stored in [across, latch(LatchKind::OnTimeline, passed)] {
        assert_eq!(PositionLatch::parse(&stored.stamp()), Some(stored));
    }
    for refused in ["", "timeline", "other_1_7_1_500", "timeline_1_7_1"] {
        assert_eq!(PositionLatch::parse(refused), None, "{refused:?}");
    }
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
async fn a_declared_restore_invalidates_space_bindings_without_releasing_the_node() {
    let Some(fixture) = GrantFixture::active("hg_restore_space_binding").await else {
        return;
    };
    let pool = owner_pool(&fixture).await;
    let lifecycle = fixture.scope.lifecycle.to_storage();
    sqlx::query(
        "INSERT INTO pubsub_nodes (owner_jid, node_name, access_model, publish_model, \
         node_type, max_items, persist_items, deliver_payloads, notify_retract, notify_delete, \
         send_last_published_item, created_at_ms) \
         SELECT service_jid, space_node, 'whitelist', 'publishers', 'urn:xmpp:spaces:0', \
         100, 1, 1, 1, 1, 'never', 42 FROM host_grant_scope_lifecycles WHERE lifecycle_id = $1",
    )
    .bind(&lifecycle)
    .execute(&pool)
    .await
    .expect("seed the exact enrolled PubSub node");
    let gate = HostGrantGate::with_enrolled_scopes(
        fixture.store.clone(),
        EnrolledScopes::watch(fixture.store.clone()),
    );
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(&fixture.store, served())
            .await
            .expect("attach to the current authority")
    ));
    let key = ScopeKey::new(
        "spaces.example.com".parse().expect("service JID"),
        fixture.scope.space_node.as_str(),
    )
    .expect("Space address");
    let command = BindSpace {
        operation: GrantOperationId::generate(),
        lifecycle: fixture.scope.lifecycle,
        expected_generation: fixture.state(&fixture.scope).await.generation,
        owner: subject(SENDER),
        federated: true,
    };
    let MutationReceipt::Recorded(record) = fixture
        .store
        .bind_space(&command, &served())
        .await
        .expect("bootstrap the Space through the scope journal")
    else {
        panic!("first bind records a new operation");
    };
    assert_eq!(record.kind, OperationKind::BindSpace);
    assert_eq!(Some(record.generation), command.expected_generation.next());
    assert_eq!(
        fixture
            .store
            .bind_space(&command, &served())
            .await
            .expect("retry"),
        MutationReceipt::Replayed(record.clone())
    );
    let changed = BindSpace {
        federated: false,
        ..command
    };
    assert!(matches!(
        fixture.store.bind_space(&changed, &served()).await,
        Err(HostGrantError::OperationConflict)
    ));
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT affiliation, effective_affiliation FROM host_grant_space_affiliations
             WHERE lifecycle_id = $1 AND subject_jid = $2"
        )
        .bind(&lifecycle)
        .bind(SENDER)
        .fetch_one(&pool)
        .await
        .expect("first owner was recorded"),
        ("owner".to_owned(), "none".to_owned())
    );

    assert!(gate
        .space_grant(&key, &subject(SENDER), &PrincipalEvidence::NoStream)
        .await
        .is_none());
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT effective_affiliation FROM host_grant_space_affiliations
             WHERE lifecycle_id = $1 AND subject_jid = $2",
        )
        .bind(&lifecycle)
        .bind(SENDER)
        .fetch_one(&pool)
        .await
        .expect("the verified marker activates the owner"),
        "owner"
    );
    let granted = gate
        .space_grant(&key, &subject(SENDER), &PrincipalEvidence::NoStream)
        .await
        .expect("the owner has an activated decision");
    assert_eq!(
        granted.admit(|standing| standing),
        Some(waddle_xmpp::pubsub::Affiliation::Owner)
    );
    assert!(gate
        .space_grant(&key, &subject(CAROL), &PrincipalEvidence::NoStream)
        .await
        .is_none());
    let wrong_service = ScopeKey::new(
        "spaces.other.test".parse().expect("service JID"),
        key.node(),
    )
    .expect("different service");
    assert!(gate
        .space_grant(
            &wrong_service,
            &subject(SENDER),
            &PrincipalEvidence::NoStream
        )
        .await
        .is_none());
    sqlx::query(
        "INSERT INTO host_grant_space_affiliations VALUES
         ($1, $2, 'member', 'member', 'unprivileged', $4),
         ($1, $3, 'outcast', 'outcast', 'local', $4)",
    )
    .bind(&lifecycle)
    .bind(PEER)
    .bind(CAROL)
    .bind(record.generation.value())
    .execute(&pool)
    .await
    .expect("simulate restored member and outcast rows");

    let binding =
        "SELECT authority_incarnation FROM host_grant_space_bindings WHERE lifecycle_id = $1";
    assert!(sqlx::query_scalar::<_, Option<String>>(binding)
        .bind(&lifecycle)
        .fetch_one(&pool)
        .await
        .expect("bound node before restore")
        .is_some());
    assert!(matches!(
        declare(&fixture, 1).await,
        RestoreDeclaration::Transitioned { .. }
    ));
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(binding)
            .bind(&lifecycle)
            .fetch_one(&pool)
            .await
            .expect("node reservation remains after restore"),
        None
    );

    assert!(gate
        .space_grant(&key, &subject(SENDER), &PrincipalEvidence::NoStream)
        .await
        .is_none());
    let rebind = BindSpace {
        operation: GrantOperationId::generate(),
        owner: subject(OWNER),
        expected_generation: fixture.state(&fixture.scope).await.generation,
        ..changed
    };
    assert!(matches!(
        fixture.store.bind_space(&rebind, &served()).await,
        Err(HostGrantError::SpaceNotBindable)
    ));
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Reconstructed
    );
    assert_eq!(reopen(&fixture, fixture.scope.lifecycle).await.reopened, 1);
    let rebind = BindSpace {
        expected_generation: fixture.state(&fixture.scope).await.generation,
        ..rebind
    };
    assert!(matches!(
        fixture
            .store
            .bind_space(&rebind, &served())
            .await
            .expect("rebind after reopen"),
        MutationReceipt::Recorded(_)
    ));
    let subjects = sqlx::query_as::<_, (String, String, String)>(
        "SELECT subject_jid, affiliation, effective_affiliation
         FROM host_grant_space_affiliations WHERE lifecycle_id = $1",
    )
    .bind(&lifecycle)
    .fetch_all(&pool)
    .await
    .expect("read current Space affiliations");
    assert_eq!(
        subjects,
        [(OWNER.to_owned(), "owner".to_owned(), "none".to_owned())]
    );
    let revoked = sqlx::query_scalar::<_, String>(
        "SELECT subject_jid FROM host_grant_space_operation_changes
         WHERE operation_id = $1 AND affiliation = 'none' ORDER BY subject_jid",
    )
    .bind(rebind.operation.to_storage())
    .fetch_all(&pool)
    .await
    .expect("journal records the discarded affiliations");
    let mut expected = [SENDER, PEER, CAROL];
    expected.sort_unstable();
    assert_eq!(revoked, expected);
    pool.close().await;
    fixture.close().await;
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
            not_reconstructed: 0,
            lost_operations: 0,
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
            not_reconstructed: 1,
            lost_operations: 0,
        }
    );
    assert_eq!(
        fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen all"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 2,
            lost_operations: 0,
        }
    );
    assert!(matches!(
        fixture
            .store
            .reopen_scopes(ReopenSelection::Scope {
                lifecycle: ScopeLifecycleId::generate(),
                lost_operations_reapplied: false,
            })
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
        fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen all"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 2,
            lost_operations: 0,
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
        fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen all"),
        ReopenReport {
            reopened: 1,
            not_reconstructed: 1,
            lost_operations: 0,
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

    // A replica that starts with the configuration from before the declared
    // restore stays unattached and reports itself not ready as well: only a
    // restart with the current configuration serves managed rooms.
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
    assert!(!gate.ready());
    // Readiness answers 503 and names the replica's own failure.
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 503);
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["host_grants"], "configuration_predates_restore");
    assert!(starting.drain_and_join(Duration::from_secs(5)).await);

    // A replica configured ahead of the declared epoch (the owner phase has
    // not declared it yet, which every replica of a rollout shares) stays
    // ready, refuses managed rooms and names the failure.
    let ahead = fixture.ingress.authority().await;
    let gate = ahead.host_grant_gate();
    gate.install_verifier_for_test(
        fixture.store.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::new(2),
    );
    assert_eq!(
        epoch_mismatch(gate.verify_once().await.expect_err("a later epoch")),
        (RestoreEpoch::new(2), RestoreEpoch::new(1))
    );
    assert_eq!(
        gate.readiness(),
        GateReadiness::Refusing(ReplicaFailure::ConfigurationAheadOfRestore)
    );
    assert!(gate.ready());
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["host_grants"], "configuration_ahead_of_restore");
    assert!(ahead.drain_and_join(Duration::from_secs(5)).await);
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

/// A refusal of the whole authority that every replica shares (here an
/// adopted lineage, as a failover the position witness cannot verify is)
/// keeps the replica ready: managed rooms are refused, rooms no scope
/// manages are served, and the replica stays in the endpoint set.
#[tokio::test]
async fn a_refused_authority_keeps_the_replica_ready_and_refuses_managed_rooms() {
    let Some(fixture) = GrantFixture::active("hg_restore_ready").await else {
        return;
    };
    assert_eq!(declare(&fixture, 0).await, RestoreDeclaration::LineageBound);

    // The database is adopted under a running replica, whose attachment
    // stands in for one made before a restart of PostgreSQL: the next
    // acceptance finds the binding changed and the gate verifies again,
    // which the fence refuses.
    let pool = owner_pool(&fixture).await;
    sqlx::query("UPDATE _lineage SET lineage_uuid = $1 WHERE id = 1")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .expect("adoption");
    pool.close().await;
    let mut stale = fixture.store.authority_binding().await.expect("binding");
    stale.server_started_at_us -= 1;
    let replica = fixture.ingress.authority().await;
    let gate = replica.host_grant_gate();
    gate.install_verifier_for_test(
        fixture.store.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::INITIAL,
    );
    assert!(replica.attach_host_grants(HostGrantAttachment::with_binding_for_test(served(), stale)));
    let refused = fixture.room_submission("ready-refused", "refused");
    assert_eq!(
        replica.commit(&refused).await.class,
        IngressDecisionClass::PolicyDenied
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while gate.state() != GateState::ReverifyFailed {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the verification after the adoption did not fail"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gate.ready());
    assert!(!gate.verified());
    // Readiness answers 200 and says the authority is unverified.
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["host_grants"], "unverified");
    let still = fixture.room_submission("ready-still", "still refused");
    let decision = replica.commit(&still).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    let unmanaged = room_submission(
        &fixture.ingress,
        &host_grant_support::bare("open@muc.example.com"),
        "ready-unmanaged",
        "local traffic",
        &[PEER_RESOURCE],
    );
    assert_eq!(
        replica.commit(&unmanaged).await.class,
        IngressDecisionClass::Accepted
    );
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

/// The periodic pass keeps an attachment through passes that cannot
/// complete, up to the bound, and then drops it; it drops it at once for a
/// runtime role that no longer matches the profile.
#[tokio::test]
async fn the_periodic_pass_keeps_or_drops_the_attachment() {
    let Some(fixture) = GrantFixture::active("hg_restore_watch_bound").await else {
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

    // The authority relation is locked away from every pass: each one
    // waits for its lock bound (5 s) and does not complete.
    let pool = owner_pool(&fixture).await;
    let mut blocker = pool.begin().await.expect("blocking transaction");
    sqlx::query("LOCK TABLE host_grant_authority IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .expect("lock the authority");
    let started = tokio::time::Instant::now();
    // After one incomplete pass the attachment is kept.
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert_eq!(gate.state(), GateState::Attached);
    assert!(gate.current().is_some());
    // After the bound it is dropped.
    let deadline = started + Duration::from_secs(60);
    while gate.state() != GateState::ReverifyFailed {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the attachment outlived {} incomplete passes",
            waddle_server::permissions::host_grant::MAX_INCOMPLETE_PASSES
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gate.current().is_none());
    // Verifications that keep failing to complete past the grace are named
    // by readiness, which stays ready: an outage every replica shares must
    // not take every replica out of the endpoint set.
    let deadline = started
        + waddle_server::permissions::host_grant::INCOMPLETE_READINESS_GRACE
        + Duration::from_secs(60);
    while gate.readiness() != GateReadiness::Refusing(ReplicaFailure::FenceIncomplete) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "incomplete verifications were not named by readiness"
        );
        assert!(gate.ready());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 200);
    assert_eq!(body["host_grants"], "fence_incomplete");
    // Released, the gate attaches again by itself.
    blocker.rollback().await.expect("release the lock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while gate.state() != GateState::Attached {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the gate did not attach again"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gate.ready());
    assert_eq!(gate.readiness(), GateReadiness::Verified);
    pool.close().await;
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);

    // A verifier that checks the runtime role's privileges, on a login that
    // owns the schema: the periodic pass drops the attachment, and each
    // later pass that verifies again is refused the same way.
    let replica = Arc::new(fixture.ingress.authority().await);
    let gate = replica.host_grant_gate();
    let stop = tokio_util::sync::CancellationToken::new();
    gate.install_verifier(
        fixture.store.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::INITIAL,
        stop.clone(),
    );
    assert!(replica.attach_host_grants(
        HostGrantAttachment::unfenced_for_test(&fixture.store, served())
            .await
            .expect("attachment")
    ));
    gate.watch(Duration::from_millis(100));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while gate.state() != GateState::ReverifyFailed {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the periodic pass kept an attachment the privilege check refuses"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gate.current().is_none());
    gate.settled().await;
    assert_eq!(gate.state(), GateState::ReverifyFailed);
    // A runtime role that does not match the profile is the owner phase's
    // grant matrix, which every replica shares: readiness names it and
    // stays ready.
    assert_eq!(
        gate.readiness(),
        GateReadiness::Refusing(ReplicaFailure::RuntimeProfileMismatch)
    );
    assert_eq!(host_grant_readiness_response(&gate).0, 200);
    stop.cancel();
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

/// A replica configured with an epoch that predates the declared restore
/// stays not ready through verifications that cannot complete, however
/// long they fail: only a verification that reaches the epoch can show the
/// configuration changed, and an outage must not report the replica ready.
#[tokio::test]
async fn incomplete_verifications_keep_a_configuration_that_predates_the_restore() {
    let Some(fixture) = GrantFixture::active("hg_restore_predates_incomplete").await else {
        return;
    };
    declare(&fixture, 1).await;
    let replica = Arc::new(fixture.ingress.authority().await);
    let gate = replica.host_grant_gate();
    gate.install_verifier_for_test(
        fixture.store.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::INITIAL,
    );
    assert_eq!(
        epoch_mismatch(gate.verify_once().await.expect_err("an older epoch")),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    let not_ready = GateReadiness::NotReady(ReplicaFailure::ConfigurationPredatesRestore);
    assert_eq!(gate.readiness(), not_ready);

    // The authority relation is locked away from every verification: each
    // one waits for its lock bound and does not complete. They are repeated
    // until more than the bound in a row have failed over more than the
    // grace, which names an outage on a replica configured correctly.
    let pool = owner_pool(&fixture).await;
    let mut blocker = pool.begin().await.expect("blocking transaction");
    sqlx::query("LOCK TABLE host_grant_authority IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .expect("lock the authority");
    let started = tokio::time::Instant::now();
    let mut incomplete = 0;
    while incomplete < waddle_server::permissions::host_grant::MAX_INCOMPLETE_PASSES
        || started.elapsed()
            < waddle_server::permissions::host_grant::INCOMPLETE_READINESS_GRACE
                + Duration::from_secs(1)
    {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "verifications kept completing or failing too slowly"
        );
        let error = gate.verify_once().await.expect_err("a locked authority");
        assert!(
            !matches!(error, HostGrantError::RestoreFence(_))
                && waddle_server::permissions::host_grant::verification_retried(&error),
            "expected a verification that does not complete, got {error:?}"
        );
        incomplete += 1;
        assert_eq!(gate.readiness(), not_ready);
    }
    assert!(!gate.ready());
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 503);
    assert_eq!(body["host_grants"], "configuration_predates_restore");
    blocker.rollback().await.expect("release the lock");
    pool.close().await;
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

/// A replica whose runtime role does not match the profile stays ready,
/// refuses managed rooms and verifies again at each periodic pass: once the
/// owner phase corrects the role's grants, it attaches without a restart.
#[tokio::test]
async fn a_corrected_runtime_role_attaches_without_a_restart() {
    let Some(fixture) = GrantFixture::active("hg_restore_profile").await else {
        return;
    };
    let admin = owner_pool(&fixture).await;
    let role = format!("hg_restore_profile_{}", Uuid::new_v4().simple());
    let password = Uuid::new_v4().simple().to_string();
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&admin)
        .await
        .expect("database name");
    // Exactly the runtime profile on the host-grant relations, plus the
    // lineage the fence reads, and one privilege too many: DELETE on the
    // grant rows. The database is this case's own, so its default TEMP
    // privilege is revoked from PUBLIC as the owner phase's provisioning
    // does.
    for statement in [
        format!(
            "CREATE ROLE {role} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB \
             NOCREATEROLE NOREPLICATION NOBYPASSRLS"
        ),
        format!("REVOKE TEMP ON DATABASE {database} FROM PUBLIC"),
        format!("GRANT CONNECT ON DATABASE {database} TO {role}"),
        format!("GRANT USAGE ON SCHEMA public TO {role}"),
        format!(
            "GRANT SELECT ON host_grant_authority, host_grant_scope_lifecycles, \
             host_grant_rooms, _lineage TO {role}"
        ),
        format!("GRANT SELECT ON host_grant_space_bindings, host_grant_space_operation_changes TO {role}"),
        format!("GRANT SELECT, UPDATE ON host_grant_space_affiliations TO {role}"),
        format!("GRANT SELECT, UPDATE ON host_grant_scopes TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON host_grants TO {role}"),
        format!(
            "GRANT SELECT, INSERT ON host_grant_operations, host_grant_operation_changes \
             TO {role}"
        ),
        format!(
            "GRANT SELECT, INSERT, UPDATE ON host_grant_fence_replicas, host_grant_fence_acks \
             TO {role}"
        ),
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
        "host-grant-restore-profile",
        &DatabaseConfig::new(DatabaseDriver::Postgres, runtime_url.to_string()),
    )
    .await
    .expect("runtime database");
    let runtime = HostGrantStore::open(runtime_db.clone()).expect("runtime store");

    let replica = Arc::new(fixture.ingress.authority().await);
    let gate = replica.host_grant_gate();
    let stop = tokio_util::sync::CancellationToken::new();
    gate.install_verifier(
        runtime.clone(),
        local_permissions(&fixture),
        served(),
        RestoreEpoch::INITIAL,
        stop.clone(),
    );
    assert!(matches!(
        gate.verify_once().await,
        Err(HostGrantError::UnsafeRuntimePrivileges {
            component: "host_grants"
        })
    ));
    assert_eq!(gate.state(), GateState::Unattached);
    assert_eq!(
        gate.readiness(),
        GateReadiness::Refusing(ReplicaFailure::RuntimeProfileMismatch)
    );
    let (status, body) = host_grant_readiness_response(&gate);
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["host_grants"], "runtime_profile_mismatch");
    let refused = fixture.room_submission("profile-refused", "refused");
    let decision = replica.commit(&refused).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);

    // The periodic pass verifies again; the corrected grants attach it.
    gate.watch(Duration::from_millis(100));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(gate.state(), GateState::Unattached);
    sqlx::query(&format!("REVOKE DELETE ON host_grants FROM {role}"))
        .execute(&admin)
        .await
        .expect("correct the grants");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while gate.state() != GateState::Attached {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a corrected runtime role was not attached"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(gate.readiness(), GateReadiness::Verified);
    stop.cancel();
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);

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
        format!("GRANT SELECT ON host_grant_space_bindings, host_grant_space_operation_changes TO {role}"),
        format!("GRANT SELECT, UPDATE ON host_grant_space_affiliations TO {role}"),
        format!("GRANT SELECT, UPDATE ON host_grant_scopes TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE ON host_grants TO {role}"),
        format!(
            "GRANT SELECT, INSERT ON host_grant_operations, host_grant_operation_changes \
             TO {role}"
        ),
        format!(
            "GRANT SELECT, INSERT, UPDATE ON host_grant_fence_replicas, host_grant_fence_acks \
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
    let witnessed = PositionWitness {
        epoch: RestoreEpoch::INITIAL,
        position: owner_position,
    };
    assert!(matches!(
        runtime
            .prepare_restore_incarnation(RestoreEpoch::new(1))
            .await,
        Err(HostGrantError::OwnerPhaseRequired)
    ));
    assert!(matches!(
        runtime.acknowledge_position(witnessed).await,
        Err(HostGrantError::OwnerPhaseRequired)
    ));
    assert_eq!(
        fixture.store.authority_binding().await.expect("binding"),
        binding
    );
    assert_eq!(fixture.state(&fixture.scope).await, before);
    // The owner records an acknowledgement bound to the witness it
    // supersedes and to the system and timeline the server writes on.
    assert_eq!(
        fixture
            .store
            .acknowledge_position(witnessed)
            .await
            .expect("ack"),
        PositionAcknowledgement {
            supersedes: witnessed,
            system_identifier: owner_position.system_identifier,
            timeline: owner_position.timeline,
        }
    );
    // One that could never apply is refused: a witness of another system,
    // of a later timeline than the server's, or of an earlier timeline at a
    // position the server's timeline is below (a regression across
    // timelines).
    for unusable in [
        PostgresPosition {
            system_identifier: owner_position.system_identifier ^ 1,
            ..owner_position
        },
        PostgresPosition {
            timeline: owner_position.timeline + 1,
            ..owner_position
        },
        PostgresPosition {
            timeline: owner_position.timeline.saturating_sub(1),
            lsn: owner_position.lsn + (1 << 40),
            ..owner_position
        },
    ] {
        assert!(matches!(
            fixture
                .store
                .acknowledge_position(PositionWitness {
                    epoch: RestoreEpoch::INITIAL,
                    position: unusable,
                })
                .await,
            Err(HostGrantError::AcknowledgementRefused)
        ));
    }
    // Outside the attested pool the system identifier comes from
    // pg_control_system(): a role that may not run it cannot read the
    // position. (On the attested pool it is the attested identity's; see
    // the restricted runtime suite.)
    sqlx::query("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
        .execute(&admin)
        .await
        .expect("revoke the control function");
    assert!(matches!(
        runtime.postgres_position().await,
        Err(HostGrantError::RestoreFence(
            RestoreFenceRefusal::PositionUnreadable
        ))
    ));

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
        // reads every one, and a lifecycle another case left behind would
        // count as a lost enrollment. The store is emptied only if no
        // position witness in it was written outside these suites.
        empty_disposable_store(&raw).await;
        let mut config = ServerConfig::test_homeserver();
        config.spicedb = Some(SpiceDbConfig {
            endpoint,
            preshared_key,
            insecure: true,
        });
        let live = Self {
            fixture,
            config,
            raw,
        };
        // The store's codec is marked by a replica's startup steps; then it
        // is marked as a test store.
        live.actor(&live.fixture.store).await;
        mark_test_store(&live.raw).await;
        live
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

    /// A production actor without the host-grant guard: a guarded actor
    /// answers no legacy read of an enrolled scope's objects.
    async fn unguarded_actor(&self) -> ActorRef<PermissionActor> {
        let backend = PermissionActor::from_server_config(&self.config)
            .await
            .expect("production SpiceDB backend");
        PermissionActor::spawn(backend)
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
        replace_witness(
            actor,
            position_witness_resource(&self.authority_id().await),
            previous.map(|witness| witness.stamp()),
            Some(next.stamp()),
        )
        .await;
    }

    /// The authority's regression latch as SpiceDB holds it.
    async fn latch(&self) -> Option<PositionLatch> {
        match self
            .stamps_of(&latch_witness_resource(&self.authority_id().await))
            .await
            .as_slice()
        {
            [] => None,
            [stamp] => Some(PositionLatch::parse(stamp).expect("well-formed latch")),
            _ => panic!("one latch"),
        }
    }

    /// Add `stamp` to one resource of the marker definition beside what it
    /// holds, as a writer that does not condition on it could.
    async fn touch_stamp(&self, resource: &str, stamp: &str) {
        let relationship = Relationship::new(
            ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, resource).expect("resource"),
            HOST_GRANT_PROJECTION_MARKER_RELATION,
            SubjectReference::new(
                ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, stamp).expect("stamp"),
                None::<String>,
            )
            .expect("subject"),
        );
        self.raw
            .write_relationships(vec![RelationshipUpdate::touch(relationship)])
            .await
            .expect("touch the stamp");
    }

    async fn close(self) {
        self.fixture.close().await;
    }
}

/// Replace one witness resource's stamp, conditioned on exactly `previous`.
async fn replace_witness(
    actor: &ActorRef<PermissionActor>,
    resource: String,
    previous: Option<String>,
    next: Option<String>,
) {
    let written = actor
        .ask(ReplaceHostGrantWitnesses {
            changes: vec![WitnessChange {
                resource,
                previous,
                next,
            }],
        })
        .await
        .expect("write the witness")
        .expect("a SpiceDB backend");
    assert_eq!(written, ProjectionWrite::Applied);
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

/// What SpiceDB itself answers for a channel permission, read through an
/// actor without the host-grant guard.
async fn spicedb_allows(
    live: &Live,
    account: &str,
    permission: Permission,
    channel: &Object,
) -> bool {
    live.unguarded_actor()
        .await
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
    assert!(state.lost_operations);
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
    reopen_after_reapplying(&live.fixture, lifecycle).await;
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
    assert!(!spicedb_allows(&live, SENDER, Permission::Read, &channel).await);
    assert!(spicedb_allows(&live, PEER, Permission::Read, &channel).await);
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
    reopen_after_reapplying(&live.fixture, lifecycle).await;
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
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let fence = || verify_restore_fence(store, &actor, RestoreEpoch::INITIAL);
    let refused = |outcome: Result<RestoreFenceReport, HostGrantError>| {
        fence_refusal(outcome.expect_err("a refused position"))
    };

    // A mutation's witness writer never writes the first position witness;
    // a fence pass that passed does.
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    assert_eq!(live.position_witness(&actor).await, None);
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert_eq!(report.position, Some(PositionStanding::Unrecorded));
    let written = live
        .position_witness(&actor)
        .await
        .expect("position witness");
    assert_eq!(written.epoch, RestoreEpoch::INITIAL);
    assert!(matches!(
        fence().await.expect("fence").position,
        Some(PositionStanding::Current | PositionStanding::Advanced)
    ));

    // A promotion: the witness names an earlier timeline of the same
    // system. The authority is refused until an operator acknowledges that
    // exact witness; no writer moves it meanwhile.
    let before_promotion = PositionWitness {
        position: PostgresPosition {
            timeline: written.position.timeline - 1,
            lsn: 0,
            ..written.position
        },
        ..written
    };
    live.write_position_witness(&actor, before_promotion).await;
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionUnverified
    );
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(live.position_witness(&actor).await, Some(before_promotion));
    store
        .acknowledge_position(written)
        .await
        .expect("an acknowledgement of another witness");
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionUnverified
    );
    store
        .acknowledge_position(before_promotion)
        .await
        .expect("acknowledgement");
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert_eq!(report.position, Some(PositionStanding::Acknowledged));
    let moved = live.position_witness(&actor).await.expect("moved witness");
    assert_eq!(moved.position.timeline, written.position.timeline);
    // Applied, the acknowledgement is spent: it names a witness that is no
    // longer the current one, so a later promotion is not excused by it.
    let another_promotion = PositionWitness {
        position: PostgresPosition {
            lsn: 1,
            ..before_promotion.position
        },
        ..before_promotion
    };
    live.write_position_witness(&actor, another_promotion).await;
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionUnverified
    );
    // A witness of a later timeline than the server's cannot be
    // acknowledged at all: that takes a declared restore.
    let after_promotion = PositionWitness {
        position: PostgresPosition {
            timeline: written.position.timeline + 1,
            lsn: 0,
            ..written.position
        },
        ..written
    };
    live.write_position_witness(&actor, after_promotion).await;
    assert!(matches!(
        store.acknowledge_position(after_promotion).await,
        Err(HostGrantError::AcknowledgementRefused)
    ));
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionUnverified
    );

    // The database went back on its own timeline: the witness is ahead of
    // the server. The regression is latched beside the witness.
    let current = store.postgres_position().await.expect("position");
    let ahead = PositionWitness {
        epoch: RestoreEpoch::INITIAL,
        position: PostgresPosition {
            lsn: current.lsn + (1 << 40),
            ..current
        },
    };
    live.write_position_witness(&actor, ahead).await;
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionRegressed
    );
    let latched = PositionLatch {
        kind: LatchKind::OnTimeline,
        witness: ahead,
    };
    assert_eq!(live.latch().await, Some(latched));
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(live.position_witness(&actor).await, Some(ahead));
    // A replica keeps the latch it saw in memory, and writes it back when
    // SpiceDB lost it.
    let memory = FenceMemory::default();
    assert_eq!(
        fence_refusal(
            verify_restore_fence_with(store, &actor, RestoreEpoch::INITIAL, &memory)
                .await
                .expect_err("latched")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    assert_eq!(memory.latch(), Some(latched));
    let latch_resource = latch_witness_resource(&live.authority_id().await);
    replace_witness(&actor, latch_resource.clone(), Some(latched.stamp()), None).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence_with(store, &actor, RestoreEpoch::INITIAL, &memory)
                .await
                .expect_err("latched in memory")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    assert_eq!(live.latch().await, Some(latched));

    // Once the server has written past the witnessed position again, the
    // position alone would pass; the latch keeps the regression. (The WAL of
    // the shared test cluster cannot be driven past a chosen point, so the
    // case stands for it with a witness below the server and its latch.)
    let passed = PositionWitness {
        epoch: RestoreEpoch::INITIAL,
        position: PostgresPosition {
            lsn: current.lsn - 1,
            ..current
        },
    };
    live.write_position_witness(&actor, passed).await;
    let passed_latch = PositionLatch {
        kind: LatchKind::OnTimeline,
        witness: passed,
    };
    replace_witness(
        &actor,
        latch_resource.clone(),
        Some(latched.stamp()),
        Some(passed_latch.stamp()),
    )
    .await;
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionRegressed
    );
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness writer");
    assert_eq!(live.position_witness(&actor).await, Some(passed));
    // An acknowledgement bound to that witness clears it: the witness moves
    // to the current position and the latch goes with it.
    store
        .acknowledge_position(passed)
        .await
        .expect("acknowledgement of the regression");
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert_eq!(report.position, Some(PositionStanding::Acknowledged));
    assert_eq!(live.latch().await, None);
    let rebased = live.position_witness(&actor).await.expect("witness");
    assert_eq!(rebased.position.timeline, current.timeline);
    assert!(rebased.position.lsn >= current.lsn);

    // Below the witness on another timeline is a regression across
    // timelines: latched as well, and no acknowledgement clears it, also
    // once the server is past the witnessed position.
    let position = store.postgres_position().await.expect("position");
    let crossed = PositionWitness {
        epoch: RestoreEpoch::INITIAL,
        position: PostgresPosition {
            timeline: position.timeline - 1,
            lsn: position.lsn + (1 << 40),
            ..position
        },
    };
    live.write_position_witness(&actor, crossed).await;
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionRegressed
    );
    let crossed_latch = PositionLatch {
        kind: LatchKind::AcrossTimelines,
        witness: crossed,
    };
    assert_eq!(live.latch().await, Some(crossed_latch));
    let crossed_passed = PositionWitness {
        position: PostgresPosition {
            lsn: position.lsn - 1,
            ..crossed.position
        },
        ..crossed
    };
    live.write_position_witness(&actor, crossed_passed).await;
    replace_witness(
        &actor,
        latch_resource,
        Some(crossed_latch.stamp()),
        Some(
            PositionLatch {
                kind: LatchKind::AcrossTimelines,
                witness: crossed_passed,
            }
            .stamp(),
        ),
    )
    .await;
    store
        .acknowledge_position(crossed_passed)
        .await
        .expect("an acknowledgement for a later timeline");
    assert_eq!(
        refused(fence().await),
        RestoreFenceRefusal::PositionRegressed
    );
    // A replica's attachment is refused with it.
    assert_eq!(
        fence_refusal(
            HostGrantAttachment::fenced_for_test(store, &actor, served(), RestoreEpoch::INITIAL)
                .await
                .expect_err("no attachment")
        ),
        RestoreFenceRefusal::PositionRegressed
    );

    // A declared restore is the acknowledgement of what was lost: the
    // witness is replaced under the declared epoch and the latch removed.
    declare(&live.fixture, 1).await;
    let report = fence_and_witness(store, &actor, RestoreEpoch::new(1)).await;
    assert_eq!(report.position, Some(PositionStanding::Rebased));
    assert_eq!(
        live.position_witness(&actor)
            .await
            .expect("witness of the declared epoch")
            .epoch,
        RestoreEpoch::new(1)
    );
    assert_eq!(live.latch().await, None);
    live.close().await;
}

/// A retirement is an owner-phase act: nothing witnesses it at its commit,
/// and the next fence pass that passes does, retired as the scope is. A
/// restore that lost the retirement then closes the scope again.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_lost_retirement_is_found_by_its_journal_witness() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_retired").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    let backup = AuthorityBackup::take(&live.fixture).await;

    store
        .retire_scope(&RetireScope {
            operation: GrantOperationId::generate(),
            lifecycle,
            expected_generation: live.fixture.generation().await,
        })
        .await
        .expect("retirement");
    let retired = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(retired.closed_reason, Some(ClosedReason::Retired));
    let OperationOutcome::Committed(record) = store
        .operation_outcome(retired.operation)
        .await
        .expect("readback")
    else {
        panic!("the retirement is recorded");
    };
    let retirement = ProjectionMarker {
        lifecycle,
        incarnation: record.authority_incarnation,
        generation: record.generation,
        operation: record.operation,
        digest: record.projection_digest,
    };
    let head = head_witness_resource(lifecycle);
    assert_ne!(live.stamps_of(&head).await, vec![retirement.stamp()]);
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert_eq!(report.fenced, 0);
    assert_eq!(live.stamps_of(&head).await, vec![retirement.stamp()]);

    // Restored to before the retirement, the scope admits again in the
    // database; its journal witness shows what it lost, and the fence
    // closes it.
    backup.restore(&live.fixture).await;
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    let report = verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence of a lost retirement");
    assert_eq!(report.fenced, 1);
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Fenced));
    assert!(state.lost_operations);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    live.close().await;
}

/// A SpiceDB that holds no marker and no witness of this authority while
/// scopes admit is not this deployment's projection, also when it holds
/// another deployment's markers: the authority is refused, nothing is
/// closed, and the refusal clears when the projection is back. With a
/// position witness, the same loss is SpiceDB losing markers, and each
/// admitting scope is closed.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_spicedb_without_the_projection_refuses_without_closing() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_absent").await;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    let other = live.fixture.enroll_another().await;
    let mut markers = Vec::new();
    for lifecycle in [live.fixture.scope.lifecycle, other.lifecycle] {
        authority.reconcile(lifecycle).await.expect("first pass");
        let stamps = live.stamps_of(&lifecycle.to_storage()).await;
        markers.push((lifecycle, stamps[0].clone()));
    }
    let drop_markers = || async {
        for (lifecycle, stamp) in &markers {
            replace_witness(&actor, lifecycle.to_storage(), Some(stamp.clone()), None).await;
        }
    };
    // The room witnesses written with the projection name this authority's
    // lifecycles too; a SpiceDB that is not this deployment's holds none.
    let rooms: Vec<(String, String)> = [&live.fixture.scope, &other]
        .into_iter()
        .map(|scope| {
            (
                room_witness_resource(&scope.channel_id),
                scope.lifecycle.to_storage(),
            )
        })
        .collect();
    for (resource, lifecycle) in &rooms {
        replace_witness(&actor, resource.clone(), Some(lifecycle.clone()), None).await;
    }

    drop_markers().await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("no projection of this deployment")
        ),
        RestoreFenceRefusal::ProjectionAbsent
    );
    // Another deployment's markers do not make it this deployment's.
    let foreign = ProjectionMarker {
        lifecycle: ScopeLifecycleId::generate(),
        ..ProjectionMarker::parse(markers[0].0, &markers[0].1).expect("marker")
    };
    replace_witness(
        &actor,
        foreign.lifecycle.to_storage(),
        None,
        Some(foreign.stamp()),
    )
    .await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("another deployment's projection")
        ),
        RestoreFenceRefusal::ProjectionAbsent
    );
    assert_eq!(live.position_witness(&actor).await, None);
    replace_witness(
        &actor,
        foreign.lifecycle.to_storage(),
        Some(foreign.stamp()),
        None,
    )
    .await;
    for scope in [&live.fixture.scope, &other] {
        assert_eq!(
            live.fixture.state(scope).await.admission,
            AdmissionState::Active
        );
    }
    // The projection is back: the fence passes and closes nothing.
    for (lifecycle, stamp) in &markers {
        replace_witness(&actor, lifecycle.to_storage(), None, Some(stamp.clone())).await;
    }
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert_eq!(report.fenced, 0);
    assert!(live.position_witness(&actor).await.is_some());

    // Beside this authority's position witness, markers that are gone were
    // lost by SpiceDB: each admitting scope is closed.
    drop_markers().await;
    let report = verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence");
    assert_eq!(report.fenced, 2);
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Fenced));
    assert!(!state.lost_operations);
    live.close().await;
}

/// A journal witness of an incarnation the scope's journal never held is
/// not one a declared restore superseded, so a reopen does not explain it:
/// it closes the scope.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_journal_witness_of_an_unknown_incarnation_closes_a_reopened_scope() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_foreign_head").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    declare(&live.fixture, 1).await;
    authority
        .reconcile(lifecycle)
        .await
        .expect("reconstruction");
    assert_eq!(reopen(&live.fixture, lifecycle).await.reopened, 1);
    authority.reconcile(lifecycle).await.expect("reopen pass");
    fence_and_witness(store, &actor, RestoreEpoch::new(1)).await;
    let head = head_witness_resource(lifecycle);
    let witnessed = live.stamps_of(&head).await;
    assert_eq!(witnessed, vec![live.current_marker().await.stamp()]);

    let foreign = ProjectionMarker {
        incarnation: AuthorityIncarnation::generate(),
        generation: generation(99),
        ..live.current_marker().await
    };
    replace_witness(
        &actor,
        head.clone(),
        Some(witnessed[0].clone()),
        Some(foreign.stamp()),
    )
    .await;
    assert_eq!(
        authority
            .sync_witnesses(lifecycle)
            .await
            .expect("witness writer"),
        waddle_server::permissions::host_grant::WitnessSync::Kept
    );
    let report = verify_restore_fence(store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence");
    assert_eq!(report.fenced, 1);
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Fenced)
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_reconstruction_keeps_lost_bans_and_grants_nothing_from_the_projection() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_bans").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
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
    reopen_after_reapplying(&live.fixture, lifecycle).await;

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
        assert!(!spicedb_allows(&live, account, Permission::Read, &channel).await);
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
    // A remote account of a trusted home holds an admin grant, which no
    // decision honours without that home's `pkix` stream, and none on a
    // replica that runs no S2S snapshot.
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
    let (authority, _actor) = live.replica().await;
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
    assert!(!spicedb_allows(&live, EVE, Permission::Read, &channel).await);
    assert!(spicedb_allows(&live, CAROL, Permission::Read, &channel).await);
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
            .take_over(&restored, Some(&stale), None)
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
    fence_and_witness(&live.fixture.store, &actor, RestoreEpoch::new(1)).await;

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
    reopen_after_reapplying(&live.fixture, lifecycle).await;
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
    let open_room = host_grant_support::bare("open@muc.example.com");
    let replica = live.fixture.ingress.authority().await;
    let gate = replica.host_grant_gate();
    gate.install_verifier_for_test(
        live.fixture.store.clone(),
        actor.clone(),
        served(),
        RestoreEpoch::INITIAL,
    );
    // Before any fence pass has read the rooms, the gate asks SpiceDB about
    // the room an acceptance names.
    assert!(gate.memory().refused_rooms().is_none());
    assert!(gate.refuses_unmanaged(&later.room).await);
    assert!(!gate.refuses_unmanaged(&open_room).await);
    gate.verify_once().await.expect("verification");
    assert!(gate.refuses_unmanaged(&later.room).await);
    assert!(!gate.refuses_unmanaged(&live.fixture.scope.room).await);
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
        &open_room,
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
    // A replica whose fence refuses the authority (its configuration
    // predates the declared restore) holds no attachment, and still refuses
    // the lost enrollment's room: the pass read the rooms before it refused.
    let starting = live.fixture.ingress.authority().await;
    let unattached = starting.host_grant_gate();
    unattached.install_verifier_for_test(
        live.fixture.store.clone(),
        actor.clone(),
        served(),
        RestoreEpoch::INITIAL,
    );
    assert_eq!(
        epoch_mismatch(
            unattached
                .verify_once()
                .await
                .expect_err("a configuration from before the declared restore")
        ),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    assert_eq!(unattached.state(), GateState::Unattached);
    assert!(unattached.memory().refused_rooms().is_some());
    let lost = room_submission(
        &live.fixture.ingress,
        &later.room,
        "lost-enrollment-unattached",
        "refused",
        &[PEER_RESOURCE],
    );
    let decision = starting.commit(&lost).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);
    let unmanaged = room_submission(
        &live.fixture.ingress,
        &open_room,
        "never-managed-unattached",
        "local traffic",
        &[PEER_RESOURCE],
    );
    assert_eq!(
        starting.commit(&unmanaged).await.class,
        IngressDecisionClass::Accepted
    );
    assert!(starting.drain_and_join(Duration::from_secs(5)).await);
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
    replace_witness(
        &actor,
        lifecycle.to_storage(),
        Some(current.stamp()),
        Some(first.stamp()),
    )
    .await;
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

/// A restored scope that was never projected has no projection to lower its
/// grants to, but its journal witness still shows the operation the restore
/// lost: its first projection marks it as one that lost operations, moves
/// the witness in the same request, and only a reopen by name with the
/// operator's confirmation reopens it.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_restored_scope_never_projected_keeps_its_lost_operations() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_unprojected").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, _actor) = live.replica().await;
    let head = head_witness_resource(lifecycle);

    // The enrollment is witnessed; its projection is never written.
    assert_eq!(
        authority
            .sync_witnesses(lifecycle)
            .await
            .expect("witness of the enrollment"),
        WitnessSync::Written
    );
    assert!(live.stamps().await.is_empty());
    let backup = AuthorityBackup::take(&live.fixture).await;

    // An operator revokes the sender; the revoke is witnessed.
    operator_revoke(&live.fixture, SENDER, false)
        .await
        .expect("operator revoke");
    assert_eq!(
        authority
            .sync_witnesses(lifecycle)
            .await
            .expect("witness of the revoke"),
        WitnessSync::Written
    );
    let witnessed = live.stamps_of(&head).await;
    assert_eq!(witnessed.len(), 1);

    // Restored to before the revoke and declared, with no fence pass in
    // between: nothing has marked the scope yet.
    backup.restore(&live.fixture).await;
    declare(&live.fixture, 1).await;
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    assert!(!state.lost_operations);

    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("first projection"),
        Reconciliation::Reconstructed(_)
    ));
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Reconstructed));
    assert!(state.lost_operations);
    // The journal witness moved with the projection, to its operation.
    let marker = live.current_marker().await;
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    assert_eq!(live.stamps_of(&head).await, vec![marker.stamp()]);

    // A reopen of every scope leaves it closed.
    assert_eq!(
        live.fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen of every scope"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 0,
            lost_operations: 1,
        }
    );
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::Read).await);
    // The operator applies the lost revoke again, then reopens it by name
    // with the confirmation.
    operator_revoke(&live.fixture, SENDER, false)
        .await
        .expect("the revoke applied again");
    authority
        .reconcile(lifecycle)
        .await
        .expect("projection of the revoke");
    reopen_after_reapplying(&live.fixture, lifecycle).await;
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    live.close().await;
}

/// A latch the fence could not write is kept in the replica's memory, and
/// the write is tried again; a writer that reads the latch never moves the
/// witness past it. A latch without its witness, and a witness that cannot
/// be read, are refused as unverified until a restore is declared, which
/// removes them.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_latch_survives_a_failed_write_and_unreadable_witnesses_need_a_restore() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_latch_write").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    let authority_id = live.authority_id().await;
    assert_eq!(
        live.stamps_of(&epoch_witness_resource(&authority_id)).await,
        vec!["0".to_owned()]
    );

    // The database went back: the witness is ahead of the server. The
    // latch write fails, and the replica keeps the regression in memory.
    let current = store.postgres_position().await.expect("position");
    let ahead = PositionWitness {
        epoch: RestoreEpoch::INITIAL,
        position: PostgresPosition {
            lsn: current.lsn + (1 << 40),
            ..current
        },
    };
    live.write_position_witness(&actor, ahead).await;
    let latched = PositionLatch {
        kind: LatchKind::OnTimeline,
        witness: ahead,
    };
    let memory = FenceMemory::default();
    assert_eq!(
        fence_refusal(
            with_host_grant_witness_write_fault(verify_restore_fence_with(
                store,
                &actor,
                RestoreEpoch::INITIAL,
                &memory
            ))
            .await
            .expect_err("regressed")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    assert_eq!(memory.latch(), Some(latched));
    assert_eq!(live.latch().await, None);
    // The next pass writes it.
    assert_eq!(
        fence_refusal(
            verify_restore_fence_with(store, &actor, RestoreEpoch::INITIAL, &memory)
                .await
                .expect_err("still regressed")
        ),
        RestoreFenceRefusal::PositionRegressed
    );
    assert_eq!(live.latch().await, Some(latched));
    // A writer reads the latch and keeps the witness.
    assert_eq!(
        sync_position_witness(store, &actor, PositionWriter::Commit, None)
            .await
            .expect("position writer"),
        (WitnessSync::Kept, Some(PositionStanding::Regressed))
    );
    assert_eq!(live.position_witness(&actor).await, Some(ahead));

    // Without its witness, the latch is refused as unverified and kept.
    let position = position_witness_resource(&authority_id);
    replace_witness(&actor, position.clone(), Some(ahead.stamp()), None).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a latch without its witness")
        ),
        RestoreFenceRefusal::PositionUnverified
    );
    for writer in [PositionWriter::Fence, PositionWriter::Commit] {
        sync_position_witness(store, &actor, writer, None)
            .await
            .expect("position writer");
        assert_eq!(live.position_witness(&actor).await, None);
        assert_eq!(live.latch().await, Some(latched));
    }

    // A position witness that cannot be read is refused as unverified, and
    // nothing removes it without a declared restore.
    replace_witness(&actor, position.clone(), None, Some(ahead.stamp())).await;
    live.touch_stamp(&position, "unreadable").await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("an unreadable witness")
        ),
        RestoreFenceRefusal::PositionUnverified
    );
    assert_eq!(
        sync_position_witness(store, &actor, PositionWriter::Commit, None)
            .await
            .expect("position writer")
            .0,
        WitnessSync::Malformed
    );
    assert_eq!(live.stamps_of(&position).await.len(), 2);

    // A declared restore removes the unreadable witness and its latch; the
    // first witness of the declared epoch follows the pass.
    declare(&live.fixture, 1).await;
    let report = fence_and_witness(store, &actor, RestoreEpoch::new(1)).await;
    assert_eq!(report.position, Some(PositionStanding::Unrecorded));
    assert_eq!(live.latch().await, None);
    assert_eq!(
        live.position_witness(&actor)
            .await
            .expect("witness of the declared epoch")
            .epoch,
        RestoreEpoch::new(1)
    );
    assert_eq!(
        live.stamps_of(&epoch_witness_resource(&authority_id)).await,
        vec!["1".to_owned()]
    );
    live.close().await;
}

/// A SpiceDB that holds only another deployment's markers is not given a
/// position witness by a fence pass that passes, while nothing admits; one
/// that holds a marker of this authority is.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_another_deployments_spicedb_gets_no_position_witness() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_foreign").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    let foreign = ProjectionMarker {
        lifecycle: ScopeLifecycleId::generate(),
        ..live.current_marker().await
    };
    replace_witness(
        &actor,
        foreign.lifecycle.to_storage(),
        None,
        Some(foreign.stamp()),
    )
    .await;

    // The scope is enrolled and admits nothing yet: the fence passes, and
    // writes no position witness beside another deployment's markers.
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert!(!report.holds_authority);
    assert_eq!(report.fenced, 0);
    assert_eq!(live.position_witness(&actor).await, None);
    // The pass witnessed the scope's journal head; that does not make the
    // SpiceDB this authority's either.
    assert_eq!(
        live.stamps_of(&head_witness_resource(lifecycle))
            .await
            .len(),
        1
    );
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert!(!report.holds_authority);
    assert_eq!(live.position_witness(&actor).await, None);

    // Projected, the scope's marker is this authority's: the next pass
    // writes the first witness.
    assert!(matches!(
        authority.reconcile(lifecycle).await.expect("first pass"),
        Reconciliation::Activated(_)
    ));
    let report = fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    assert!(report.holds_authority);
    assert!(live.position_witness(&actor).await.is_some());
    live.close().await;
}

/// The history both cases of a second restore during restore handling
/// start from: a revoke witnessed and never projected, a restore to before
/// it declared, and the backup of the database right after that
/// declaration, before reconciliation found the lost revoke.
async fn restored_during_restore_handling(
    label: &str,
) -> (
    Live,
    HostGrantAuthority,
    ActorRef<PermissionActor>,
    AuthorityBackup,
) {
    let live = Live::start(label).await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    fence_and_witness(&live.fixture.store, &actor, RestoreEpoch::INITIAL).await;
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    let before_revoke = AuthorityBackup::take(&live.fixture).await;
    let command = live
        .fixture
        .command(OWNER, SENDER, RoomAffiliation::None)
        .await;
    let revoke = authority.set_affiliation(&command).await.expect("revoke");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witness of the revoke");
    let head = live.stamps_of(&head_witness_resource(lifecycle)).await;
    assert_eq!(head.len(), 1);
    assert!(head[0].contains(&revoke.record().operation.to_storage()));

    // Restore #1, to before the revoke, declared.
    before_revoke.restore(&live.fixture).await;
    declare(&live.fixture, 1).await;
    let declared = AuthorityBackup::take(&live.fixture).await;
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Restore
    );

    // Reconciliation finds the lost revoke through the journal witness and
    // marks the scope under an operation of its own; the projection it then
    // writes names that operation, not the restore's.
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(_)
    ));
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Reconstructed));
    assert!(state.lost_operations);
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Reconstruct
    );
    let marker = live.current_marker().await;
    assert_eq!(marker.operation, state.operation);
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    assert_eq!(
        live.stamps_of(&head_witness_resource(lifecycle)).await,
        vec![marker.stamp()]
    );

    // Restore #2: PostgreSQL goes back to after the declaration, before the
    // mark. SpiceDB keeps the marker of the operation that recorded it.
    declared.restore(&live.fixture).await;
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    assert!(!state.lost_operations);
    (live, authority, actor, declared)
}

/// A second restore, declared, to a database that predates the lost
/// operations mark: the marker names an operation the restored journal does
/// not hold, so the reconstruction marks the scope again and a reopen of
/// every scope leaves it closed.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_second_declared_restore_keeps_the_lost_operations_mark() {
    let _serial = LIVE_SPICEDB.lock().await;
    let (live, authority, _actor, _declared) =
        restored_during_restore_handling("hg_restore_twice_declared").await;
    let lifecycle = live.fixture.scope.lifecycle;
    declare(&live.fixture, 2).await;
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(_)
    ));
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Reconstructed));
    assert!(state.lost_operations);
    assert_eq!(
        live.fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen of every scope"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 0,
            lost_operations: 1,
        }
    );
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::Read).await);
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::Read).await);
    live.close().await;
}

/// The same second restore, met with an acknowledged position instead of a
/// declaration: the restored scope's marker is ahead of its journal, so the
/// pass fences it, marked, and a reopen of every scope leaves it closed.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_second_acknowledged_restore_keeps_the_scope_closed() {
    let _serial = LIVE_SPICEDB.lock().await;
    let (live, authority, actor, _declared) =
        restored_during_restore_handling("hg_restore_twice_acknowledged").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let witnessed = live
        .position_witness(&actor)
        .await
        .expect("the position witness");
    live.fixture
        .store
        .acknowledge_position(witnessed)
        .await
        .expect("acknowledge the position");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Contradiction(ProjectionContradiction::AheadOfCanonical)
    );
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Fenced));
    assert!(state.lost_operations);
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Fence
    );
    assert_eq!(
        live.fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen of every scope"),
        ReopenReport::default()
    );
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.closed_reason,
        Some(ClosedReason::Fenced)
    );
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::Read).await);
    live.close().await;
}

/// No first projection is written while scopes admit and SpiceDB holds
/// nothing of this authority: the fence refuses that state as a SpiceDB
/// that is empty or another deployment's, and a first projection written
/// into it would clear the refusal and let the fence close every scope
/// that admits.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_no_first_projection_is_written_into_a_spicedb_without_the_authority() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_absent_first").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    assert!(matches!(
        authority.reconcile(lifecycle).await.expect("first pass"),
        Reconciliation::Activated(_)
    ));

    // The replica now reaches a SpiceDB that holds nothing of this
    // authority, while the scope admits.
    live.raw
        .delete_relationships(RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION))
        .await
        .expect("empty the projection");
    mark_test_store(&live.raw).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a SpiceDB without the authority")
        ),
        RestoreFenceRefusal::ProjectionAbsent
    );

    // A scope enrolled now gets no first projection.
    let later = live.fixture.enroll_another().await;
    assert!(matches!(
        authority.reconcile(later.lifecycle).await,
        Err(HostGrantError::RestoreFence(
            RestoreFenceRefusal::ProjectionAbsent
        ))
    ));
    assert!(live
        .stamps_of(&later.lifecycle.to_storage())
        .await
        .is_empty());
    assert!(live
        .stamps_of(&room_witness_resource(&later.channel_id))
        .await
        .is_empty());
    // The refusal stands and nothing was closed.
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("still a SpiceDB without the authority")
        ),
        RestoreFenceRefusal::ProjectionAbsent
    );
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.admission,
        AdmissionState::Active
    );
    live.close().await;
}

/// Nor is a restored scope projected into such a SpiceDB once a declared
/// restore closed every scope: nothing admits, so the fence has nothing to
/// refuse, but a scope was projected before and SpiceDB holds none of its
/// markers or witnesses. A first projection written there would find no
/// journal witness of what the restore lost, and its marker would pass
/// that SpiceDB off as this deployment's.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_no_restored_scope_is_projected_into_a_spicedb_without_the_authority() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_absent_restored").await;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    // One scope is projected; another is enrolled and never projected.
    assert!(matches!(
        authority
            .reconcile(live.fixture.scope.lifecycle)
            .await
            .expect("first pass"),
        Reconciliation::Activated(_)
    ));
    let unprojected = live.fixture.enroll_another().await;

    // A restore is declared, and the replica reaches a SpiceDB that holds
    // nothing of this authority. No scope admits, so the fence passes.
    declare(&live.fixture, 1).await;
    assert_eq!(
        live.fixture.state(&unprojected).await.closed_reason,
        Some(ClosedReason::Restored)
    );
    live.raw
        .delete_relationships(RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION))
        .await
        .expect("empty the projection");
    mark_test_store(&live.raw).await;
    verify_restore_fence(store, &actor, RestoreEpoch::new(1))
        .await
        .expect("no scope admits");

    // The restored scope that was never projected gets no first projection,
    // and is left as the restore closed it.
    match authority.reconcile(unprojected.lifecycle).await {
        Err(HostGrantError::RestoreFence(RestoreFenceRefusal::ProjectionAbsent)) => {}
        other => panic!("expected the first projection to be refused, got {other:?}"),
    }
    assert!(live
        .stamps_of(&unprojected.lifecycle.to_storage())
        .await
        .is_empty());
    assert!(live
        .stamps_of(&room_witness_resource(&unprojected.channel_id))
        .await
        .is_empty());
    let state = live.fixture.state(&unprojected).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    assert!(!state.lost_operations);
    live.close().await;
}

/// A journal witness whose stamp is not a canonical one cannot show what a
/// restored database lost: the reconstruction marks the scope, under an
/// operation of its own, and stops without moving the witness or taking
/// the projection over. Once an operator removes the witness the scope is
/// reconstructed, and a reopen of every scope still leaves it closed.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_an_unreadable_journal_witness_keeps_a_restored_scope_closed() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_head_unreadable").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    authority
        .sync_witnesses(lifecycle)
        .await
        .expect("witnesses");
    let head = head_witness_resource(lifecycle);
    let witnessed = live.stamps_of(&head).await;
    assert_eq!(witnessed.len(), 1);
    replace_witness(
        &actor,
        head.clone(),
        Some(witnessed[0].clone()),
        Some("not_a_marker".to_owned()),
    )
    .await;
    let projected = live.stamps().await;
    declare(&live.fixture, 1).await;

    for _ in 0..2 {
        assert_eq!(
            authority.reconcile(lifecycle).await.expect("pass"),
            Reconciliation::Contradiction(ProjectionContradiction::MalformedJournalWitness)
        );
    }
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.closed_reason, Some(ClosedReason::Restored));
    assert!(state.lost_operations);
    // One operation recorded the mark; the second pass recorded nothing.
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Reconstruct
    );
    assert_eq!(state.generation, generation(3));
    assert_eq!(live.stamps_of(&head).await, vec!["not_a_marker".to_owned()]);
    assert_eq!(live.stamps().await, projected);

    // The operator removes the witness; the scope is reconstructed, and
    // reopens only by name with the confirmation.
    replace_witness(&actor, head.clone(), Some("not_a_marker".to_owned()), None).await;
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Reconstructed(_)
    ));
    assert_eq!(
        live.fixture
            .store
            .reopen_scopes(ReopenSelection::All)
            .await
            .expect("reopen of every scope"),
        ReopenReport {
            reopened: 0,
            not_reconstructed: 0,
            lost_operations: 1,
        }
    );
    reopen_after_reapplying(&live.fixture, lifecycle).await;
    live.close().await;
}

/// The unreadable position witnesses are removed after a declared restore
/// only when nothing readable beside them was written under the declared
/// epoch, and a removal names one authority's position witness, latch and
/// witnessed epoch only.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_unreadable_witnesses_beside_one_of_the_declared_epoch_are_kept() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_clear_epoch").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    let authority_id = live.authority_id().await;
    let position = position_witness_resource(&authority_id);
    let latch = latch_witness_resource(&authority_id);
    let epoch = epoch_witness_resource(&authority_id);
    assert_eq!(live.stamps_of(&epoch).await, vec!["0".to_owned()]);

    // After a declared restore, a position witness of the declared epoch
    // sits beside an unreadable latch and the witnessed epoch of before.
    declare(&live.fixture, 1).await;
    let declared = PositionWitness {
        epoch: RestoreEpoch::new(1),
        position: store.postgres_position().await.expect("position"),
    };
    live.write_position_witness(&actor, declared).await;
    live.touch_stamp(&latch, "not_a_latch").await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(store, &actor, RestoreEpoch::new(1))
                .await
                .expect_err("an unreadable latch beside a witness of the declared epoch")
        ),
        RestoreFenceRefusal::PositionUnverified
    );
    assert_eq!(live.stamps_of(&position).await, vec![declared.stamp()]);
    assert_eq!(live.stamps_of(&latch).await, vec!["not_a_latch".to_owned()]);
    assert_eq!(live.stamps_of(&epoch).await, vec!["0".to_owned()]);

    // A removal of anything but one authority's position witnesses is
    // refused before a request is sent.
    let condition = WitnessChange {
        resource: position.clone(),
        previous: Some(declared.stamp()),
        next: Some(declared.stamp()),
    };
    for resources in [
        vec![head_witness_resource(lifecycle)],
        vec![lifecycle.to_storage()],
        vec![epoch_witness_resource(&"0".repeat(32))],
    ] {
        assert!(actor
            .ask(ClearHostGrantWitnesses {
                resources,
                condition: condition.clone(),
            })
            .await
            .is_err());
    }
    assert!(actor
        .ask(ClearHostGrantWitnesses {
            resources: vec![latch.clone()],
            condition: WitnessChange {
                resource: head_witness_resource(lifecycle),
                previous: Some("x".to_owned()),
                next: Some("x".to_owned()),
            },
        })
        .await
        .is_err());
    assert_eq!(live.stamps_of(&latch).await, vec!["not_a_latch".to_owned()]);
    live.close().await;
}

/// A witnessed epoch that cannot be read beside a readable position
/// witness is not needed: the fence passes, and the next move of the
/// witness replaces it, also when it holds more than one stamp.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_an_unreadable_epoch_beside_a_readable_witness_is_replaced() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_restore_epoch_unreadable").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let store = &live.fixture.store;
    let (authority, actor) = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    fence_and_witness(store, &actor, RestoreEpoch::INITIAL).await;
    let epoch = epoch_witness_resource(&live.authority_id().await);

    // One stamp that is not an epoch.
    replace_witness(
        &actor,
        epoch.clone(),
        Some("0".to_owned()),
        Some("not_an_epoch".to_owned()),
    )
    .await;
    verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("the fence passes beside a readable witness");
    advance_wal(&live.fixture).await;
    assert_eq!(
        sync_position_witness(store, &actor, PositionWriter::Commit, None)
            .await
            .expect("position writer"),
        (WitnessSync::Written, Some(PositionStanding::Advanced))
    );
    assert_eq!(live.stamps_of(&epoch).await, vec!["0".to_owned()]);

    // Two stamps: no write can be conditioned on them, so they are removed
    // first.
    live.touch_stamp(&epoch, "7").await;
    assert_eq!(live.stamps_of(&epoch).await.len(), 2);
    verify_restore_fence(store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("the fence passes beside a readable witness");
    advance_wal(&live.fixture).await;
    assert_eq!(
        sync_position_witness(store, &actor, PositionWriter::Commit, None)
            .await
            .expect("position writer"),
        (WitnessSync::Written, Some(PositionStanding::Advanced))
    );
    assert_eq!(live.stamps_of(&epoch).await, vec!["0".to_owned()]);
    let witness = live
        .position_witness(&actor)
        .await
        .expect("the position witness");
    assert_eq!(witness.epoch, RestoreEpoch::INITIAL);
    live.close().await;
}

/// Commit a row into a table of the case's own database, and wait until the
/// flushed position has moved past where it stood: with asynchronous
/// commit the WAL writer flushes a commit some time after it returns.
async fn advance_wal(fixture: &GrantFixture) {
    let before = fixture
        .store
        .postgres_position()
        .await
        .expect("position")
        .lsn;
    let pool = owner_pool(fixture).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        for statement in [
            "CREATE TABLE IF NOT EXISTS wal_advance (written_at timestamptz NOT NULL)",
            "INSERT INTO wal_advance VALUES (pg_catalog.now())",
        ] {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .unwrap_or_else(|error| panic!("{statement}: {error}"));
        }
        let now = fixture
            .store
            .postgres_position()
            .await
            .expect("position")
            .lsn;
        if now > before {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the flushed position did not move"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pool.close().await;
}
