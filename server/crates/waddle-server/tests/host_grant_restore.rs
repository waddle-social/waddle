//! Restoring the canonical host-grant store (ADR-0020 D5 condition B7, gate
//! G13): the rows the restore slice owns in the host-grant design's
//! acceptance matrix.
//!
//! A restore of PostgreSQL brings back older grants, an older generation and
//! the `active` flag of every scope. These cases show that such a restore
//! cannot reopen authority that a later revoke or ban closed:
//!
//! - an undeclared restore is refused when a replica starts, from the
//!   projection markers, the database lineage and the configured restore
//!   epoch;
//! - a declared restore closes every scope under a new incarnation, so old
//!   commands, old activations, old projection passes and replicas attached
//!   before it admit nothing;
//! - a closed scope reopens only through a verified reconstruction, which
//!   keeps the revokes and bans the projection shows the restore lost and
//!   grants nothing from the projection;
//! - a reconstruction that does not fit the scope's bound leaves the scope
//!   closed.
//!
//! A restore is simulated by copying every host-grant relation into a
//! schema of its own (the backup) and later rewriting the relations from
//! the copy in one transaction (the restore). The rest of the database and
//! its lineage stay as they are, as after a restore within the same cluster.
//! The simulation does not restart PostgreSQL, so the server start an
//! attachment binds does not change; a real restore restarts the server and
//! a running replica then admits nothing (`AdmissionDenial::AuthorityChanged`).
//!
//! The cases without SpiceDB need the PostgreSQL test cluster only; where
//! they verify the restore fence they use the local permission backend,
//! which holds no projection. The `live_spicedb_` cases are `#[ignore]`d like
//! the other live suites: they need `WADDLE_TEST_SPICEDB_ENDPOINT`,
//! `WADDLE_TEST_SPICEDB_PRESHARED_KEY` and `WADDLE_TEST_SPICEDB_DISPOSABLE=1`
//! as well as the PostgreSQL test cluster.
#![cfg(feature = "test-support")]

