//! XEP-0045 groupchat acceptance in a room managed by an enrolled host-grant
//! scope (ADR-0020 D5, first slice).
//!
//! A managed members-only room accepts a fresh groupchat only when the
//! canonical PostgreSQL policy state admits the sender and every seated
//! recipient, and the decision is made inside the acceptance transaction,
//! under the scope's guard. A refused stanza is answered with a committed
//! stanza error from the room to the sender: `forbidden` when the sender
//! holds no grant that allows the send, `recipient-unavailable` while the
//! room still seats someone whose grant no longer allows reading, and
//! `service-unavailable` when the scope admits nothing or no manifest can be
//! sealed. Rooms that no scope manages, and SQLite deployments, keep their
//! existing admission.
//!
//! These cases run the production commit path against PostgreSQL. SpiceDB is
//! not involved in admission, and the cases that need to show so use the
//! local permission backend as a projection that never answers. The fixture
//! activates each scope in place of a verified projection; the projection
//! itself is covered in `host_grant_projection.rs`.
#![cfg(feature = "test-support")]

mod host_grant_support;
mod ingress_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use host_grant_support::{
    bare, refusal, room_submission, sealed_manifest, subject, GrantFixture, OWNER, PEER,
    PEER_RESOURCE, SENDER,
};
use ingress_support::IngressFixture;
use kameo::actor::Spawn;
use kameo::error::SendError;
use tokio::sync::Notify;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::effects::ExternalEffect;
use waddle_server::ingress::{IngressDecisionClass, RoomExecutionPath};
use waddle_server::permissions::host_grant::{
    Activation, GrantOperationId, HostGrantAction, HostGrantAuthority, HostGrantError,
    HostGrantStore, MutationReceipt, Reconciliation, RetireScope, RoomAffiliation,
};
use waddle_server::permissions::{
    DeleteTuple, Object, ObjectType, PermissionActor, PermissionError, Relation,
    ReplaceExclusiveRelation, Subject, SubjectType, SwapExclusiveRelation, Tuple, WriteTuple,
};
use waddle_server::test_support::with_host_grant_admission_gate;
use waddle_xmpp::Stanza;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

const FORBIDDEN: (ErrorType, DefinedCondition) = (ErrorType::Auth, DefinedCondition::Forbidden);
const RECIPIENT_UNAVAILABLE: (ErrorType, DefinedCondition) =
    (ErrorType::Wait, DefinedCondition::RecipientUnavailable);
const SERVICE_UNAVAILABLE: (ErrorType, DefinedCondition) =
    (ErrorType::Cancel, DefinedCondition::ServiceUnavailable);

/// A raw connection pool on the fixture's database, for observing and
/// holding locks from outside the server's own pool.
async fn observer(fixture: &GrantFixture) -> sqlx::PgPool {
    sqlx::PgPool::connect(fixture.ingress.db.database_url())
        .await
        .expect("observer connection")
}

/// Wait until some backend of this database waits for a lock another
/// backend holds. PostgreSQL reports the conflict itself; the interval only
/// spaces the polls.
async fn wait_for_blocked_backend(observer: &sqlx::PgPool) {
    let deadline = Instant::now() + Duration::from_millis(2500);
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock' \
               AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0",
        )
        .fetch_one(observer)
        .await
        .expect("lock observation");
        if blocked > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no backend waited for the scope guard"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_room_no_scope_manages_keeps_its_admission() {
    // SQLite holds no canonical grant store: every room is unmanaged.
    let sqlite = IngressFixture::sqlite().await;
    let room = bare("open@muc.example.com");
    let submission = room_submission(&sqlite, &room, "sqlite-open", "hello", &[PEER_RESOURCE]);
    let decision = commit_submission(&sqlite.uow, &submission, 1)
        .await
        .expect("unmanaged room on SQLite");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert!(sealed_manifest(&sqlite, decision.message_key.expect("key"))
        .await
        .is_some());
    sqlite.close().await;

    // On PostgreSQL a room outside every scope is accepted as before, also
    // for a sender and an audience that hold no grant anywhere.
    let Some(fixture) = GrantFixture::active("hg_unmanaged_room").await else {
        return;
    };
    let submission = room_submission(
        &fixture.ingress,
        &room,
        "postgres-open",
        "hello",
        &["stranger@example.com/phone"],
    );
    let decision = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("unmanaged room on PostgreSQL");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert!(fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .is_some());
    fixture.close().await;
}

