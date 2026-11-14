//! The fenced SpiceDB projection of canonical host grants (ADR-0020 D5,
//! first slice) and its recovery.
//!
//! Every case runs the production permission backend against a disposable
//! SpiceDB and the canonical store against PostgreSQL: the first projection
//! of a fresh namespace, a grant that becomes effective only after its
//! projection is verified and activated, recovery by another replica after
//! the first one stopped at each stage, a lost and a never-sent projection
//! write, a delayed writer that cannot apply an older grant after a newer
//! revoke, projection states that are never written over, the refusal of
//! legacy tuple writes for an enrolled scope, and a projection read that
//! does not answer while the permission actor keeps serving.
//!
//! The cases are `#[ignore]`d like the other `live_spicedb_` suites: they
//! need `WADDLE_TEST_SPICEDB_ENDPOINT`, `WADDLE_TEST_SPICEDB_PRESHARED_KEY`
//! and `WADDLE_TEST_SPICEDB_DISPOSABLE=1`, and a PostgreSQL test cluster.
//! A replica here is a store on its own connection pool with its own
//! permission actor; nothing is shared between two replicas but the two
//! stores they talk to. The recovery case runs its replicas as separate
//! processes of this test binary: one records a grant and exits abruptly
//! after a given stage, and another starts afterwards and recovers.
#![cfg(feature = "test-support")]

mod host_grant_support;
mod ingress_support;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use host_grant_support::{
    bare, issuance_policy, refusal, room_submission, served, subject, GrantFixture, OWNER, PEER,
    PEER_RESOURCE, SENDER,
};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use prescience::{
    Client, Consistency, ObjectReference, Relationship, RelationshipFilter, RelationshipUpdate,
    SubjectReference,
};
use tokio::sync::Mutex;
use waddle_server::config::SpiceDbConfig;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::IngressDecisionClass;
use waddle_server::permissions::host_grant::{
    Activation, AdmissionState, GrantGeneration, GrantOperationId, HostGrantAction,
    HostGrantAuthority, HostGrantError, HostGrantStore, MarkerObservation, OperationOutcome,
    ProjectionContradiction, ProjectionMarker, ProjectionRefusal, ProjectionWrite, Reconciliation,
    RoomAffiliation, ScopeLifecycleId, SetAffiliation, HOST_GRANT_PROJECTION_DEFINITION,
    HOST_GRANT_PROJECTION_MARKER_RELATION,
};
use waddle_server::permissions::{
    guarded_resource_definitions, CheckPermission, DeleteTuple, EnsureAuthzCodecIncarnation,
    EnsureSchema, Object, ObjectType, Permission, PermissionActor, PermissionError,
    RecordAuthzSchemaVersion, Relation, ReplaceExclusiveRelation, Subject, Tuple,
    WriteHostGrantProjection, WriteTuple, AUTHZ_CODEC_MARKER_DEFINITION,
    AUTHZ_SCHEMA_MARKER_DEFINITION,
};
use waddle_server::test_support::activate_host_grant_marker;
use waddle_server::ServerConfig;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const CAROL: &str = "carol@example.com";

/// What a replica process does: `grant-and-stop` or `recover`.
const REPLICA_ROLE: &str = "WADDLE_HOST_GRANT_REPLICA_ROLE";
/// The PostgreSQL database of the scope the replica process serves.
const REPLICA_DATABASE: &str = "WADDLE_HOST_GRANT_REPLICA_DATABASE";
const REPLICA_LIFECYCLE: &str = "WADDLE_HOST_GRANT_REPLICA_LIFECYCLE";
const REPLICA_ROOM: &str = "WADDLE_HOST_GRANT_REPLICA_ROOM";
/// The account a `grant-and-stop` replica grants membership to.
const REPLICA_ACCOUNT: &str = "WADDLE_HOST_GRANT_REPLICA_ACCOUNT";
/// The last stage a `grant-and-stop` replica completes before it exits.
const REPLICA_STAGE: &str = "WADDLE_HOST_GRANT_REPLICA_STAGE";
/// Exit status of a replica process that stopped where it was told to.
const REPLICA_STOPPED: i32 = 86;
/// The stages a grant passes, in order: its PostgreSQL commit, its
/// projection write, the verification read and the activation.
const STAGES: [&str; 4] = [
    "after-commit",
    "after-write",
    "after-verification",
    "after-activation",
];