mod host_grant_support;
mod ingress_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use host_grant_support::{served, subject, GrantFixture, OWNER, PEER, SENDER};
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
    reconstruction_targets, verify_restore_fence, Activation, AdmissionState, AuthorityIncarnation,
    ClosedReason, GrantGeneration, GrantOperationId, HostGrantAction, HostGrantAttachment,
    HostGrantAuthority, HostGrantError, HostGrantStore, OperationKind, OperationOutcome,
    ProjectedGrant, ProjectedRelationship, ProjectionContradiction, ProjectionIntent,
    ProjectionMarker, ProjectionRefusal, ProjectionWrite, Reconciliation, ReconstructionTarget,
    RestoreDeclaration, RestoreEpoch, RestoreFenceRefusal, RetireScope, RoomAffiliation,
    ScopeLifecycleId, HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    guarded_resource_definitions, CheckPermission, EnsureAuthzCodecIncarnation, EnsureSchema,
    Object, ObjectType, Permission, PermissionActor, RecordAuthzSchemaVersion, Relation, Subject,
    Tuple, AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_DEFINITION,
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

    // Nothing admits, whatever the grant rows hold.
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
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
async fn work_from_before_a_declared_restore_admits_nothing() {
    let Some(fixture) = GrantFixture::active("hg_restore_old_work").await else {
        return;
    };
    let lifecycle = fixture.scope.lifecycle;
    // A replica attached before the restore, and work prepared then: a
    // command decided at the current generation and the plan of a pass.
    let early = fixture.ingress.authority().await;
    assert!(early.attach_host_grants(
        HostGrantAttachment::bound_for_test(&fixture.store, served(), RestoreEpoch::INITIAL)
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
            HostGrantAttachment::bound_for_test(&fixture.store, served(), RestoreEpoch::INITIAL)
                .await
                .expect_err("an older epoch")
        ),
        (RestoreEpoch::INITIAL, RestoreEpoch::new(1))
    );
    let fresh = fixture.ingress.authority().await;
    assert!(fresh.attach_host_grants(
        HostGrantAttachment::bound_for_test(&fixture.store, served(), RestoreEpoch::new(1))
            .await
            .expect("attachment after the restore"),
    ));
    let closed = fixture.room_submission("restore-closed", "not yet");
    let decision = fresh.commit(&closed).await;
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(host_grant_support::refusal(&decision), SERVICE_UNAVAILABLE);

    // A revoke recorded while the scope is closed takes effect at its
    // commit, as at any other time.
    fixture
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke while restored");
    assert_eq!(
        fixture.affiliations(PEER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );

    // Once the restored scope is reactivated (here in place of a verified
    // reconstruction), the fresh replica admits and the early one does not.
    let plan = fixture
        .store
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    assert!(plan.restored);
    assert_ne!(plan.marker().incarnation, stale_plan.marker().incarnation);
    assert_eq!(
        activate_host_grant_marker(&fixture.store, plan.marker())
            .await
            .expect("activation"),
        Activation::Activated
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
    assert!(early.drain_and_join(Duration::from_secs(5)).await);
    assert!(fresh.drain_and_join(Duration::from_secs(5)).await);
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

    // A restore into another cluster is adopted, which rotates the lineage.
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
    verify_restore_fence(&fixture.store, &permissions, RestoreEpoch::new(1))
        .await
        .expect("fence after the declaration");
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
        assert_eq!(
            verify_restore_fence(&store, &permissions, configured)
                .await
                .expect("fence without a scope"),
            Default::default()
        );
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
async fn the_runtime_role_cannot_declare_a_restore() {
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
    // a guarded store transaction needs besides.
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

    // The runtime reads the binding, and cannot change it.
    assert_eq!(
        runtime.authority_binding().await.expect("runtime read"),
        binding
    );
    assert!(matches!(
        runtime
            .prepare_restore_incarnation(RestoreEpoch::new(1))
            .await,
        Err(HostGrantError::OwnerPhaseRequired)
    ));
    assert_eq!(
        fixture.store.authority_binding().await.expect("binding"),
        binding
    );
    assert_eq!(fixture.state(&fixture.scope).await, before);

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
        // Empty the store, markers included: the restore fence reads every
        // projection marker. A definition the stored schema lacks makes its
        // delete fail; that is the state wanted.
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

    /// The marker stamps SpiceDB holds for the fixture's lifecycle.
    async fn stamps(&self) -> Vec<String> {
        let mut stream = self
            .raw
            .read_relationships(
                RelationshipFilter::new(HOST_GRANT_PROJECTION_DEFINITION)
                    .resource_id(self.fixture.scope.lifecycle.to_storage()),
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

    /// Write `accounts` as members of the fixture's room beside the
    /// projection, in one request, as a holder of the key without the scope
    /// guard could.
    async fn write_strays(&self, accounts: &[String]) {
        let updates: Vec<RelationshipUpdate> = accounts
            .iter()
            .map(|account| {
                let wire = spicedb_wire::relationship(&Tuple::new(
                    self.channel(),
                    Relation::new("member"),
                    Subject::user(account.as_str()),
                ))
                .expect("wire relationship");
                RelationshipUpdate::touch(Relationship::new(
                    ObjectReference::new(wire.resource.object_type, wire.resource.object_id)
                        .expect("resource"),
                    wire.relation,
                    SubjectReference::new(
                        ObjectReference::new(wire.subject.object_type, wire.subject.object_id)
                            .expect("subject"),
                        wire.subject.relation,
                    )
                    .expect("subject reference"),
                ))
            })
            .collect();
        self.raw
            .write_relationships(updates)
            .await
            .expect("stray relationships");
    }

    async fn close(self) {
        self.fixture.close().await;
    }
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

fn generation(value: i64) -> GrantGeneration {
    GrantGeneration::new(value).expect("generation")
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

    // Undeclared, it is refused when a replica starts: the projection
    // records the revoke, which the journal does not.
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("an undeclared restore")
        ),
        RestoreFenceRefusal::ProjectionAhead
    );

    // Declared, the scope is closed and a replica configured for the
    // declared epoch starts.
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
    for account in [OWNER, SENDER, PEER] {
        assert!(!sql_admits(&live.fixture, account, HostGrantAction::Read).await);
    }

    // Reconstruction keeps the revoke the restore lost, rebuilds the
    // projection from the canonical rows and reopens the scope.
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Activated(generation(3))
    );
    assert_eq!(
        current_kind(&live.fixture, lifecycle).await,
        OperationKind::Reconstruct
    );
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(sql_admits(&live.fixture, PEER, HostGrantAction::SendMessage).await);
    assert!(sql_admits(&live.fixture, OWNER, HostGrantAction::Manage).await);
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.admission, AdmissionState::Active);
    assert_ne!(state.authority_incarnation, revoked.incarnation);
    let marker = live.current_marker().await;
    assert_eq!(marker.incarnation, state.authority_incarnation);
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    let channel = live.channel();
    assert!(!spicedb_allows(&actor, SENDER, Permission::Read, &channel).await);
    assert!(spicedb_allows(&actor, PEER, Permission::Read, &channel).await);
    let report = verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
        .await
        .expect("fence after reconstruction");
    assert_eq!(report.awaiting_reconstruction, 0);
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
        Reconciliation::Activated(generation(3))
    );

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

    // The rebuilt projection is exactly the canonical rows.
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
        Reconciliation::Activated(generation(4))
    );

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

    // The old plan is no takeover, and its write, applied against the
    // marker it saw, cannot be activated: the scope is bound to another
    // incarnation now.
    assert!(matches!(
        authority.take_over(&old_plan, &projected).await,
        Err(HostGrantError::Projection(ProjectionRefusal::NotATakeover))
    ));
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

    // Reconstruction takes the projection over from the old write and
    // reopens the scope under the new incarnation.
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Activated(generation(3))
    );
    assert_eq!(
        live.current_marker().await.incarnation,
        live.fixture
            .store
            .authority_incarnation()
            .await
            .expect("incarnation")
    );
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

    // A first restore is declared and reconstructed.
    declare(&live.fixture, 1).await;
    assert_eq!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("reconstruction"),
        Reconciliation::Activated(generation(2))
    );

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
    // With the configuration rolled back as well, the projection still
    // names an incarnation the journal does not record.
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a projection of a newer incarnation")
        ),
        RestoreFenceRefusal::UnknownIncarnation
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
        Reconciliation::Activated(generation(2))
    );
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_lost_enrollment_is_refused_at_startup() {
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
    verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
        .await
        .expect("fence before the restore");

    // Restored, its rooms would return to the admission of rooms no scope
    // manages. No replica starts, declared or not.
    backup.restore(&live.fixture).await;
    assert!(live
        .fixture
        .store
        .scope(later.lifecycle)
        .await
        .expect("read")
        .is_none());
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::INITIAL)
                .await
                .expect_err("a lost enrollment")
        ),
        RestoreFenceRefusal::OrphanedProjection
    );
    declare(&live.fixture, 1).await;
    assert_eq!(
        fence_refusal(
            verify_restore_fence(&live.fixture.store, &actor, RestoreEpoch::new(1))
                .await
                .expect_err("a lost enrollment, declared")
        ),
        RestoreFenceRefusal::OrphanedProjection
    );
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
