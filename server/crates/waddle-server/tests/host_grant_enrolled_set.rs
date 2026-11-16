//! A replica's in-memory enrolled set (`EnrolledScopes`, ADR-0020 D5;
//! gates G11, G14): which channels, room addresses and Space nodes enrolled
//! host-grant scopes manage, as the host-grant gate and the permission
//! actor's legacy reads decide it.
//!
//! - The loaded set names every object of an enrolled scope and nothing
//!   else; a spelling of a managed room that is not canonical is undecided,
//!   while one of an unmanaged room keeps its legacy checks.
//! - A scope enrolled after a load is managed from the next refresh on, and
//!   a refresh that fails keeps the set loaded before.
//! - A room outside the set is decided without reading PostgreSQL, so it
//!   keeps its legacy checks while the grants cannot be read; a managed room
//!   is read from PostgreSQL for every decision and fails closed.
//! - A schema without the host-grant relations manages nothing.
//! - Until the set has loaded, every channel, room and Space is undecided:
//!   the gate and the permission actor's legacy reads fail closed.
//!
//! The cases need PostgreSQL only. The fixture attaches the replica without
//! the runtime-privilege check, since its login owns the schema.
#![cfg(feature = "test-support")]

// The shared fixtures serve several host-grant suites; this binary uses a
// subset of each.
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use host_grant_support::{bare, served, GrantFixture, OWNER, SENDER};
use ingress_support::IsolatedPostgresDatabase;
use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::permissions::host_grant::{
    EnrolledScopes, Enrollment, HostGrantAction, HostGrantAttachment, HostGrantError,
    HostGrantGate, HostGrantStore, RoomBinding, RoomGate, ENROLLED_SET_STALE_AFTER,
};
use waddle_server::permissions::{
    CheckPermission, ListRelations, ListSubjects, Object, ObjectType, Permission, PermissionActor,
    PermissionError, Relation, Subject, Tuple, WriteTuple,
};

/// An account of the served domain with only legacy relationships.
const CAROL: &str = "carol@example.com";
/// Longest a case waits for a set's first load.
const LOAD: Duration = Duration::from_secs(10);

/// A room address with the trailing dot the `jid` parser keeps: the same
/// room in a spelling that is not canonical.
fn trailing_dot(room: &BareJid) -> BareJid {
    format!("{room}.").parse().expect("dotted room address")
}

/// An enrolled set of `store`, loaded.
async fn loaded(store: &HostGrantStore) -> EnrolledScopes {
    let scopes = EnrolledScopes::watch(store.clone());
    assert!(scopes.wait_loaded(LOAD).await, "the enrolled set loads");
    scopes
}

/// An enrolled set of `store` that nothing loads: built where no Tokio
/// runtime runs, so no background refresh is started.
fn never_loaded(store: &HostGrantStore) -> EnrolledScopes {
    let store = store.clone();
    std::thread::spawn(move || EnrolledScopes::watch(store))
        .join()
        .expect("enrolled set built outside the runtime")
}

/// A permission actor fenced by `store` that reads enrollment from `scopes`,
/// and an actor without the fence, both on `fixture`'s database.
fn actors(
    fixture: &GrantFixture,
    scopes: &EnrolledScopes,
) -> (ActorRef<PermissionActor>, ActorRef<PermissionActor>) {
    let db = Arc::new(fixture.ingress.db.clone());
    let guarded = PermissionActor::spawn(
        PermissionActor::new_for_tests(Arc::clone(&db))
            .with_host_grant_authority(fixture.store.clone(), scopes.clone()),
    );
    let unguarded = PermissionActor::spawn(PermissionActor::new_for_tests(db));
    (guarded, unguarded)
}

async fn legacy_member(actor: &ActorRef<PermissionActor>, object: &Object) {
    actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                object.clone(),
                Relation::new("member"),
                Subject::user(CAROL),
            ),
        })
        .await
        .expect("legacy relationship");
}

async fn legacy_check(
    actor: &ActorRef<PermissionActor>,
    object: &Object,
) -> Result<bool, SendError<CheckPermission, PermissionError>> {
    actor
        .ask(CheckPermission {
            subject: Subject::user(CAROL),
            permission: Permission::Member,
            object: object.clone(),
        })
        .await
        .map(|response| response.allowed)
}