// Live tests share one SpiceDB with the other `spicedb-live` binaries, which
// empty it, so they run one at a time: this lock serializes them within the
// binary, and the nextest group serializes them across binaries.
static LIVE_SPICEDB: Mutex<()> = Mutex::const_new(());

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
            "the host-grant projection cases need the PostgreSQL test cluster as well as \
             SpiceDB; set the WADDLE_TEST_POSTGRES_URL variable",
        );
        let raw = Client::builder(&endpoint, &preshared_key)
            .insecure(true)
            .build()
            .await
            .expect("raw SpiceDB client");
        // Empty the store, markers included, so that this build's schema is
        // installed whatever an earlier test left. A definition the stored
        // schema lacks makes its delete fail; that is the state wanted.
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
    /// the server's three startup steps and guarding `store`'s scopes.
    async fn actor(&self, store: &HostGrantStore) -> ActorRef<PermissionActor> {
        let backend = PermissionActor::from_server_config(&self.config)
            .await
            .expect("production SpiceDB backend");
        assert_eq!(backend.backend_name(), "spicedb");
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

    /// A production actor that does not carry the scope guard, as a binary
    /// built before the host-grant authority would run.
    async fn unguarded_actor(&self) -> ActorRef<PermissionActor> {
        let backend = PermissionActor::from_server_config(&self.config)
            .await
            .expect("production SpiceDB backend");
        PermissionActor::spawn(backend)
    }

    /// A replica: a store on its own pool and its own permission actor.
    async fn replica(&self) -> HostGrantAuthority {
        let db = Database::from_config(
            "host-grant-replica",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                self.fixture.ingress.db.database_url().to_owned(),
            ),
        )
        .await
        .expect("replica database");
        let store = HostGrantStore::open(db).expect("replica store");
        let actor = self.actor(&store).await;
        HostGrantAuthority::new(store, actor, self.fixture.policy.clone())
    }

    fn channel(&self) -> Object {
        Object::new(ObjectType::Channel, self.fixture.scope.channel_id.as_str())
    }

    /// The marker relationships SpiceDB holds for the fixture's lifecycle,
    /// as stamps.
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

    fn marker_relationship(&self, stamp: &str) -> Relationship {
        Relationship::new(
            ObjectReference::new(
                HOST_GRANT_PROJECTION_DEFINITION,
                self.fixture.scope.lifecycle.to_storage(),
            )
            .expect("marker resource"),
            HOST_GRANT_PROJECTION_MARKER_RELATION,
            SubjectReference::new(
                ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, stamp)
                    .expect("marker stamp"),
                None::<String>,
            )
            .expect("marker subject"),
        )
    }

    /// Replace whatever markers the lifecycle holds by `stamps`, verbatim.
    /// One request states a relationship once: a marker that stays is
    /// touched and not deleted as well.
    async fn set_stamps(&self, stamps: &[String]) {
        let mut updates: Vec<RelationshipUpdate> = self
            .stamps()
            .await
            .iter()
            .filter(|held| !stamps.contains(held))
            .map(|stamp| RelationshipUpdate::delete(self.marker_relationship(stamp)))
            .collect();
        updates.extend(
            stamps
                .iter()
                .map(|stamp| RelationshipUpdate::touch(self.marker_relationship(stamp))),
        );
        if !updates.is_empty() {
            self.raw
                .write_relationships(updates)
                .await
                .expect("raw marker write");
        }
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

    async fn close(self) {
        self.fixture.close().await;
    }
}

/// What SpiceDB itself answers for a channel permission. It is read
/// through a permission actor without the host-grant guard: a guarded
/// actor answers no legacy read of an enrolled scope's objects.
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

async fn sql_admits(fixture: &GrantFixture, account: &str, action: HostGrantAction) -> bool {
    fixture
        .store
        .candidate_admission(&fixture.scope.room, &subject(account), action, &served())
        .await
        .expect("candidate admission")
        .admitted
}

