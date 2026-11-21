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

// The shared fixtures serve several host-grant suites; this binary uses a
// subset of each.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[path = "host_grant_support/live_spicedb.rs"]
mod live_spicedb;
#[path = "host_grant_projection/protected_space_contents.rs"]
mod protected_space_contents;
#[path = "host_grant_projection/protected_space_events.rs"]
mod protected_space_events;
#[path = "host_grant_projection/protected_space_subscriptions.rs"]
mod protected_space_subscriptions;
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

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
    CheckPermission, DeleteTuple, EnsureAuthzCodecIncarnation, EnsureSchema, Object, ObjectType,
    Permission, PermissionActor, PermissionError, RecordAuthzSchemaVersion, Relation,
    ReplaceExclusiveRelation, Subject, Tuple, WriteHostGrantProjection, WriteTuple,
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
        let (endpoint, preshared_key) = live_spicedb::disposable_spicedb_env();
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
        // installed whatever an earlier test left; a store holding markers
        // or witnesses no test suite marked is refused instead. Each replica
        // marks the store as a test store once it has started.
        live_spicedb::empty_disposable_store(&raw).await;
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
        // is marked as a test store, before the case writes anything.
        live.actor(&live.fixture.store).await;
        live_spicedb::mark_test_store(&live.raw).await;
        live
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
        Self::marker_relationship_of(self.fixture.scope.lifecycle, stamp)
    }

    fn marker_relationship_of(lifecycle: ScopeLifecycleId, stamp: &str) -> Relationship {
        Relationship::new(
            ObjectReference::new(HOST_GRANT_PROJECTION_DEFINITION, lifecycle.to_storage())
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
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_space_owner_acknowledges_membership_over_websocket() {
    exercise_protected_space(SpaceExercise::MembershipAndDiscovery).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_space_subscription_lifecycle_over_websocket() {
    exercise_protected_space(SpaceExercise::Subscription).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_space_bookmark_events_over_websocket() {
    exercise_protected_space(SpaceExercise::BookmarkEvents).await;
}

enum SpaceExercise {
    MembershipAndDiscovery,
    Subscription,
    BookmarkEvents,
}

async fn exercise_protected_space(exercise: SpaceExercise) {
    use host_grant_revocation_support::{authenticate_unbound, Replica};
    use protected_space_subscriptions::{receive_sm, wait_for_detach};
    use s2s_inbound_muc_support::Wiring;
    use waddle_server::permissions::host_grant::{BindSpace, FenceAcknowledgement, ScopeKey};
    use waddle_ws_test_support::{
        iq::{receive_iq, round_trip, send_stanza},
        stanza::{get, is_error, is_result, set},
    };
    use waddle_xmpp::pubsub::{Affiliation, NodeConfig, NS_PUBSUB_OWNER};
    use waddle_xmpp::stream_management::SM_NS;
    use xmpp_parsers::{iq::Iq, minidom::Element};

    const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
    let _serial = LIVE_SPICEDB.lock().await;
    let label = match exercise {
        SpaceExercise::MembershipAndDiscovery => "hg_space_owner_ws",
        SpaceExercise::Subscription => "hg_space_subscription_ws",
        SpaceExercise::BookmarkEvents => "hg_space_bookmark_event_ws",
    };
    let live = Live::start(label).await;
    let wiring = Wiring::new(|_| {}).await;
    let authority = live.replica().await;
    let lifecycle = live.fixture.scope.lifecycle;
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("bootstrap projection"),
        Reconciliation::Activated(_)
    ));
    let mut replicas = Vec::new();
    for _ in 0..2 {
        let backend = PermissionActor::from_server_config(&live.config)
            .await
            .expect("the live permission backend");
        replicas.push(
            Replica::start_running_with_backend(
                live.fixture.ingress.db.database_url(),
                live.fixture.ingress.deployment_uuid(),
                &wiring.config,
                backend,
            )
            .await,
        );
    }
    let (first, second) = (&replicas[0], &replicas[1]);
    first.account("owner").await;
    first.account("alice").await;
    let service = bare("spaces.example.com");
    let node = live.fixture.scope.space_node.as_str();
    let storage = &first.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&service, node)
        .await
        .expect("create node");
    storage
        .update_node_config(&service, node, &NodeConfig::spaces_private())
        .await
        .expect("configure private Space");
    let hidden_scope = protected_space_contents::seed_legacy_child(&live, first).await;
    live.fixture
        .store
        .bind_space(
            &BindSpace {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: live.fixture.generation().await,
                owner: subject(OWNER),
                federated: true,
            },
            &served(),
        )
        .await
        .expect("bind Space authority");
    assert!(matches!(
        authority
            .reconcile(lifecycle)
            .await
            .expect("owner projection"),
        Reconciliation::Activated(_)
    ));

    let affiliations = |entries: &[(&str, &str)]| {
        let mut items = Element::builder("affiliations", NS_PUBSUB_OWNER)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
        for &(jid, affiliation) in entries {
            items = items.append(
                Element::builder("affiliation", NS_PUBSUB_OWNER)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
                    .attr(
                        minidom::rxml::xml_ncname!("affiliation").to_owned(),
                        affiliation,
                    )
                    .build(),
            );
        }
        Element::builder("pubsub", NS_PUBSUB_OWNER)
            .append(items.build())
            .build()
    };
    let change = |affiliation: &str| affiliations(&[(PEER, affiliation)]);
    let discover = |id: &str| {
        get(
            id,
            service.as_str(),
            Element::builder("query", NS_DISCO_INFO)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .build(),
        )
    };
    let key = ScopeKey::new(service.clone(), node).expect("Space key");
    let second_gate = &second.state.deps.app_state.host_grants;
    assert!(second_gate
        .space_read_local(&key, &subject(OWNER))
        .await
        .is_some());
    let mut owner = first.client("owner", "desk").await;
    let mut member = second.client("alice", "phone").await;
    let original = live.fixture.generation().await;
    let undisclosed = round_trip(&mut member, discover("space-disco-before")).await;
    assert!(
        is_error(&undisclosed, DefinedCondition::ItemNotFound),
        "{undisclosed:?}"
    );
    let denied = round_trip(
        &mut member,
        set("space-denied", service.as_str(), change("member")),
    )
    .await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    assert_eq!(live.fixture.generation().await, original);

    let unsupported = round_trip(
        &mut owner,
        set(
            "space-reject-publisher",
            service.as_str(),
            affiliations(&[(CAROL, "publisher")]),
        ),
    )
    .await;
    assert!(
        is_error(&unsupported, DefinedCondition::FeatureNotImplemented),
        "{unsupported:?}"
    );
    assert_eq!(live.fixture.generation().await, original);
    assert!(second_gate
        .space_read_local(&key, &subject(PEER))
        .await
        .is_none());

    let granted = round_trip(
        &mut owner,
        set("space-granted", service.as_str(), change("member")),
    )
    .await;
    assert!(is_result(&granted), "{granted:?}");
    match exercise {
        SpaceExercise::MembershipAndDiscovery => {}
        SpaceExercise::Subscription => {
            protected_space_subscriptions::assert_local_subscription_lifecycle(
                &service,
                node,
                first,
                &mut member,
                live.fixture.ingress.db.database_url(),
            )
            .await;
            drop((owner, member, replicas));
            live.close().await;
            return;
        }
        SpaceExercise::BookmarkEvents => {
            protected_space_events::assert_local_bookmark_events(
                first,
                &mut owner,
                &service,
                node,
                &live.fixture.scope.room,
            )
            .await;
            drop((owner, member, replicas));
            live.close().await;
            return;
        }
    }
    let list_affiliations = |id| get(id, service.as_str(), affiliations(&[]));
    let assert_affiliation_payload = |payload: Element, expected: &[(&str, &str)]| {
        assert!(payload.is("pubsub", NS_PUBSUB_OWNER), "{payload:?}");
        let rows = payload
            .get_child("affiliations", NS_PUBSUB_OWNER)
            .expect("owner affiliations");
        assert_eq!(rows.attr("node"), Some(node));
        let mut actual: Vec<_> = rows
            .children()
            .map(|row| {
                assert!(row.is("affiliation", NS_PUBSUB_OWNER), "{row:?}");
                (
                    row.attr("jid").expect("affiliation jid"),
                    row.attr("affiliation").expect("affiliation value"),
                )
            })
            .collect();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    };
    let assert_affiliations = |reply: Iq, expected: &[(&str, &str)]| {
        let Iq::Result {
            payload: Some(payload),
            ..
        } = reply
        else {
            panic!("Space owner affiliation query returned {reply:?}");
        };
        assert_affiliation_payload(payload, expected);
    };
    let mixed = round_trip(
        &mut owner,
        set(
            "space-affiliations-partial",
            service.as_str(),
            affiliations(&[(CAROL, "member"), (OWNER, "none")]),
        ),
    )
    .await;
    let Iq::Error {
        payload: Some(refused),
        error,
        ..
    } = mixed
    else {
        panic!("partial affiliation change returned {mixed:?}");
    };
    assert_eq!(
        (error.type_, error.defined_condition),
        (ErrorType::Modify, DefinedCondition::NotAcceptable)
    );
    assert_affiliation_payload(refused, &[(OWNER, "owner")]);
    let active = [(PEER, "member"), (CAROL, "member"), (OWNER, "owner")];
    assert_affiliations(
        round_trip(&mut owner, list_affiliations("space-affiliations-active")).await,
        &active,
    );
    let generation = live.fixture.generation().await;
    let duplicate = round_trip(
        &mut owner,
        set(
            "space-affiliations-duplicate",
            service.as_str(),
            affiliations(&[(CAROL, "none"), (CAROL, "outcast")]),
        ),
    )
    .await;
    assert!(
        is_error(&duplicate, DefinedCondition::BadRequest),
        "{duplicate:?}"
    );
    assert_eq!(live.fixture.generation().await, generation);
    assert_affiliations(
        round_trip(&mut owner, list_affiliations("space-affiliations-intact")).await,
        &active,
    );
    let denied = round_trip(
        &mut member,
        list_affiliations("space-affiliations-member-denied"),
    )
    .await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    let configuration = |id| {
        get(
            id,
            service.as_str(),
            Element::builder("pubsub", NS_PUBSUB_OWNER)
                .append(
                    Element::builder("configure", NS_PUBSUB_OWNER)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                        .build(),
                )
                .build(),
        )
    };
    let reply = round_trip(&mut owner, configuration("space-owner-configuration")).await;
    let Iq::Result {
        payload: Some(payload),
        ..
    } = reply
    else {
        panic!("Space owner configuration query returned {reply:?}");
    };
    assert!(payload.is("pubsub", NS_PUBSUB_OWNER), "{payload:?}");
    let config = payload
        .get_child("configure", NS_PUBSUB_OWNER)
        .expect("node configuration");
    assert_eq!(config.attr("node"), Some(node));
    let form = config
        .get_child("x", xmpp_parsers::ns::DATA_FORMS)
        .expect("configuration data form");
    assert_eq!(form.attr("type"), Some("form"));
    let field = |name| {
        form.children()
            .find(|child| {
                child.is("field", xmpp_parsers::ns::DATA_FORMS) && child.attr("var") == Some(name)
            })
            .expect("configuration field")
    };
    assert_eq!(field("FORM_TYPE").attr("type"), Some("hidden"));
    for (name, expected) in [
        ("FORM_TYPE", "http://jabber.org/protocol/pubsub#node_config"),
        ("pubsub#access_model", "whitelist"),
        ("pubsub#publish_model", "publishers"),
        ("pubsub#persist_items", "1"),
        ("pubsub#deliver_payloads", "1"),
    ] {
        assert_eq!(
            field(name)
                .get_child("value", xmpp_parsers::ns::DATA_FORMS)
                .expect("configuration field value")
                .text(),
            expected,
            "{name}",
        );
    }
    let denied = round_trip(
        &mut member,
        configuration("space-member-configuration-denied"),
    )
    .await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    protected_space_contents::assert_canonical_space_children(
        &live,
        &authority,
        first,
        &mut owner,
        &mut member,
        &hidden_scope,
    )
    .await;
    let scope = live.fixture.state(&live.fixture.scope).await;
    assert_eq!(scope.projected_generation, Some(scope.generation));
    let discovered = round_trip(&mut member, discover("space-disco-member")).await;
    let Iq::Result {
        from: Some(from),
        to: Some(to),
        id,
        payload: Some(info),
    } = discovered
    else {
        panic!("protected Space disco returned {discovered:?}");
    };
    assert_eq!(from.as_str(), service.as_str());
    assert_eq!(Some(to.as_str()), member.full_jid.as_deref());
    assert_eq!(id, "space-disco-member");
    assert!(info.is("query", NS_DISCO_INFO), "{info:?}");
    assert_eq!(info.attr("node"), Some(node));
    let before = second_gate
        .space_read_local(&key, &subject(PEER))
        .await
        .expect("member is effective on the other replica");
    assert_eq!(
        before.admit(|affiliation| affiliation),
        Some(Affiliation::Member)
    );

    let member_denied = round_trip(
        &mut member,
        set(
            "space-member-cannot-revoke",
            service.as_str(),
            change("none"),
        ),
    )
    .await;
    assert!(
        is_error(&member_denied, DefinedCondition::Forbidden),
        "{member_denied:?}"
    );
    assert_eq!(live.fixture.generation().await, scope.generation);
    send_stanza(
        &mut member,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = receive_sm(&mut member, &["enabled", "failed"]).await;
    assert!(enabled.is("enabled", SM_NS), "{enabled:?}");
    let token = enabled.attr("id").expect("durable resume token");
    let resume = || {
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), token)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build()
    };
    let resource: jid::FullJid = member
        .full_jid
        .as_deref()
        .expect("bound")
        .parse()
        .expect("JID");
    let retained = round_trip(&mut member, discover("space-disco-retained")).await;
    assert!(is_result(&retained), "{retained:?}");
    drop(member);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(live.fixture.ingress.db.database_url())
        .await
        .expect("test readback pool");
    let first_detach = wait_for_detach(&pool, &resource, 0).await;
    let mut resumed = authenticate_unbound(&second.url, "alice").await;
    send_stanza(&mut resumed, resume()).await;
    let confirmation = receive_sm(&mut resumed, &["resumed", "failed"]).await;
    assert!(confirmation.is("resumed", SM_NS), "{confirmation:?}");
    assert_eq!(confirmation.attr("previd"), Some(token));
    assert_eq!(confirmation.attr("h"), Some("1"));
    let replay = receive_iq(&mut resumed, "space-disco-retained").await;
    assert_eq!(
        replay, retained,
        "the authorized protected reply replays exactly"
    );
    drop(resumed);
    wait_for_detach(&pool, &resource, first_detach).await;
    let revoked = round_trip(
        &mut owner,
        set("space-revoked", service.as_str(), change("none")),
    )
    .await;
    assert!(is_result(&revoked), "{revoked:?}");
    let generation = live.fixture.generation().await;
    assert_eq!(
        live.fixture
            .store
            .revocation_fence(lifecycle, generation)
            .await
            .expect("fence status"),
        FenceAcknowledgement::Acknowledged { replicas: 2 }
    );
    assert!(
        before.admit(|_| ()).is_none(),
        "a stale member decision was admitted"
    );
    assert!(second_gate
        .space_read_local(&key, &subject(PEER))
        .await
        .is_none());
    assert_affiliations(
        round_trip(&mut owner, list_affiliations("space-affiliations-revoked")).await,
        &[(CAROL, "member"), (OWNER, "owner")],
    );
    let mut member = authenticate_unbound(&second.url, "alice").await;
    let registry = &second.state.deps.protocol.connection_registry;
    // The stale resume must return its original handled count without
    // entering the protected resource's registration lock.
    let registration_lock = registry.lock_bind(&resource).await;
    let mut pending_bind = registry.observe_next_pending_bind(&resource);
    send_stanza(&mut member, resume()).await;
    let failure = tokio::select! {
        biased;
        observed = &mut pending_bind => panic!("revoked replay reached registration: {observed:?}"),
        failure = receive_sm(&mut member, &["resumed", "failed"]) => failure,
    };
    assert!(failure.is("failed", SM_NS), "{failure:?}");
    assert!(
        failure
            .get_child("item-not-found", xmpp_parsers::ns::XMPP_STANZAS)
            .is_some(),
        "{failure:?}"
    );
    assert_eq!(failure.attr("h"), Some("1"));
    assert!(!registry.list_connections().contains(&resource));
    drop(registration_lock);
    assert_eq!(
        member
            .bind("phone")
            .await
            .expect("fresh bind after refused resume"),
        resource.as_str()
    );
    pool.close().await;
    let no_longer_disclosed = round_trip(&mut member, discover("space-disco-after")).await;
    assert!(
        is_error(&no_longer_disclosed, DefinedCondition::ItemNotFound),
        "{no_longer_disclosed:?}"
    );
    drop((owner, member, replicas));
    live.close().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and SpiceDB"]
async fn live_spicedb_graph_only_admin_cannot_moderate_a_managed_room_over_s2s() {
    use host_grant_revocation_support::{role_item, Replica};
    use s2s_inbound_muc_support::{
        error_condition, item, local_join, local_receives, local_set, presence_between,
        remote_join, remote_set, statuses, Hosted, Wiring, ARRIVES, HOME_B, QUIET,
    };
    use waddle_server::s2s::route::RouterLimits;
    use waddle_xmpp::{muc::room_actor::GetSnapshot, muc::room_registry_actor::GetRoom, Role};

    const GUEST: &str = "guest@home-b.test";
    const GUEST_PHONE: &str = "guest@home-b.test/phone";
    let _serial = LIVE_SPICEDB.lock().await;
    for (label, trusted) in [("hg_graph_untrusted", false), ("hg_graph_trusted", true)] {
        let live = Live::start(label).await;
        let wiring = Wiring::new(|spec| {
            if trusted {
                spec.trusted_homes = vec![HOME_B];
            }
        })
        .await;
        let authority = live.replica().await;
        let lifecycle = live.fixture.scope.lifecycle;
        assert!(matches!(
            authority
                .reconcile(lifecycle)
                .await
                .expect("bootstrap projection"),
            Reconciliation::Activated(_)
        ));
        for account in [GUEST, "alice@example.com"] {
            live.fixture
                .set(OWNER, account, RoomAffiliation::Member)
                .await
                .expect("canonical membership");
            assert!(matches!(
                authority
                    .reconcile(lifecycle)
                    .await
                    .expect("membership projection"),
                Reconciliation::Activated(_)
            ));
        }
        let backend = PermissionActor::from_server_config(&live.config)
            .await
            .expect("the live permission backend");
        assert_eq!(backend.backend_name(), "spicedb");
        let replica = Replica::start_running_with_backend(
            live.fixture.ingress.db.database_url(),
            live.fixture.ingress.deployment_uuid(),
            &wiring.config,
            backend,
        )
        .await;
        replica.account("owner").await;
        replica.account("alice").await;
        replica.channel(&live.fixture.scope.channel_id).await;
        replica.federate(&live.fixture.scope.channel_id, true).await;
        let hosted =
            Hosted::serve(wiring, Arc::clone(&replica.state), RouterLimits::default()).await;
        let room = live.fixture.scope.room.as_str();
        let guest_occupant = format!("{room}/guest");
        let target_occupant = format!("{room}/alice");
        let member = Some(("participant".to_owned(), "member".to_owned()));
        let mut owner = replica.client("owner", "desk").await;
        let mut target = replica.client("alice", "desk").await;
        assert_eq!(
            error_condition(&local_join(&mut owner, room, "owner").await),
            None
        );
        assert_eq!(item(&local_join(&mut target, room, "alice").await), member);
        hosted
            .peer_sends(remote_join(GUEST_PHONE, room, "guest", "guest-join"))
            .await;
        let admitted = hosted
            .at_peer(|stanza| {
                presence_between(stanza, &guest_occupant, GUEST_PHONE)
                    && statuses(stanza).iter().any(|code| code == "110")
            })
            .await
            .expect("the guest joins on its canonical membership");
        assert_eq!(item(&admitted), member);
        assert!(hosted.seat(room, GUEST_PHONE).is_some());
        let room_actor = replica
            .state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: live.fixture.scope.room.clone(),
            })
            .await
            .expect("room lookup")
            .expect("room actor");

        let channel = live.channel();
        let graph_only_admin = Tuple::new(
            channel.clone(),
            Relation::new("admin"),
            Subject::user(GUEST),
        );
        let raw = live.unguarded_actor().await;
        assert!(!spicedb_allows(&live, GUEST, Permission::Moderate, &channel).await);
        raw.ask(WriteTuple {
            tuple: graph_only_admin.clone(),
        })
        .await
        .expect("graph-only relationship");
        assert!(spicedb_allows(&live, GUEST, Permission::Moderate, &channel).await);
        assert_eq!(
            live.fixture.affiliations(GUEST).await,
            Some((RoomAffiliation::Member, RoomAffiliation::Member))
        );

        hosted
            .peer_sends(remote_set(
                GUEST_PHONE,
                room,
                "graph-only-devoice",
                role_item("visitor", "alice"),
            ))
            .await;
        let refused = hosted.answered("graph-only-devoice").await;
        assert_eq!(refused.attr("from"), Some(room), "{refused:?}");
        assert_eq!(refused.attr("to"), Some(GUEST_PHONE), "{refused:?}");
        assert_eq!(
            error_condition(&refused).as_deref(),
            Some("forbidden"),
            "{refused:?}"
        );
        let target_devoiced = |presence: &minidom::Element| {
            presence.name() == "presence"
                && presence.attr("from") == Some(target_occupant.as_str())
                && item(presence).is_some_and(|(role, _)| role == "visitor")
        };
        assert!(local_receives(&mut target, QUIET, target_devoiced)
            .await
            .is_none());
        let snapshot = room_actor.ask(GetSnapshot).await.expect("room snapshot");
        assert_eq!(snapshot.room.occupants["alice"].role, Role::Participant);
        assert!(spicedb_allows(&live, GUEST, Permission::Moderate, &channel).await);

        let authorized = local_set(
            &mut owner,
            room,
            "owner-devoice",
            role_item("visitor", "alice"),
        )
        .await;
        assert_eq!(authorized.attr("type"), Some("result"), "{authorized:?}");
        assert!(local_receives(&mut target, ARRIVES, target_devoiced)
            .await
            .is_some());
        let snapshot = room_actor.ask(GetSnapshot).await.expect("room snapshot");
        assert_eq!(snapshot.room.occupants["alice"].role, Role::Visitor);
        raw.ask(DeleteTuple {
            tuple: graph_only_admin,
        })
        .await
        .expect("remove graph-only relationship");
        assert!(!spicedb_allows(&live, GUEST, Permission::Moderate, &channel).await);
        drop(hosted);
        drop(replica);
        live.close().await;
    }
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
    let graph_only_owner = "mallory@example.com";
    let stray = Tuple::new(
        live.channel(),
        Relation::new("owner"),
        Subject::user(graph_only_owner),
    );
    let legacy = live.unguarded_actor().await;
    legacy
        .ask(WriteTuple {
            tuple: stray.clone(),
        })
        .await
        .expect("stray relationship");
    assert!(
        spicedb_allows(
            &live,
            graph_only_owner,
            Permission::Moderate,
            &live.channel()
        )
        .await
    );
    assert_eq!(live.fixture.affiliations(graph_only_owner).await, None);
    assert!(sql_admits(&live.fixture, OWNER, HostGrantAction::Moderate).await);
    assert!(!sql_admits(&live.fixture, graph_only_owner, HostGrantAction::Moderate).await);
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        expect(ProjectionContradiction::ContentMismatch)
    );
    assert!(!sql_admits(&live.fixture, "dave@example.com", HostGrantAction::Read).await);
    // The stray owner is nobody to the canonical policy.
    assert!(!sql_admits(&live.fixture, graph_only_owner, HostGrantAction::Manage).await);
    legacy
        .ask(DeleteTuple { tuple: stray })
        .await
        .expect("remove the stray relationship");
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("pass"),
        Reconciliation::Activated(plan.marker().generation)
    );
    assert!(sql_admits(&live.fixture, "dave@example.com", HostGrantAction::Read).await);

    // A marker the journal contradicts shows that the database lost
    // operations: besides being reported, it closes the scope as `fenced`
    // (the restore slice's decision), so each such case runs on a scope of
    // its own, with a projected first operation and a pending grant.
    type Contradicting = fn(&ProjectionMarker, &ProjectionMarker) -> ProjectionMarker;
    let cases: [(Contradicting, ProjectionContradiction); 4] = [
        // A marker of an incarnation the journal never held.
        (
            |projected, _| ProjectionMarker {
                incarnation: waddle_server::permissions::host_grant::AuthorityIncarnation::generate(
                ),
                ..projected.clone()
            },
            ProjectionContradiction::ForeignIncarnation,
        ),
        // A marker ahead of the canonical generation.
        (
            |_, pending| ProjectionMarker {
                generation: GrantGeneration::new(pending.generation.value() + 5)
                    .expect("generation"),
                ..pending.clone()
            },
            ProjectionContradiction::AheadOfCanonical,
        ),
        // One at the canonical generation that names another operation.
        (
            |_, pending| ProjectionMarker {
                operation: GrantOperationId::generate(),
                ..pending.clone()
            },
            ProjectionContradiction::AheadOfCanonical,
        ),
        // A marker of an older generation that the journal does not record.
        (
            |projected, _| ProjectionMarker {
                operation: GrantOperationId::generate(),
                ..projected.clone()
            },
            ProjectionContradiction::UnknownMarker,
        ),
    ];
    for (contradicting, contradiction) in cases {
        let scope = live.fixture.enroll_another().await;
        assert_eq!(
            authority
                .reconcile(scope.lifecycle)
                .await
                .expect("first pass"),
            Reconciliation::Activated(GrantGeneration::FIRST)
        );
        let projected = authority
            .store()
            .projection_plan(scope.lifecycle)
            .await
            .expect("plan")
            .marker()
            .clone();
        live.fixture
            .store
            .set_affiliation(
                &SetAffiliation {
                    operation: GrantOperationId::generate(),
                    lifecycle: scope.lifecycle,
                    expected_generation: live.fixture.state(&scope).await.generation,
                    initiator: subject(OWNER),
                    room: scope.room.clone(),
                    subject: subject(CAROL),
                    affiliation: RoomAffiliation::Member,
                },
                &live.fixture.policy,
            )
            .await
            .expect("pending grant");
        let pending = authority
            .store()
            .projection_plan(scope.lifecycle)
            .await
            .expect("plan")
            .marker()
            .clone();
        let marker = contradicting(&projected, &pending);
        live.raw
            .write_relationships(vec![
                RelationshipUpdate::delete(Live::marker_relationship_of(
                    scope.lifecycle,
                    &projected.stamp(),
                )),
                RelationshipUpdate::touch(Live::marker_relationship_of(
                    scope.lifecycle,
                    &marker.stamp(),
                )),
            ])
            .await
            .expect("raw marker write");
        assert_eq!(
            authority.reconcile(scope.lifecycle).await.expect("pass"),
            expect(contradiction)
        );
        let state = live.fixture.state(&scope).await;
        assert_eq!(state.admission, AdmissionState::Closed);
        assert_eq!(
            state.closed_reason,
            Some(waddle_server::permissions::host_grant::ClosedReason::Fenced)
        );
        assert!(matches!(
            authority.reconcile(scope.lifecycle).await,
            Err(HostGrantError::ScopeFenced)
        ));
    }
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
    let (endpoint, preshared_key) = live_spicedb::disposable_spicedb_env();
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