#[tokio::test]
async fn a_member_is_admitted_and_the_manifest_freezes_the_admitted_audience() {
    let Some(fixture) = GrantFixture::active("hg_admitted").await else {
        return;
    };
    let submission = fixture.room_submission("admitted", "hello members");
    let decision = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("admitted groupchat");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let manifest = fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .expect("sealed manifest");
    assert_eq!(manifest.room, fixture.scope.room);
    let mut targets: Vec<String> = manifest
        .children
        .iter()
        .filter_map(|child| child.target.as_ref().map(ToString::to_string))
        .collect();
    targets.sort();
    targets.dedup();
    assert_eq!(
        targets,
        vec![PEER_RESOURCE.to_owned(), submission.sender.to_string()]
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_revoke_committed_after_a_candidate_read_is_seen_by_acceptance() {
    let Some(fixture) = GrantFixture::active("hg_revoke_before").await else {
        return;
    };
    // The planner's view: the sender is a member, at this generation.
    let candidate = fixture
        .store
        .candidate_admission(
            &fixture.scope.room,
            &subject(SENDER),
            HostGrantAction::SendMessage,
        )
        .await
        .expect("candidate admission");
    assert!(candidate.admitted);
    assert_eq!(candidate.lifecycle, Some(fixture.scope.lifecycle));
    assert_eq!(candidate.generation, Some(fixture.generation().await));
    let submission = fixture.room_submission("stale-candidate", "sent after the revoke");

    // The revoke commits before acceptance begins.
    fixture
        .set(OWNER, SENDER, RoomAffiliation::None)
        .await
        .expect("revoke the sender");

    // Acceptance reads the policy state again, under the guard, and accepts
    // neither the manifest nor any child.
    let decision = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(refusal(&decision), FORBIDDEN);
    let [ExternalEffect::Frame(frame)] = decision.external.as_slice() else {
        panic!("a refusal sends one frame");
    };
    let Stanza::Message(reply) = frame.as_ref() else {
        panic!("a refusal replies with a message");
    };
    // XEP-0045: the error comes from the room and goes to the sender.
    assert_eq!(reply.from, Some(fixture.scope.room.clone().into()));
    assert_eq!(reply.to, Some(submission.sender.clone().into()));
    let key = decision.message_key.expect("the refusal is committed");
    assert!(fixture.manifest(key).await.is_none());
    // The canonical row of the refusal and its one error-reply intent; no
    // archive row and no delivery child.
    assert_eq!(fixture.accepted_rows().await, [1, 1, 0, 0]);

    // A retransmission resolves to the recorded refusal and accepts nothing.
    let replay = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("replayed refusal");
    assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
    assert_eq!(replay.message_key, Some(key));
    assert!(fixture.manifest(key).await.is_none());
    assert_eq!(fixture.accepted_rows().await, [1, 1, 0, 0]);
    fixture.close().await;
}

#[tokio::test]
async fn a_recipient_whose_grant_no_longer_allows_reading_is_never_frozen() {
    let Some(fixture) = GrantFixture::active("hg_recipient_revoked").await else {
        return;
    };
    fixture
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke the peer");

    // The room still seats the revoked peer: the stanza is refused whole,
    // so no child is frozen for the peer and none for anyone else.
    let seated = fixture.room_submission("peer-still-seated", "who reads this?");
    let decision = commit_submission(&fixture.ingress.uow, &seated, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(refusal(&decision), RECIPIENT_UNAVAILABLE);
    assert!(fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .is_none());

    // Once the room no longer seats the peer, the sender is admitted.
    let unseated = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "peer-unseated",
        "members only",
        &[],
    );
    let decision = commit_submission(&fixture.ingress.uow, &unseated, 1)
        .await
        .expect("admitted groupchat");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert!(fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .is_some());
    fixture.close().await;
}

#[tokio::test]
async fn acceptance_first_makes_the_revoke_wait_and_keeps_the_accepted_children() {
    let Some(fixture) = GrantFixture::active("hg_acceptance_first").await else {
        return;
    };
    let observer = observer(&fixture).await;
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let submission = fixture.room_submission("accepted-first", "accepted before the revoke");

    // The acceptance is admitted and holds the scope's guard.
    let acceptance = tokio::spawn({
        let uow = fixture.ingress.uow.clone();
        let submission = submission.clone();
        let (entered, proceed) = (Arc::clone(&entered), Arc::clone(&proceed));
        async move {
            with_host_grant_admission_gate(entered, proceed, async {
                commit_submission(&uow, &submission, 1).await
            })
            .await
        }
    });
    entered.notified().await;

    // The revoke cannot commit while the acceptance is unresolved.
    let command = fixture.command(OWNER, SENDER, RoomAffiliation::None).await;
    let revoke = tokio::spawn({
        let store = fixture.store.clone();
        let policy = fixture.policy.clone();
        let command = command.clone();
        async move { store.set_affiliation(&command, &policy).await }
    });
    wait_for_blocked_backend(&observer).await;
    assert!(!revoke.is_finished());
    assert_eq!(fixture.generation().await, command.expected_generation);

    proceed.notify_one();
    let decision = acceptance
        .await
        .expect("acceptance task")
        .expect("acceptance commits");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let receipt = revoke.await.expect("revoke task").expect("revoke commits");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );

    // The revoke erased nothing that was accepted: the manifest and its
    // children are as sealed, and a retransmission replays the acceptance
    // instead of deciding it again under today's policy.
    let key = decision.message_key.expect("key");
    let manifest = fixture.manifest(key).await.expect("sealed manifest");
    let replay = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("replayed acceptance");
    assert!(replay.class.advances());
    assert!(!matches!(
        replay.class,
        IngressDecisionClass::AuthorizationDenied | IngressDecisionClass::PolicyDenied
    ));
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(fixture.manifest(key).await, Some(manifest));

    // A new stanza of the revoked sender is refused.
    let later = fixture.room_submission("after-the-revoke", "not accepted");
    let decision = commit_submission(&fixture.ingress.uow, &later, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(refusal(&decision), FORBIDDEN);
    observer.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn a_backend_that_dies_before_commit_accepts_nothing() {
    let Some(fixture) = GrantFixture::active("hg_backend_death").await else {
        return;
    };
    let observer = observer(&fixture).await;
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let submission = fixture.room_submission("backend-death", "never acknowledged");
    let acceptance = tokio::spawn({
        let uow = fixture.ingress.uow.clone();
        let submission = submission.clone();
        let (entered, proceed) = (Arc::clone(&entered), Arc::clone(&proceed));
        async move {
            with_host_grant_admission_gate(entered, proceed, async {
                commit_submission(&uow, &submission, 1).await
            })
            .await
        }
    });
    entered.notified().await;

    // The acceptance transaction has read the policy state and holds the
    // guard. Its backend is terminated before it records or commits.
    let backends: Vec<i32> = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity \
         WHERE datname = current_database() AND state = 'idle in transaction' \
           AND pid <> pg_catalog.pg_backend_pid()",
    )
    .fetch_all(&observer)
    .await
    .expect("acceptance backend");
    let [backend] = backends.as_slice() else {
        panic!("expected one open acceptance transaction, found {backends:?}");
    };
    let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(backend)
        .fetch_one(&observer)
        .await
        .expect("terminate the backend");
    assert!(terminated);
    proceed.notify_one();

    // No decision is returned, so nothing can execute an effect, and
    // nothing was accepted.
    let failure = acceptance
        .await
        .expect("acceptance task")
        .expect_err("the acceptance cannot commit");
    assert!(!failure.class().advances());
    assert_eq!(fixture.accepted_rows().await, [0, 0, 0, 0]);
    assert_eq!(fixture.ingress.count("ingress_origin_aliases").await, 0);
    // The guard died with the backend: a mutation is not held up.
    fixture
        .set(OWNER, "carol@example.com", RoomAffiliation::Member)
        .await
        .expect("mutation after the dead acceptance");

    // The retransmission is accepted once, on its own transaction.
    let decision = commit_submission(&fixture.ingress.uow, &submission, 1)
        .await
        .expect("retransmission");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert!(fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .is_some());
    assert_eq!(fixture.ingress.count("ingress_messages").await, 1);
    observer.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn a_stalled_scope_fails_closed_within_bounds_and_other_rooms_progress() {
    let Some(fixture) = GrantFixture::active("hg_stalled_scope").await else {
        return;
    };
    let other = fixture.enroll_another().await;
    assert_eq!(
        fixture.activate_current(&other).await,
        Activation::Activated
    );
    let observer = observer(&fixture).await;

    // A writer holds the first scope's guard and does not finish.
    let mut stalled = observer.begin().await.expect("stalled transaction");
    sqlx::query("SELECT 1 FROM host_grant_scopes WHERE lifecycle_id = $1 FOR UPDATE")
        .bind(fixture.scope.lifecycle.to_storage())
        .execute(&mut *stalled)
        .await
        .expect("hold the scope guard");

    // Acceptance in that scope gives up after its bounded lock wait and
    // accepts nothing.
    let started = Instant::now();
    let blocked = fixture.room_submission("stalled-scope", "waits for nothing");
    let failure = commit_submission(&fixture.ingress.uow, &blocked, 1)
        .await
        .expect_err("the guard is held");
    assert_eq!(failure.class(), IngressDecisionClass::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fixture.accepted_rows().await, [0, 0, 0, 0]);

    // So does a mutation of that scope, with a typed result.
    let started = Instant::now();
    assert!(matches!(
        fixture.set(OWNER, PEER, RoomAffiliation::None).await,
        Err(HostGrantError::Busy)
    ));
    assert!(started.elapsed() < Duration::from_secs(10));
    // The policy read that prepares a command takes no lock.
    assert!(
        fixture
            .store
            .candidate_admission(
                &fixture.scope.room,
                &subject(SENDER),
                HostGrantAction::SendMessage
            )
            .await
            .expect("candidate admission")
            .admitted
    );

    // Another community and an unmanaged room are not held up.
    let elsewhere = room_submission(
        &fixture.ingress,
        &other.room,
        "other-scope",
        "another community",
        &[PEER_RESOURCE],
    );
    let decision = commit_submission(&fixture.ingress.uow, &elsewhere, 1)
        .await
        .expect("another scope");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    let unmanaged = room_submission(
        &fixture.ingress,
        &bare("open@muc.example.com"),
        "unmanaged-room",
        "local traffic",
        &[PEER_RESOURCE],
    );
    let decision = commit_submission(&fixture.ingress.uow, &unmanaged, 1)
        .await
        .expect("unmanaged room");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);

    // When the writer goes away, the same stanza is accepted.
    stalled.rollback().await.expect("release the scope guard");
    let decision = commit_submission(&fixture.ingress.uow, &blocked, 1)
        .await
        .expect("after the stall");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    observer.close().await;
    fixture.close().await;
}

#[tokio::test]
async fn admission_does_not_depend_on_the_projection() {
    let Some(fixture) = GrantFixture::active("hg_projection_absent").await else {
        return;
    };
    // A projection target that never gives an acknowledged answer.
    let unavailable = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
        fixture.ingress.db.clone(),
    )));
    let authority =
        HostGrantAuthority::new(fixture.store.clone(), unavailable, fixture.policy.clone());

    // A new grant is recorded and cannot become effective.
    let grant = fixture
        .command(OWNER, "carol@example.com", RoomAffiliation::Member)
        .await;
    authority.set_affiliation(&grant).await.expect("grant");
    assert_eq!(
        authority
            .reconcile(fixture.scope.lifecycle)
            .await
            .expect("reconciliation"),
        Reconciliation::ProjectionUnknown
    );
    assert_eq!(
        fixture.affiliations("carol@example.com").await,
        Some((RoomAffiliation::Member, RoomAffiliation::None))
    );
    let with_carol = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "pending-grant-seated",
        "carol is not admitted yet",
        &[PEER_RESOURCE, "carol@example.com/phone"],
    );
    let decision = commit_submission(&fixture.ingress.uow, &with_carol, 1)
        .await
        .expect("committed refusal");
    assert_eq!(refusal(&decision), RECIPIENT_UNAVAILABLE);

    // Grants that were effective keep admitting meanwhile.
    let members = fixture.room_submission("effective-grants", "still admitted");
    let decision = commit_submission(&fixture.ingress.uow, &members, 1)
        .await
        .expect("admitted groupchat");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);

    // A revoke takes effect at its commit, whatever the projection does.
    let revoke = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    authority.set_affiliation(&revoke).await.expect("revoke");
    let after_revoke = fixture.room_submission("revoked-peer", "the peer was revoked");
    let decision = commit_submission(&fixture.ingress.uow, &after_revoke, 1)
        .await
        .expect("committed refusal");
    assert_eq!(refusal(&decision), RECIPIENT_UNAVAILABLE);

    // A backlog of unprojected operations holds nothing up: mutations
    // commit, admission continues, and each pass reports the projection as
    // unknown without changing canonical state.
    for round in 0..12 {
        let affiliation = if round % 2 == 0 {
            RoomAffiliation::Outcast
        } else {
            RoomAffiliation::None
        };
        fixture
            .set(OWNER, "carol@example.com", affiliation)
            .await
            .expect("mutation with the projection behind");
    }
    let pending = authority.recover_pending().await.expect("recovery pass");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, fixture.scope.lifecycle);
    assert!(matches!(
        pending[0].1,
        Ok(Reconciliation::ProjectionUnknown)
    ));
    assert!(fixture.state(&fixture.scope).await.projection_pending());
    let alone = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "backlog",
        "admission continues",
        &[],
    );
    let decision = commit_submission(&fixture.ingress.uow, &alone, 1)
        .await
        .expect("admitted groupchat");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    fixture.close().await;
}