#[tokio::test]
async fn the_loaded_set_names_every_object_of_an_enrolled_scope_and_nothing_else() {
    let Some(fixture) = GrantFixture::active("hg_set_objects").await else {
        return;
    };
    let scope = &fixture.scope;
    let scopes = loaded(&fixture.store).await;

    assert_eq!(scopes.channel(&scope.channel_id), Enrollment::Enrolled);
    assert_eq!(scopes.room(&scope.room), Enrollment::Enrolled);
    assert_eq!(scopes.space_node(&scope.space_node), Enrollment::Enrolled);
    assert_eq!(
        scopes.object(&Object::new(ObjectType::Channel, scope.channel_id.as_str())),
        Enrollment::Enrolled
    );
    assert_eq!(
        scopes.object(&Object::new(ObjectType::Space, scope.space_node.as_str())),
        Enrollment::Enrolled
    );
    let set = scopes.snapshot().expect("loaded set");
    assert_eq!(
        set.scope_channels(scope.lifecycle),
        vec![scope.channel_id.clone()]
    );

    let unmanaged_room = bare("unmanaged@muc.example.com");
    assert_eq!(scopes.channel("unmanaged"), Enrollment::Unmanaged);
    assert_eq!(scopes.room(&unmanaged_room), Enrollment::Unmanaged);
    assert_eq!(scopes.space_node("plain"), Enrollment::Unmanaged);
    assert_eq!(
        scopes.object(&Object::new(ObjectType::Server, "deployment")),
        Enrollment::Unmanaged
    );

    // A managed room is not served under another spelling; an unmanaged
    // room keeps its legacy checks whatever its spelling.
    assert_eq!(
        scopes.room(&trailing_dot(&scope.room)),
        Enrollment::Undecided
    );
    assert_eq!(
        scopes.room(&trailing_dot(&unmanaged_room)),
        Enrollment::Unmanaged
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_scope_enrolled_after_a_load_is_managed_from_the_next_refresh() {
    let Some(fixture) = GrantFixture::active("hg_set_refresh").await else {
        return;
    };
    // Refreshed only by the case itself, so that no background refresh
    // lands between the enrollment and the reads before the refresh.
    let scopes = never_loaded(&fixture.store);
    scopes.refresh().await.expect("first load");
    let (guarded, unguarded) = actors(&fixture, &scopes);

    let another = fixture.enroll_another().await;
    let channel = Object::new(ObjectType::Channel, another.channel_id.as_str());
    legacy_member(&unguarded, &channel).await;

    // Until the set is refreshed, the replica does not know the scope: its
    // room keeps the legacy answers. Enrollment waits one refresh interval
    // before the scope is relied on for that reason.
    assert_eq!(scopes.channel(&another.channel_id), Enrollment::Unmanaged);
    assert_eq!(scopes.room(&another.room), Enrollment::Unmanaged);
    assert_eq!(legacy_check(&guarded, &channel).await.ok(), Some(true));

    scopes.refresh().await.expect("refresh");
    assert_eq!(scopes.channel(&another.channel_id), Enrollment::Enrolled);
    assert_eq!(scopes.room(&another.room), Enrollment::Enrolled);
    assert_eq!(scopes.space_node(&another.space_node), Enrollment::Enrolled);
    assert_eq!(legacy_check(&guarded, &channel).await.ok(), Some(false));
    fixture.close().await;
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_set_and_unmanaged_rooms_need_no_read() {
    let Some(fixture) = GrantFixture::active("hg_set_failure").await else {
        return;
    };
    let scope = &fixture.scope;
    let scopes = loaded(&fixture.store).await;

    // The lifecycle ledger cannot be read: the refresh fails and the set
    // loaded before still applies.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles RENAME TO host_grant_scope_lifecycles_hidden",
            (),
        )
        .await;
    assert!(scopes.refresh().await.is_err());
    assert_eq!(scopes.channel(&scope.channel_id), Enrollment::Enrolled);
    assert_eq!(scopes.space_node(&scope.space_node), Enrollment::Enrolled);
    assert_eq!(scopes.channel("unmanaged"), Enrollment::Unmanaged);
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles_hidden RENAME TO host_grant_scope_lifecycles",
            (),
        )
        .await;
    scopes.refresh().await.expect("refresh");

    // The room bindings are gone from a schema that held them: that is a
    // failed refresh, never a schema without scopes, so the managed rooms
    // stay managed.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_rooms RENAME TO host_grant_rooms_hidden",
            (),
        )
        .await;
    assert!(matches!(
        scopes.refresh().await,
        Err(HostGrantError::RelationsMissing)
    ));
    assert_eq!(scopes.channel(&scope.channel_id), Enrollment::Enrolled);
    assert_eq!(scopes.room(&scope.room), Enrollment::Enrolled);
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_rooms_hidden RENAME TO host_grant_rooms",
            (),
        )
        .await;
    scopes.refresh().await.expect("refresh");

    // The grants cannot be read: a managed room fails closed, a room the
    // set does not name is decided without them.
    let gate = HostGrantGate::with_enrolled_scopes(fixture.store.clone(), scopes.clone());
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(gate.store().expect("store"), served())
            .await
            .expect("attachment")
    ));
    fixture
        .ingress
        .execute("ALTER TABLE host_grants RENAME TO host_grants_hidden", ())
        .await;
    let unmanaged_room = bare("unmanaged@muc.example.com");
    assert!(matches!(
        gate.room(RoomBinding::Room(&unmanaged_room)).await,
        RoomGate::Unmanaged
    ));
    assert!(matches!(
        gate.room(RoomBinding::Room(&scope.room)).await,
        RoomGate::Unavailable
    ));
    fixture
        .ingress
        .execute("ALTER TABLE host_grants_hidden RENAME TO host_grants", ())
        .await;
    match gate.room(RoomBinding::Room(&scope.room)).await {
        RoomGate::Managed(room) => {
            assert!(room.allows(&bare(OWNER), HostGrantAction::Manage));
            assert!(room.allows(&bare(SENDER), HostGrantAction::Read));
            assert!(!room.allows(&bare(CAROL), HostGrantAction::View));
        }
        other => panic!("the managed room was not decided: {other:?}"),
    }
    fixture.close().await;
}