/// The revocation fence acknowledges a revoke from PostgreSQL alone
/// (ADR-0020 D5): the acknowledgement is given while SpiceDB still holds
/// the projection of the revoked grant, and nothing the projection holds
/// reopens it.
#[tokio::test]
#[ignore = "requires a disposable SpiceDB: WADDLE_TEST_SPICEDB_ENDPOINT, WADDLE_TEST_SPICEDB_PRESHARED_KEY and WADDLE_TEST_SPICEDB_DISPOSABLE=1"]
async fn live_spicedb_a_revoke_is_acknowledged_while_the_projection_still_holds_the_grant() {
    use waddle_server::permissions::host_grant::{
        EnrolledScopes, FenceAcknowledgement, HostGrantAttachment, HostGrantGate, RoomBinding,
        RoomGate,
    };
    let _serial = LIVE_SPICEDB.lock().await;
    let live = Live::start("hg_fence_projection").await;
    let authority = live.replica().await;
    let lifecycle = live.fixture.scope.lifecycle;
    let channel = live.channel();
    let room = live.fixture.scope.room.clone();
    authority.reconcile(lifecycle).await.expect("first pass");
    assert!(spicedb_allows(&live, PEER, Permission::Read, &channel).await);

    // A replica that decides for the room, with its lease.
    let enrolled = EnrolledScopes::watch(authority.store().clone());
    enrolled.refresh().await.expect("enrolled set");
    let gate = HostGrantGate::with_enrolled_scopes(authority.store().clone(), enrolled);
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(authority.store(), served())
            .await
            .expect("attachment")
    ));
    let decide = || async {
        match gate.room(RoomBinding::Room(&room)).await {
            RoomGate::Managed(grants) => grants,
            other => panic!("the replica does not decide the managed room: {other:?}"),
        }
    };
    let before = decide().await;
    assert!(before.allows(&bare(PEER), HostGrantAction::Read));

    // The revoke is recorded; no pass has projected it.
    let stamps = live.stamps().await;
    let revoke = live
        .fixture
        .command(OWNER, PEER, RoomAffiliation::None)
        .await;
    let receipt = authority
        .store()
        .set_affiliation(&revoke, &live.fixture.policy)
        .await
        .expect("revoke");
    let acknowledged = authority
        .store()
        .await_revocation_fence(
            lifecycle,
            receipt.record().generation,
            Duration::from_secs(10),
        )
        .await
        .expect("fence reading");
    assert_eq!(
        acknowledged,
        FenceAcknowledgement::Acknowledged { replicas: 1 }
    );
    // SpiceDB still answers for the projection before the revoke, and the
    // replica admits nothing under it: neither from the decision it made
    // before nor from one it makes now.
    assert_eq!(live.stamps().await, stamps);
    assert!(spicedb_allows(&live, PEER, Permission::Read, &channel).await);
    assert!(!before.allows(&bare(PEER), HostGrantAction::Read));
    assert!(!decide().await.allows(&bare(PEER), HostGrantAction::Read));
    assert!(!sql_admits(&live.fixture, PEER, HostGrantAction::Read).await);

    // The projection follows on the next pass.
    assert_eq!(
        authority.reconcile(lifecycle).await.expect("second pass"),
        Reconciliation::Activated(receipt.record().generation)
    );
    assert_ne!(live.stamps().await, stamps);
    assert!(!spicedb_allows(&live, PEER, Permission::Read, &channel).await);
    drop(gate);
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
    let (endpoint, preshared_key) = live_spicedb::disposable_spicedb_env();
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