#[tokio::test]
async fn negative_overrides_win_over_alternate_grants_and_room_occupancy() {
    let Some(fixture) = GrantFixture::active("hg_negative_override").await else {
        return;
    };
    // Grants that other stores hold for the same room: the legacy tuple
    // mirror and the durable room affiliations. None of them is canonical.
    let alternate_grants = |account: &'static str| {
        let ingress = &fixture.ingress;
        let channel = fixture.scope.channel_id.clone();
        let room = fixture.scope.room.to_string();
        async move {
            for relation in ["owner", "member"] {
                ingress
                    .execute(
                        "INSERT INTO permission_tuples (id, object_type, object_id, relation, \
                         subject_type, subject_id) VALUES (?, 'channel', ?, ?, 'user', ?)",
                        waddle_server::db_params![
                            uuid::Uuid::new_v4().to_string(),
                            channel.as_str(),
                            relation,
                            account
                        ],
                    )
                    .await;
            }
            ingress
                .execute(
                    "INSERT INTO clustering_muc_room_affiliations (room_jid, member_jid, \
                     affiliation) VALUES (?, ?, 'owner') \
                     ON CONFLICT (room_jid, member_jid) DO UPDATE SET affiliation = 'owner'",
                    waddle_server::db_params![room.as_str(), account],
                )
                .await;
        }
    };

    // An account without any canonical affiliation is not a member of a
    // members-only room, whatever else names it and although the room seats
    // it.
    alternate_grants("dave@example.com").await;
    let with_stranger = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "unaffiliated-occupant",
        "members only",
        &[PEER_RESOURCE, "dave@example.com/phone"],
    );
    let decision = commit_submission(&fixture.ingress.uow, &with_stranger, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(refusal(&decision), RECIPIENT_UNAVAILABLE);

    // A banned recipient stays banned although it was an admin and other
    // stores still call it an owner.
    fixture
        .set(OWNER, PEER, RoomAffiliation::Admin)
        .await
        .expect("admin");
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );
    fixture
        .set(OWNER, PEER, RoomAffiliation::Outcast)
        .await
        .expect("ban the admin");
    alternate_grants(PEER).await;
    let banned_peer = fixture.room_submission("banned-recipient", "not for the outcast");
    let decision = commit_submission(&fixture.ingress.uow, &banned_peer, 1)
        .await
        .expect("committed refusal");
    assert_eq!(refusal(&decision), RECIPIENT_UNAVAILABLE);

    // A banned sender is refused every send, which is how a correction or a
    // retraction of its own message would travel as well.
    fixture
        .set(OWNER, SENDER, RoomAffiliation::Outcast)
        .await
        .expect("ban the sender");
    alternate_grants(SENDER).await;
    for action in HostGrantAction::ALL {
        assert!(
            !fixture
                .store
                .candidate_admission(&fixture.scope.room, &subject(SENDER), action)
                .await
                .expect("candidate admission")
                .admitted,
            "{action:?}"
        );
    }
    let banned_sender = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "banned-sender",
        "an outcast cannot send",
        &[],
    );
    let decision = commit_submission(&fixture.ingress.uow, &banned_sender, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(refusal(&decision), FORBIDDEN);
    fixture.close().await;
}