/// The gate decides managed rooms with the attachment the replica's
/// attachment gate holds at each decision, the one room-manifest acceptance
/// uses: none before that gate attaches, the one it holds once it does, and
/// none once a different attachment gate was offered.
#[tokio::test]
async fn the_gate_reads_the_attachment_its_attachment_gate_holds() {
    let Some(fixture) = GrantFixture::active("hg_set_bound").await else {
        return;
    };
    let scope = &fixture.scope;
    let gate =
        HostGrantGate::with_enrolled_scopes(fixture.store.clone(), loaded(&fixture.store).await);
    let replica = fixture.ingress.authority().await;
    assert!(gate.bind_attachment(replica.host_grant_gate()));
    assert!(!gate.bind_attachment(replica.host_grant_gate()));
    assert!(gate.attachment().is_none());
    assert!(matches!(
        gate.room(RoomBinding::Room(&scope.room)).await,
        RoomGate::Unavailable
    ));

    // The replica's attachment gate attaches, as its verification would.
    assert!(replica.attach_host_grants(
        HostGrantAttachment::unfenced_for_test(&fixture.store, served())
            .await
            .expect("attachment")
    ));
    assert!(gate.attachment().is_some());
    assert!(matches!(
        gate.room(RoomBinding::Room(&scope.room)).await,
        RoomGate::Managed(_)
    ));

    // Another attachment gate offered once one is bound leaves the gate
    // unable to tell which speaks for the replica: it refuses every managed
    // room from then on.
    assert!(!gate.bind_attachment(Arc::default()));
    assert!(gate.attachment().is_none());
    assert!(matches!(
        gate.room(RoomBinding::Room(&scope.room)).await,
        RoomGate::Unavailable
    ));
    assert!(replica.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

#[tokio::test]
async fn the_gate_reads_grants_only_for_enrolled_rooms_on_an_attached_replica() {
    let Some(fixture) = GrantFixture::active("hg_set_gate").await else {
        return;
    };
    let scope = &fixture.scope;
    let scopes = loaded(&fixture.store).await;
    let gate = HostGrantGate::with_enrolled_scopes(fixture.store.clone(), scopes);
    let unmanaged_room = bare("unmanaged@muc.example.com");

    // Without an attachment, an unmanaged room is still decided by its
    // legacy checks; a managed room is not decided at all.
    assert!(matches!(
        gate.room(RoomBinding::Room(&unmanaged_room)).await,
        RoomGate::Unmanaged
    ));
    assert!(matches!(
        gate.room(RoomBinding::Room(&scope.room)).await,
        RoomGate::Unavailable
    ));
    assert!(matches!(
        gate.room(RoomBinding::Channel(&scope.channel_id)).await,
        RoomGate::Unavailable
    ));
    assert_eq!(gate.channel_enrolled(&scope.channel_id).await, Ok(true));
    assert_eq!(gate.channel_enrolled("unmanaged").await, Ok(false));
    assert_eq!(gate.space_enrolled(&scope.space_node).await, Ok(true));

    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(gate.store().expect("store"), served())
            .await
            .expect("attachment")
    ));
    assert!(matches!(
        gate.room(RoomBinding::Channel(&scope.channel_id)).await,
        RoomGate::Managed(_)
    ));
    assert!(matches!(
        gate.allows(
            RoomBinding::Room(&scope.room),
            &bare(SENDER),
            HostGrantAction::SendMessage
        )
        .await,
        RoomGate::Managed(true)
    ));
    // The managed room under a spelling that is not canonical is refused.
    assert!(matches!(
        gate.room(RoomBinding::Room(&trailing_dot(&scope.room)))
            .await,
        RoomGate::Unavailable
    ));
    assert!(matches!(
        gate.room(RoomBinding::Room(&trailing_dot(&unmanaged_room)))
            .await,
        RoomGate::Unmanaged
    ));
    fixture.close().await;
}