/// The marker a present, well-formed observation holds.
fn observed_marker(observation: MarkerObservation) -> ProjectionMarker {
    match observation {
        MarkerObservation::Present(marker) => marker,
        other => panic!("expected a marker, observed {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_first_projection_activates_the_bootstrap_grants() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_first_projection").await;
    let authority = live.replica().await;
    let lifecycle = live.fixture.scope.lifecycle;
    let actor = live.actor(&live.fixture.store).await;
    let channel = live.channel();

    // Recorded, not projected, not effective.
    assert!(live.stamps().await.is_empty());
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(!spicedb_allows(&live, SENDER, Permission::SendMessage, &channel).await);

    assert_eq!(
        authority.reconcile(lifecycle).await.expect("first pass"),
        Reconciliation::Activated(GrantGeneration::FIRST)
    );
    let state = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(state.admission, AdmissionState::Active);
    assert_eq!(state.projected_generation, Some(GrantGeneration::FIRST));

    // The projection is the marker and exactly the canonical family.
    let marker = live.current_marker().await;
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    assert_eq!(
        ProjectionMarker::parse(lifecycle, &marker.stamp()),
        Some(marker.clone())
    );
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let observation = authority.observe(&plan).await.expect("observation");
    assert!(HostGrantAuthority::verified(&plan, &observation));
    assert_eq!(observation.relationships.len(), 3);

    // Both stores now agree, each by its own evaluation.
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    assert!(spicedb_allows(&live, SENDER, Permission::SendMessage, &channel).await);
    assert!(spicedb_allows(&live, OWNER, Permission::Manage, &channel).await);
    assert!(!spicedb_allows(&live, SENDER, Permission::Manage, &channel).await);
    assert!(!spicedb_allows(&live, CAROL, Permission::Read, &channel).await);

    assert_eq!(
        authority.reconcile(lifecycle).await.expect("second pass"),
        Reconciliation::AlreadyActive(GrantGeneration::FIRST)
    );
    assert_eq!(live.stamps().await, vec![marker.stamp()]);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_grant_is_effective_only_after_its_projection_is_verified() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_grant_projection").await;
    let authority = live.replica().await;
    let lifecycle = live.fixture.scope.lifecycle;
    let actor = live.actor(&live.fixture.store).await;
    let channel = live.channel();
    authority.reconcile(lifecycle).await.expect("first pass");

    // A grant: recorded in PostgreSQL, in neither store's answers yet.
    let grant = live
        .fixture
        .command(OWNER, CAROL, RoomAffiliation::Member)
        .await;
    authority.set_affiliation(&grant).await.expect("grant");
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(!spicedb_allows(&live, CAROL, Permission::Read, &channel).await);
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("grant pass"),
        Reconciliation::Activated(GrantGeneration::new(2).expect("generation"))
    );
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(spicedb_allows(&live, CAROL, Permission::Read, &channel).await);
    assert_eq!(
        live.stamps().await,
        vec![live.current_marker().await.stamp()]
    );

    // A revoke: effective in PostgreSQL at its commit, in SpiceDB once
    // projected. Nothing in between reopens it.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(spicedb_allows(&live, CAROL, Permission::Read, &channel).await);
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("revoke pass"),
        Reconciliation::Activated(GrantGeneration::new(3).expect("generation"))
    );
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(!spicedb_allows(&live, CAROL, Permission::Read, &channel).await);

    // A ban replaces the member relationship by the outcast one, and the
    // schema's outcast precedence then denies every channel permission.
    live.fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban");
    authority.reconcile(lifecycle).await.expect("ban pass");
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let observation = authority.observe(&plan).await.expect("observation");
    assert!(HostGrantAuthority::verified(&plan, &observation));
    let peer: Vec<&str> = observation
        .relationships
        .iter()
        .filter(|relationship| relationship.subject == PEER)
        .map(|relationship| relationship.relation.as_str())
        .collect();
    assert_eq!(peer, vec!["outcast"]);
    for permission in [
        Permission::Read,
        Permission::View,
        Permission::SendMessage,
        Permission::Moderate,
        Permission::Manage,
        Permission::Delete,
    ] {
        assert!(
            !spicedb_allows(&live, PEER, permission.clone(), &channel).await,
            "{}",
            permission.as_str()
        );
    }
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_another_replica_recovers_after_a_stop_at_every_stage() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_recovery_stages").await;
    let lifecycle = live.fixture.scope.lifecycle;
    live.replica()
        .await
        .reconcile(lifecycle)
        .await
        .expect("first pass");

    // Each round, replica A is a process that records a grant and exits
    // abruptly after one more stage. Replica B is a process started
    // afterwards with nothing but the two stores: it finds the pending scope
    // itself and brings the grant to the same end state.
    let observer = live.replica().await;
    for (stage, name) in STAGES.into_iter().enumerate() {
        let account = format!("{name}@example.com");
        let stopped = replica_process(
            &live,
            "grant-and-stop",
            &[
                (REPLICA_ACCOUNT, account.clone()),
                (REPLICA_STAGE, stage.to_string()),
            ],
        )
        .await;
        assert_eq!(
            stopped.status.code(),
            Some(REPLICA_STOPPED),
            "replica A at {name}: {}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        let generation = live.fixture.generation().await;
        assert_eq!(
            live.fixture.affiliations(&account).await,
            Some((
                RoomAffiliation::Member,
                if stage >= 3 {
                    RoomAffiliation::Member
                } else {
                    RoomAffiliation::None
                }
            )),
            "{name}"
        );

        let recovered = replica_process(&live, "recover", &[]).await;
        let report = String::from_utf8_lossy(&recovered.stdout).into_owned();
        assert!(
            recovered.status.success(),
            "replica B at {name}: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        if stage >= 3 {
            assert!(
                report.contains("host-grant-recovered: 0"),
                "{name}: {report}"
            );
        } else {
            assert!(
                report.contains("host-grant-recovered: 1"),
                "{name}: {report}"
            );
            assert!(
                report.contains(&format!(
                    "host-grant-recovery: {} Ok(Activated(GrantGeneration({})))",
                    lifecycle.to_storage(),
                    generation.value()
                )),
                "{name}: {report}"
            );
        }
        assert_eq!(
            live.fixture.affiliations(&account).await,
            Some((RoomAffiliation::Member, RoomAffiliation::Member)),
            "{name}"
        );
        assert_eq!(
            live.stamps().await,
            vec![live.current_marker().await.stamp()]
        );
        let plan = observer
            .store()
            .projection_plan(lifecycle)
            .await
            .expect("plan");
        assert!(!plan.pending());
        assert!(HostGrantAuthority::verified(
            &plan,
            &observer.observe(&plan).await.expect("observation")
        ));
    }

    // A caller that stops waiting for its mutation leaves an operation the
    // other replica completes as well.
    let a = live.replica().await;
    let command = live
        .fixture
        .command(
            OWNER,
            "cancelled-caller@example.com",
            RoomAffiliation::Member,
        )
        .await;
    assert!(
        tokio::time::timeout(Duration::ZERO, a.set_affiliation(&command))
            .await
            .is_err()
    );
    drop(a);
    let b = live.replica().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while b
        .store()
        .operation_outcome(command.operation)
        .await
        .expect("readback")
        == OperationOutcome::Absent
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the abandoned operation never resolved"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let recovered = b.recover_pending().await.expect("recovery pass");
    assert!(matches!(
        recovered.as_slice(),
        [(_, Ok(Reconciliation::Activated(_)))]
    ));
    assert_eq!(
        live.fixture
            .affiliations("cancelled-caller@example.com")
            .await,
        Some((RoomAffiliation::Member, RoomAffiliation::Member))
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_lost_projection_reply_is_resolved_by_marker_readback() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_lost_reply").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let authority = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let first = live.current_marker().await;

    // Applied, and the reply never reached the writer: the next pass reads
    // the marker of the current operation and does not write again.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let _lost_reply = authority.write(&plan, Some(&first)).await;
    assert_eq!(live.stamps().await, vec![plan.marker().stamp()]);
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    // The write it would repeat is conditioned on a marker that is gone.
    assert_eq!(
        authority
            .write(&plan, Some(&first))
            .await
            .expect("repeated write"),
        ProjectionWrite::PreconditionFailed
    );
    assert_eq!(
        live.replica()
            .await
            .reconcile(lifecycle)
            .await
            .expect("pass after a lost reply"),
        Reconciliation::Activated(plan.marker().generation)
    );
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);

    // Never sent: the marker is still the previous one, and the pass writes.
    let second = live.current_marker().await;
    live.fixture
        .set(OWNER, "dave@example.com", RoomAffiliation::Member)
        .await
        .expect("grant");
    assert_eq!(live.stamps().await, vec![second.stamp()]);
    assert_eq!(
        live.replica()
            .await
            .reconcile(lifecycle)
            .await
            .expect("pass after a write that was never sent"),
        Reconciliation::Activated(GrantGeneration::new(3).expect("generation"))
    );
    assert!(sql_admits(&live.fixture, "dave@example.com", HostGrantAction::Read).await);

    // No acknowledged answer at all: unknown. Nothing is activated and
    // nothing is assumed rolled back; a later pass that gets an answer
    // completes the grant.
    live.fixture
        .set(OWNER, "erin@example.com", RoomAffiliation::Member)
        .await
        .expect("grant");
    let silent = HostGrantAuthority::new(
        live.fixture.store.clone(),
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
            live.fixture.ingress.db.clone(),
        ))),
        live.fixture.policy.clone(),
    );
    assert_eq!(
        silent.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::ProjectionUnknown
    );
    assert!(!sql_admits(&live.fixture, "erin@example.com", HostGrantAction::Read).await);
    assert!(live
        .fixture
        .state(&live.fixture.scope)
        .await
        .projection_pending());
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(GrantGeneration::new(4).expect("generation"))
    );
    assert!(sql_admits(&live.fixture, "erin@example.com", HostGrantAction::Read).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_delayed_grant_cannot_follow_a_newer_revoke() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_delayed_grant").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let stale = live.replica().await;
    let current = live.replica().await;
    let actor = live.actor(&live.fixture.store).await;
    let channel = live.channel();
    current.reconcile(lifecycle).await.expect("first pass");

    // The stale writer prepares the grant's projection: its intent, and the
    // marker it will condition the write on.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    let grant_plan = stale
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("grant plan");
    let before_grant = observed_marker(
        stale
            .observe(&grant_plan)
            .await
            .expect("observation")
            .marker,
    );
    assert_eq!(before_grant.generation, GrantGeneration::FIRST);

    // A newer revoke is recorded, projected and activated by another
    // replica while the stale writer is delayed.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert_eq!(
        current.reconcile(lifecycle).await.expect("revoke pass"),
        Reconciliation::Activated(GrantGeneration::new(3).expect("generation"))
    );
    let after_revoke = live.current_marker().await;
    assert_eq!(live.stamps().await, vec![after_revoke.stamp()]);

    // The delayed write fails its precondition and writes nothing.
    assert_eq!(
        stale
            .write(&grant_plan, Some(&before_grant))
            .await
            .expect("delayed write"),
        ProjectionWrite::PreconditionFailed
    );
    // Refreshing the precondition to the marker that is there now does not
    // let the old intent through: the write is refused before any RPC, by
    // the authority and by the backend's own message.
    assert!(matches!(
        stale.write(&grant_plan, Some(&after_revoke)).await,
        Err(HostGrantError::Projection(
            ProjectionRefusal::GenerationRegression
        ))
    ));
    assert!(matches!(
        actor
            .ask(WriteHostGrantProjection {
                previous: Some(after_revoke.clone()),
                intent: grant_plan.intent.clone(),
            })
            .await,
        Err(SendError::HandlerError(
            PermissionError::HostGrantProjection(ProjectionRefusal::GenerationRegression)
        ))
    ));
    // Nor does a write that claims the namespace is fresh.
    assert_eq!(
        stale
            .write(&grant_plan, None)
            .await
            .expect("write into a namespace that is not fresh"),
        ProjectionWrite::PreconditionFailed
    );
    // The delayed activation is for an operation that is no longer current.
    assert_eq!(
        activate_host_grant_marker(stale.store(), grant_plan.marker())
            .await
            .expect("delayed activation"),
        Activation::Superseded
    );

    // The revoke stands in both stores.
    assert_eq!(live.stamps().await, vec![after_revoke.stamp()]);
    assert_eq!(
        live.fixture.affiliations(CAROL).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    assert!(!spicedb_allows(&live, CAROL, Permission::Read, &channel).await);
    // The stale replica's next pass starts from the canonical state.
    assert_eq!(
        stale.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::AlreadyActive(after_revoke.generation)
    );
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_the_first_projection_requires_a_fresh_namespace() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_fresh_namespace").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let authority = live.replica().await;
    let legacy = live.unguarded_actor().await;
    let channel = live.channel();
    let stray = Tuple::new(
        channel.clone(),
        Relation::new("member"),
        Subject::user("mallory@example.com"),
    );
    legacy
        .ask(WriteTuple {
            tuple: stray.clone(),
        })
        .await
        .expect("relationship written before enrollment was projected");

    // The room's affiliation family is not empty, so the first projection
    // is not written and the scope stays closed.
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Contradiction(ProjectionContradiction::NamespaceNotFresh)
    );
    assert!(live.stamps().await.is_empty());
    assert_eq!(
        live.fixture.state(&live.fixture.scope).await.admission,
        AdmissionState::Closed
    );
    assert!(!sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    let refused = live
        .fixture
        .room_submission("not-fresh", "the scope never opened");
    let decision = commit_submission(&live.fixture.ingress.uow, &refused, 1)
        .await
        .expect("committed refusal");
    assert_eq!(
        refusal(&decision),
        (ErrorType::Cancel, DefinedCondition::ServiceUnavailable)
    );

    // With the namespace empty, the same pass projects and activates.
    legacy
        .ask(DeleteTuple { tuple: stray })
        .await
        .expect("remove the stray relationship");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(GrantGeneration::FIRST)
    );
    assert!(sql_admits(&live.fixture, SENDER, HostGrantAction::SendMessage).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_contradicting_projection_state_is_never_written_over() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_contradictions").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let authority = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    let projected = live.current_marker().await;

    // A grant waits for its projection throughout.
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    let pending = live.current_marker().await;
    let expect = |contradiction| Reconciliation::Contradiction(contradiction);
    let unchanged = |live: &Live| {
        let lifecycle = live.fixture.scope.lifecycle;
        let store = live.fixture.store.clone();
        async move {
            let state = store
                .scope(lifecycle)
                .await
                .expect("read scope")
                .expect("scope");
            assert!(state.projection_pending());
            assert_eq!(state.admission, AdmissionState::Active);
        }
    };

    // A second marker, and a marker that is not a stamp.
    live.set_stamps(&[projected.stamp(), "not-a-stamp".to_owned()])
        .await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::MalformedMarker)
    );
    live.set_stamps(&["not-a-stamp".to_owned()]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::MalformedMarker)
    );
    unchanged(&live).await;

    // No marker, although a projection of this lifecycle was activated.
    live.set_stamps(&[]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::MarkerMissing)
    );

    // A marker of another authority incarnation.
    let mut foreign = projected.clone();
    foreign.incarnation = waddle_server::permissions::host_grant::AuthorityIncarnation::generate();
    live.set_stamps(&[foreign.stamp()]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::ForeignIncarnation)
    );

    // A marker ahead of the canonical generation, and one at the canonical
    // generation that names another operation.
    let mut ahead = pending.clone();
    ahead.generation = GrantGeneration::new(pending.generation.value() + 5).expect("generation");
    live.set_stamps(&[ahead.stamp()]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::AheadOfCanonical)
    );
    let mut other_operation = pending.clone();
    other_operation.operation = GrantOperationId::generate();
    live.set_stamps(&[other_operation.stamp()]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::AheadOfCanonical)
    );

    // A marker of an older generation that the journal does not record.
    let mut unrecorded = projected.clone();
    unrecorded.operation = GrantOperationId::generate();
    live.set_stamps(&[unrecorded.stamp()]).await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::UnknownMarker)
    );
    unchanged(&live).await;

    // Throughout, the pending grant stayed ineffective, grants that were
    // effective kept admitting, and a revoke still took effect at once.
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    let admitted = live
        .fixture
        .room_submission("contradiction", "still admitted");
    let decision = commit_submission(&live.fixture.ingress.uow, &admitted, 1)
        .await
        .expect("admitted groupchat");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    live.fixture
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::Read).await);

    // With the marker of the last activated projection back, the pass
    // projects the current state: the grant and the revoke.
    live.set_stamps(&[projected.stamp()]).await;
    let generation = live.fixture.generation().await;
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(generation)
    );
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);

    // The canonical marker over a family that differs from the canonical
    // rows: a relationship written beside the projection by a writer that
    // does not carry the scope guard.
    live.fixture
        .set(OWNER, "dave@example.com", RoomAffiliation::Member)
        .await
        .expect("grant");
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let previous = observed_marker(authority.observe(&plan).await.expect("observation").marker);
    assert_eq!(
        authority
            .write(&plan, Some(&previous))
            .await
            .expect("write"),
        ProjectionWrite::Applied
    );
    let stray = Tuple::new(
        live.channel(),
        Relation::new("owner"),
        Subject::user("mallory@example.com"),
    );
    let legacy = live.unguarded_actor().await;
    legacy
        .ask(WriteTuple {
            tuple: stray.clone(),
        })
        .await
        .expect("stray relationship");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::ContentMismatch)
    );
    assert!(!sql_admits(&live.fixture, "dave@example.com", HostGrantAction::Read).await);
    // The stray owner is nobody to the canonical policy.
    assert!(
        !sql_admits(
            &live.fixture,
            "mallory@example.com",
            HostGrantAction::Manage
        )
        .await
    );
    legacy
        .ask(DeleteTuple { tuple: stray })
        .await
        .expect("remove the stray relationship");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(plan.marker().generation)
    );
    assert!(sql_admits(&live.fixture, "dave@example.com", HostGrantAction::Read).await);
    live.close().await;
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_legacy_writes_are_refused_for_an_enrolled_scope_after_a_restart() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_legacy_routes").await;
    let lifecycle = live.fixture.scope.lifecycle;
    let authority = live.replica().await;
    authority.reconcile(lifecycle).await.expect("first pass");
    live.fixture
        .set(OWNER, SENDER, RoomAffiliation::Outcast)
        .await
        .expect("ban the sender");
    authority.reconcile(lifecycle).await.expect("ban pass");
    let channel = live.channel();
    let space = Object::new(ObjectType::Space, live.fixture.scope.space_node.as_str());

    // A restarted replica: a new store handle and a new production actor.
    let restarted = live.replica().await;
    let actor = live.actor(restarted.store()).await;
    let family: Vec<Relation> = ["owner", "admin", "member", "outcast"]
        .into_iter()
        .map(Relation::new)
        .collect();
    for object in [&channel, &space] {
        assert!(matches!(
            actor
                .ask(WriteTuple {
                    tuple: Tuple::new(
                        object.clone(),
                        Relation::new("owner"),
                        Subject::user(SENDER)
                    ),
                })
                .await,
            Err(SendError::HandlerError(
                PermissionError::HostGrantScopeEnrolled
            ))
        ));
        assert!(matches!(
            actor
                .ask(ReplaceExclusiveRelation {
                    object: object.clone(),
                    subject: Subject::user(SENDER),
                    family: family.clone(),
                    replacement: Some(Relation::new("owner")),
                })
                .await,
            Err(SendError::HandlerError(
                PermissionError::HostGrantScopeEnrolled
            ))
        ));
    }
    // Nothing reached SpiceDB: the projection is still exactly canonical.
    let plan = restarted
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    assert!(HostGrantAuthority::verified(
        &plan,
        &restarted.observe(&plan).await.expect("observation")
    ));
    assert!(!spicedb_allows(&live, SENDER, Permission::SendMessage, &channel).await);

    // A writer without the guard can still put an owner relationship beside
    // the projection. SpiceDB keeps denying the banned sender, because the
    // schema subtracts outcast, and SQL admission never reads it.
    let legacy = live.unguarded_actor().await;
    legacy
        .ask(WriteTuple {
            tuple: Tuple::new(
                channel.clone(),
                Relation::new("owner"),
                Subject::user(SENDER),
            ),
        })
        .await
        .expect("unguarded legacy write");
    assert!(!spicedb_allows(&live, SENDER, Permission::SendMessage, &channel).await);
    assert_eq!(
        live.fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    let banned = room_submission(
        &live.fixture.ingress,
        &live.fixture.scope.room,
        "banned-after-restart",
        "still banned",
        &[PEER_RESOURCE],
    );
    let decision = commit_submission(&live.fixture.ingress.uow, &banned, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(
        refusal(&decision),
        (ErrorType::Auth, DefinedCondition::Forbidden)
    );
    live.close().await;
}

/// A TCP relay in front of the disposable SpiceDB that can stop forwarding,
/// so that a request sent through it stays unanswered until its deadline.
struct StallableRelay {
    endpoint: String,
    stalled: Arc<std::sync::atomic::AtomicBool>,
    accept: tokio::task::JoinHandle<()>,
}

impl StallableRelay {
    async fn start(upstream_endpoint: &str) -> Self {
        let upstream = upstream_endpoint
            .strip_prefix("http://")
            .expect("the disposable SpiceDB endpoint is a plaintext http:// URL")
            .trim_end_matches('/')
            .to_owned();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let endpoint = format!("http://{}", listener.local_addr().expect("relay address"));
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accept = tokio::spawn({
            let stalled = Arc::clone(&stalled);
            async move {
                while let Ok((client, _)) = listener.accept().await {
                    let Ok(server) = tokio::net::TcpStream::connect(&upstream).await else {
                        continue;
                    };
                    let (client_read, client_write) = client.into_split();
                    let (server_read, server_write) = server.into_split();
                    tokio::spawn(Self::forward(
                        client_read,
                        server_write,
                        Arc::clone(&stalled),
                    ));
                    tokio::spawn(Self::forward(
                        server_read,
                        client_write,
                        Arc::clone(&stalled),
                    ));
                }
            }
        });
        Self {
            endpoint,
            stalled,
            accept,
        }
    }

    /// Copy one direction of a connection, holding what was read for as
    /// long as the relay is stalled.
    async fn forward(
        mut from: tokio::net::tcp::OwnedReadHalf,
        mut to: tokio::net::tcp::OwnedWriteHalf,
        stalled: Arc<std::sync::atomic::AtomicBool>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buffer = vec![0_u8; 16 * 1024];
        loop {
            let read = match from.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            // The flag is the condition; the interval only spaces the polls.
            while stalled.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if to.write_all(&buffer[..read]).await.is_err() {
                return;
            }
        }
    }

    fn stall(&self, stalled: bool) {
        self.stalled
            .store(stalled, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for StallableRelay {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_projection_read_that_does_not_answer_does_not_hold_the_permission_actor() {
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_projection_stall").await;
    let lifecycle = live.fixture.scope.lifecycle;
    // The first projection, through an actor that reaches SpiceDB directly.
    live.replica()
        .await
        .reconcile(lifecycle)
        .await
        .expect("first pass");

    // A replica whose permission actor reaches SpiceDB through the relay.
    let (endpoint, preshared_key) = disposable_spicedb_env();
    let relay = StallableRelay::start(&endpoint).await;
    let mut config = ServerConfig::test_homeserver();
    config.spicedb = Some(SpiceDbConfig {
        endpoint: relay.endpoint.clone(),
        preshared_key,
        insecure: true,
    });
    let backend = PermissionActor::from_server_config(&config)
        .await
        .expect("SpiceDB backend through the relay");
    let actor = PermissionActor::spawn(backend.with_host_grant_store(live.fixture.store.clone()));
    let authority = HostGrantAuthority::new(
        live.fixture.store.clone(),
        actor.clone(),
        live.fixture.policy.clone(),
    );
    let plan = authority
        .store()
        .projection_plan(lifecycle)
        .await
        .expect("plan");
    let observation = authority
        .observe(&plan)
        .await
        .expect("observation through the relay");
    assert!(HostGrantAuthority::verified(&plan, &observation));

    // SpiceDB stops answering. An observation is now in flight until its
    // read deadline.
    relay.stall(true);
    let stalled = tokio::spawn({
        let authority = authority.clone();
        let plan = plan.clone();
        async move { authority.observe(&plan).await }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!stalled.is_finished());

    // The actor answers what it can decide without SpiceDB meanwhile: a
    // legacy write to the scope's channel is refused from PostgreSQL. It
    // does not wait for the observation, whose reads run outside the
    // actor's mailbox turn.
    let started = std::time::Instant::now();
    let refused = actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                live.channel(),
                Relation::new("member"),
                Subject::user(CAROL),
            ),
        })
        .await;
    assert!(matches!(
        refused,
        Err(SendError::HandlerError(
            PermissionError::HostGrantScopeEnrolled
        ))
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!stalled.is_finished());

    // The observation ends at its deadline with an unknown outcome, and a
    // pass reports the projection as unknown without changing anything.
    assert!(matches!(
        stalled.await.expect("observation task"),
        Err(HostGrantError::ProjectionStore(_))
    ));
    live.fixture
        .set(OWNER, CAROL, RoomAffiliation::Member)
        .await
        .expect("grant");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("stalled pass"),
        Reconciliation::ProjectionUnknown
    );
    assert!(!sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);

    // With SpiceDB answering again the next pass activates the grant.
    relay.stall(false);
    let generation = live.fixture.generation().await;
    let mut outcome = authority.reconcile(lifecycle).await.expect("pass");
    // Requests the stalled passes left in flight may still be draining; a
    // pass that meets them reports the projection as unknown and the next
    // one starts over.
    for _ in 0..20 {
        if outcome != Reconciliation::ProjectionUnknown {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        outcome = authority.reconcile(lifecycle).await.expect("pass");
    }
    assert_eq!(outcome, Reconciliation::Activated(generation));
    assert!(sql_admits(&live.fixture, CAROL, HostGrantAction::Read).await);
    drop(relay);
    live.close().await;
}

/// Run this test binary's replica entry as a separate process serving the
/// fixture's scope, and wait for it to exit.
async fn replica_process(live: &Live, role: &str, vars: &[(&str, String)]) -> std::process::Output {
    let mut command =
        tokio::process::Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "live_spicedb_replica_process_entry",
            "--ignored",
            "--nocapture",
        ])
        .env(REPLICA_ROLE, role)
        .env(REPLICA_DATABASE, live.fixture.ingress.db.database_url())
        .env(REPLICA_LIFECYCLE, live.fixture.scope.lifecycle.to_storage())
        .env(REPLICA_ROOM, live.fixture.scope.room.as_str())
        .kill_on_drop(true);
    for (name, value) in vars {
        command.env(name, value);
    }
    tokio::time::timeout(Duration::from_secs(180), command.output())
        .await
        .expect("the replica process exceeded its deadline")
        .expect("run the replica process")
}

/// The body of a replica process. Run by the suite itself it does nothing.
///
/// A replica starts as a server does: it opens its own pool on the scope's
/// database and its own production permission backend, and runs the three
/// permission startup steps. It never empties SpiceDB.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_replica_process_entry() {
    let Ok(role) = std::env::var(REPLICA_ROLE) else {
        return;
    };
    let variable = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is set"));
    let (endpoint, preshared_key) = disposable_spicedb_env();
    let mut config = ServerConfig::test_homeserver();
    config.spicedb = Some(SpiceDbConfig {
        endpoint,
        preshared_key,
        insecure: true,
    });
    let db = Database::from_config(
        "host-grant-replica-process",
        &DatabaseConfig::new(DatabaseDriver::Postgres, variable(REPLICA_DATABASE)),
    )
    .await
    .expect("replica database");
    let store = HostGrantStore::open(db).expect("replica store");
    let backend = PermissionActor::from_server_config(&config)
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
    let authority = HostGrantAuthority::new(store, actor, issuance_policy(&[]));
    let lifecycle =
        ScopeLifecycleId::from_storage(&variable(REPLICA_LIFECYCLE)).expect("lifecycle");

    match role.as_str() {
        "grant-and-stop" => {
            let stage: usize = variable(REPLICA_STAGE).parse().expect("stage");
            let generation = authority
                .store()
                .scope(lifecycle)
                .await
                .expect("read scope")
                .expect("enrolled scope")
                .generation;
            let command = SetAffiliation {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: generation,
                initiator: subject(OWNER),
                room: bare(&variable(REPLICA_ROOM)),
                subject: subject(&variable(REPLICA_ACCOUNT)),
                affiliation: RoomAffiliation::Member,
            };
            authority.set_affiliation(&command).await.expect("grant");
            if stage >= 1 {
                let plan = authority
                    .store()
                    .projection_plan(lifecycle)
                    .await
                    .expect("plan");
                let previous =
                    observed_marker(authority.observe(&plan).await.expect("observation").marker);
                assert_eq!(
                    authority
                        .write(&plan, Some(&previous))
                        .await
                        .expect("write"),
                    ProjectionWrite::Applied
                );
                if stage >= 2 {
                    let observation = authority.observe(&plan).await.expect("verification read");
                    assert!(HostGrantAuthority::verified(&plan, &observation));
                }
                if stage >= 3 {
                    assert_eq!(
                        activate_host_grant_marker(authority.store(), plan.marker())
                            .await
                            .expect("activation"),
                        Activation::Activated
                    );
                }
            }
            // The replica stops here, without closing or completing anything.
            std::process::exit(REPLICA_STOPPED);
        }
        "recover" => {
            let recovered = authority.recover_pending().await.expect("recovery pass");
            println!("host-grant-recovered: {}", recovered.len());
            for (lifecycle, outcome) in recovered {
                println!(
                    "host-grant-recovery: {} {outcome:?}",
                    lifecycle.to_storage()
                );
            }
        }
        other => panic!("unknown replica role {other}"),
    }
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