#[tokio::test]
async fn a_managed_room_accepts_nothing_without_an_admitted_manifest() {
    let Some(fixture) = GrantFixture::enrolled("hg_closed_scope").await else {
        return;
    };
    // Enrolled but never activated: the scope admits nothing.
    let early = fixture.room_submission("before-activation", "too early");
    let decision = commit_submission(&fixture.ingress.uow, &early, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(refusal(&decision), SERVICE_UNAVAILABLE);
    assert_eq!(
        fixture.activate_current(&fixture.scope).await,
        Activation::Activated
    );

    // An acceptance that freezes no manifest is not an admitted acceptance.
    let mut unsealed = fixture.room_submission("no-manifest", "no frozen children");
    let RoomExecutionPath::Local {
        manifest_snapshot, ..
    } = &mut unsealed.plan.room_execution
    else {
        panic!("local room plan");
    };
    *manifest_snapshot = None;
    let decision = commit_submission(&fixture.ingress.uow, &unsealed, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert_eq!(refusal(&decision), SERVICE_UNAVAILABLE);
    assert!(fixture
        .manifest(decision.message_key.expect("key"))
        .await
        .is_none());

    // A retired scope does not hand its room back to unmanaged admission.
    let generation = fixture.generation().await;
    fixture
        .store
        .retire_scope(&RetireScope {
            operation: GrantOperationId::generate(),
            lifecycle: fixture.scope.lifecycle,
            expected_generation: generation,
        })
        .await
        .expect("retire");
    let retired = fixture.room_submission("after-retirement", "closed for good");
    let decision = commit_submission(&fixture.ingress.uow, &retired, 1)
        .await
        .expect("committed refusal");
    assert_eq!(refusal(&decision), SERVICE_UNAVAILABLE);
    fixture.close().await;
}

/// The error a legacy tuple write is refused with.
fn legacy_refusal<T: std::fmt::Debug, M>(
    result: Result<T, SendError<M, PermissionError>>,
) -> PermissionError {
    match result {
        Err(SendError::HandlerError(error)) => error,
        Err(_) => panic!("the permission actor did not answer"),
        Ok(value) => panic!("the legacy write was not refused: {value:?}"),
    }
}

#[tokio::test]
async fn a_restart_keeps_legacy_routes_refused_and_restores_no_grant() {
    let Some(fixture) = GrantFixture::active("hg_restart").await else {
        return;
    };
    fixture
        .set(OWNER, SENDER, RoomAffiliation::Outcast)
        .await
        .expect("ban the sender");
    fixture
        .set(OWNER, PEER, RoomAffiliation::None)
        .await
        .expect("revoke the peer");

    // A new process: its own pool, store and permission actor, with nothing
    // carried over in memory.
    let restarted_db = Database::from_config(
        "host-grant-restart",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            fixture.ingress.db.database_url().to_owned(),
        ),
    )
    .await
    .expect("restarted database");
    let restarted_store = HostGrantStore::open(restarted_db.clone()).expect("restarted store");
    let guarded = PermissionActor::spawn(
        PermissionActor::new_for_tests(Arc::new(restarted_db.clone()))
            .with_host_grant_store(restarted_store.clone()),
    );

    // Every legacy tuple route is refused for the scope's channel and Space,
    // which is how a configured owner or a restored actor would write.
    let channel = Object::new(ObjectType::Channel, fixture.scope.channel_id.as_str());
    let space = Object::new(ObjectType::Space, fixture.scope.space_node.as_str());
    let family: Vec<Relation> = ["owner", "admin", "member", "outcast"]
        .into_iter()
        .map(Relation::new)
        .collect();
    for object in [&channel, &space] {
        let tuple = Tuple::new(
            object.clone(),
            Relation::new("owner"),
            Subject::user(SENDER),
        );
        assert!(matches!(
            legacy_refusal(
                guarded
                    .ask(WriteTuple {
                        tuple: tuple.clone()
                    })
                    .await
            ),
            PermissionError::HostGrantScopeEnrolled
        ));
        assert!(matches!(
            legacy_refusal(guarded.ask(DeleteTuple { tuple }).await),
            PermissionError::HostGrantScopeEnrolled
        ));
        assert!(matches!(
            legacy_refusal(
                guarded
                    .ask(ReplaceExclusiveRelation {
                        object: object.clone(),
                        subject: Subject::user(SENDER),
                        family: family.clone(),
                        replacement: Some(Relation::new("owner")),
                    })
                    .await
            ),
            PermissionError::HostGrantScopeEnrolled
        ));
        assert!(matches!(
            legacy_refusal(
                guarded
                    .ask(SwapExclusiveRelation {
                        object: object.clone(),
                        subject: Subject::user(SENDER),
                        family: family.clone(),
                        expected: Some(Relation::new("outcast")),
                        replacement: Some(Relation::new("owner")),
                    })
                    .await
            ),
            PermissionError::HostGrantScopeEnrolled
        ));
    }
    // Attaching another channel to the scope's Space is such a write too.
    assert!(matches!(
        legacy_refusal(
            guarded
                .ask(WriteTuple {
                    tuple: Tuple::new(
                        Object::new(ObjectType::Channel, "some-other-channel"),
                        Relation::new("parent"),
                        Subject::userset(SubjectType::Space, fixture.scope.space_node.as_str(), ""),
                    ),
                })
                .await
        ),
        PermissionError::HostGrantScopeEnrolled
    ));
    // Objects outside every scope are written as before.
    guarded
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "some-other-channel"),
                Relation::new("member"),
                Subject::user(SENDER),
            ),
        })
        .await
        .expect("unmanaged channel");

    // A writer that does not carry the guard can still leave tuples in the
    // legacy mirror. They grant nothing here: canonical state is unchanged
    // and admission keeps refusing.
    let unguarded = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
        restarted_db.clone(),
    )));
    unguarded
        .ask(WriteTuple {
            tuple: Tuple::new(
                channel.clone(),
                Relation::new("owner"),
                Subject::user(SENDER),
            ),
        })
        .await
        .expect("unguarded legacy write");
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::Outcast, RoomAffiliation::Outcast))
    );
    assert!(
        !restarted_store
            .candidate_admission(
                &fixture.scope.room,
                &subject(SENDER),
                HostGrantAction::SendMessage
            )
            .await
            .expect("candidate admission")
            .admitted
    );
    let after_restart = room_submission(
        &fixture.ingress,
        &fixture.scope.room,
        "after-restart",
        "still banned",
        &[],
    );
    let decision = commit_submission(&fixture.ingress.uow, &after_restart, 1)
        .await
        .expect("committed refusal");
    assert_eq!(decision.class, IngressDecisionClass::AuthorizationDenied);
    assert_eq!(refusal(&decision), FORBIDDEN);

    drop(guarded);
    drop(unguarded);
    drop(restarted_store);
    drop(restarted_db);
    fixture.close().await;
}