#[tokio::test]
async fn the_set_ages_while_refreshes_fail_and_a_reload_resets_it() {
    let Some(fixture) = GrantFixture::active("hg_set_age").await else {
        return;
    };
    let scopes = loaded(&fixture.store).await;
    scopes.refresh().await.expect("refresh");
    let age = scopes.age().expect("a loaded set has an age");
    assert!(age < LOAD, "a set loaded just now is {age:?} old");
    assert!(
        !scopes.older_than(ENROLLED_SET_STALE_AFTER),
        "a set loaded just now is past the warning bound"
    );

    // The lifecycle ledger cannot be read: the refresh fails, the set
    // keeps aging from its last load and is past any bound it has crossed.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles RENAME TO host_grant_scope_lifecycles_hidden",
            (),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(scopes.refresh().await.is_err());
    assert!(scopes.age().expect("age") >= Duration::from_millis(50));
    assert!(scopes.older_than(Duration::from_millis(10)));
    assert!(
        !scopes.older_than(ENROLLED_SET_STALE_AFTER),
        "the warning bound is not yet reached"
    );

    // A successful reload makes the set fresh again.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles_hidden RENAME TO host_grant_scope_lifecycles",
            (),
        )
        .await;
    scopes.refresh().await.expect("refresh");
    assert!(!scopes.older_than(Duration::from_millis(10)));
    fixture.close().await;
}

#[tokio::test]
async fn a_load_is_dated_from_its_start_and_only_a_successful_one_moves_it() {
    let Some(fixture) = GrantFixture::active("hg_set_load_start").await else {
        return;
    };
    // Built outside the runtime: only this case's refreshes load it.
    let scopes = never_loaded(&fixture.store);
    assert_eq!(scopes.load_started(), None);
    assert_eq!(scopes.age(), None);

    let before = SystemTime::now();
    scopes.refresh().await.expect("first load");
    let after = SystemTime::now();
    let first = scopes.load_started().expect("a loaded set has a start");
    assert!(before <= first && first <= after, "{first:?}");

    // A session holds the lifecycle ledger, so the next load waits on it
    // after it has started. The set it installs is dated from that start,
    // not from when the wait ended.
    const HOLD: Duration = Duration::from_millis(600);
    let blocker = sqlx::PgPool::connect(fixture.ingress.db.database_url())
        .await
        .expect("blocking connection");
    let mut held = blocker.begin().await.expect("blocking transaction");
    sqlx::query("LOCK TABLE host_grant_scope_lifecycles IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .expect("hold the lifecycle ledger");
    let refreshing = {
        let scopes = scopes.clone();
        tokio::spawn(async move {
            let begun = SystemTime::now();
            scopes.refresh().await.map(|()| begun)
        })
    };
    tokio::time::sleep(HOLD).await;
    assert!(
        !refreshing.is_finished(),
        "the load did not wait on the lifecycle ledger"
    );
    let released = SystemTime::now();
    held.rollback().await.expect("release the lifecycle ledger");
    let begun = refreshing
        .await
        .expect("refresh task")
        .expect("a load after the wait");
    let second = scopes.load_started().expect("start of the second load");
    assert!(second > first);
    assert!(begun <= second, "{begun:?} {second:?}");
    assert!(
        second + HOLD / 2 <= released,
        "the load is dated from when its wait ended: {second:?} {released:?}"
    );
    assert!(scopes.age().expect("age") >= HOLD / 2);
    blocker.close().await;

    // A failed refresh keeps the set and its start.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles RENAME TO host_grant_scope_lifecycles_hidden",
            (),
        )
        .await;
    assert!(scopes.refresh().await.is_err());
    assert_eq!(scopes.load_started(), Some(second));
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_scope_lifecycles_hidden RENAME TO host_grant_scope_lifecycles",
            (),
        )
        .await;

    // So does a refresh that finds the relations gone after a load: it is
    // not a fresh load of an empty schema.
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_rooms RENAME TO host_grant_rooms_hidden",
            (),
        )
        .await;
    assert!(matches!(
        scopes.refresh().await,
        Err(HostGrantError::RelationsMissing)
    ));
    assert_eq!(scopes.load_started(), Some(second));
    assert_eq!(
        scopes.channel(&fixture.scope.channel_id),
        Enrollment::Enrolled
    );
    fixture
        .ingress
        .execute(
            "ALTER TABLE host_grant_rooms_hidden RENAME TO host_grant_rooms",
            (),
        )
        .await;

    scopes.refresh().await.expect("refresh");
    assert!(scopes.load_started().expect("start of the third load") > second);
    fixture.close().await;
}

#[tokio::test]
async fn a_schema_without_host_grant_relations_manages_nothing() {
    let Some(database) = IsolatedPostgresDatabase::create("hg_set_absent").await else {
        return;
    };
    let db = Database::from_config(
        "host-grant-absent",
        &DatabaseConfig::new(DatabaseDriver::Postgres, database.url().to_owned()),
    )
    .await
    .expect("database without migrations");
    let store = HostGrantStore::open(db.clone()).expect("store handle");
    let scopes = loaded(&store).await;
    let room = bare("room@muc.example.com");

    assert_eq!(scopes.channel("room"), Enrollment::Unmanaged);
    assert_eq!(scopes.room(&room), Enrollment::Unmanaged);
    assert_eq!(scopes.space_node("space"), Enrollment::Unmanaged);
    assert!(scopes.snapshot().is_some());
    // A schema that holds no scope has nothing to grow stale.
    assert!(!scopes.older_than(Duration::ZERO));
    let gate = HostGrantGate::with_enrolled_scopes(store, scopes);
    assert!(matches!(
        gate.room(RoomBinding::Room(&room)).await,
        RoomGate::Unmanaged
    ));
    assert_eq!(gate.space_enrolled("space").await, Ok(false));
    drop(gate);
    drop(db);
    database.drop_database().await;
}

#[tokio::test]
async fn legacy_reads_answer_nothing_for_enrolled_objects_and_fail_until_the_set_loads() {
    let Some(fixture) = GrantFixture::active("hg_set_reads").await else {
        return;
    };
    let scope = &fixture.scope;
    let channel = Object::new(ObjectType::Channel, scope.channel_id.as_str());
    let space = Object::new(ObjectType::Space, scope.space_node.as_str());
    let other = Object::new(ObjectType::Channel, "unmanaged-room");
    let deployment = Object::new(ObjectType::Server, "deployment");

    // A set that never loaded: every channel and Space read fails, whether
    // or not a scope manages it; other objects answer.
    let pending = never_loaded(&fixture.store);
    assert!(!pending.loaded());
    assert!(!pending.wait_loaded(Duration::from_millis(100)).await);
    assert_eq!(pending.channel("unmanaged-room"), Enrollment::Undecided);
    let (guarded, unguarded) = actors(&fixture, &pending);
    for object in [&channel, &space, &other] {
        legacy_member(&unguarded, object).await;
    }
    for object in [&channel, &space, &other] {
        assert!(matches!(
            legacy_check(&guarded, object).await,
            Err(SendError::HandlerError(
                PermissionError::HostGrantAuthorityUnavailable
            ))
        ));
    }
    assert!(legacy_check(&guarded, &deployment).await.is_ok());
    let gate = HostGrantGate::with_enrolled_scopes(fixture.store.clone(), pending.clone());
    assert!(gate.attach(
        HostGrantAttachment::unfenced_for_test(gate.store().expect("store"), served())
            .await
            .expect("attachment")
    ));
    assert!(matches!(
        gate.room(RoomBinding::Room(&bare("unmanaged@muc.example.com")))
            .await,
        RoomGate::Unavailable
    ));
    assert!(gate.channel_enrolled("unmanaged-room").await.is_err());

    // Once it loads, an enrolled object answers nothing and every other
    // object keeps its legacy answers.
    pending.refresh().await.expect("first load");
    for object in [&channel, &space] {
        assert_eq!(legacy_check(&guarded, object).await.ok(), Some(false));
        let relations = guarded
            .ask(ListRelations {
                subject: Subject::user(CAROL),
                object: object.clone(),
            })
            .await
            .expect("relations");
        assert!(relations.is_empty());
        let subjects = guarded
            .ask(ListSubjects {
                object: object.clone(),
                relation: Relation::new("member"),
            })
            .await
            .expect("subjects");
        assert!(subjects.is_empty());
    }
    assert_eq!(legacy_check(&guarded, &other).await.ok(), Some(true));
    assert!(matches!(
        gate.room(RoomBinding::Room(&bare("unmanaged@muc.example.com")))
            .await,
        RoomGate::Unmanaged
    ));
    fixture.close().await;
}
