//! Revocation in a room an enrolled host-grant scope manages (XEP-0045 §9.1
//! and §9.4; ADR-0020 D5, gates G14 and G16): what a revoke's reply means
//! and what happens to the occupants it leaves seated.
//!
//! A revoke takes effect at its PostgreSQL commit. Its reply follows the
//! acknowledgement of the revocation fence: every replica that may still
//! decide for the scope has installed the revoke's generation, so none acts
//! on a decision read before it. The replica that hosts a room's actor then
//! removes the occupants the grants no longer admit, with status 301 for a
//! ban and status 321 for the loss of membership, lowers the affiliations
//! and roles its actor mirrors above the recorded ones, and refuses a join
//! or an administration request that was decided before the revoke when it
//! reaches the actor.
//!
//! The cases drive the production router over loopback sockets against one
//! or two replicas of a deployment on the fixture's PostgreSQL database
//! (`host_grant_revocation_support`). A second replica is a second state in
//! this process: it shares nothing with the first but the database. The
//! replica that stops running its fence in the middle of a revoke is
//! `two_processes`, where it is a second operating-system process.
//!
//! They need PostgreSQL only; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them without it.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;

use std::time::{Duration, Instant};

use host_grant_revocation_support::{
    admin_item, admin_item_with_reason, connect, error_condition, groupchat, join, mam_query,
    presence_from, receives_groupchat, role_item, room_presence, Deployment, Replica, NS_PING,
    SWEEP_BUDGET,
};
use host_grant_support::{bare, subject, OWNER, PEER, SENDER};
use jid::FullJid;
use kameo::error::SendError;
use waddle_server::permissions::host_grant::{
    FenceAcknowledgement, GrantOperationId, HostGrantAction, ManagedRoomGrants, OperatorRevoke,
    RetireScope, RoomAffiliation, RoomBinding, RoomGate, FENCE_LEASE_TTL,
};
use waddle_server::test_support::{sweep_host_grant_room, WebSocketState};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{get, is_error, is_result, set};
use waddle_xmpp::muc::admin::AdminItem;
use waddle_xmpp::muc::durable::AdminMutationId;
use waddle_xmpp::muc::room_actor::{
    AdmissionCheck, ApplyAdminItems, ApplyAdminItemsIfAdmitted, GetSnapshot, JoinAffiliationGrant,
    JoinWithAffiliation, JoinWithAffiliationIfAdmitted, RoomActor,
};
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::muc::RoomActorError;
use waddle_xmpp::{Affiliation, Role};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// How long a case waits to see that nothing arrives.
const QUIET: Duration = Duration::from_millis(1500);

async fn decision(state: &WebSocketState, room: &jid::BareJid) -> ManagedRoomGrants {
    match state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room))
        .await
    {
        RoomGate::Managed(grants) => grants,
        other => panic!("the replica does not decide the managed room: {other:?}"),
    }
}

async fn room_actor(
    state: &WebSocketState,
    room: &jid::BareJid,
) -> kameo::actor::ActorRef<RoomActor> {
    state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("room actor")
}

/// Whether `account` holds a seat in the room actor of `state`.
async fn seated(state: &WebSocketState, room: &jid::BareJid, account: &str) -> bool {
    let snapshot = room_actor(state, room)
        .await
        .ask(GetSnapshot)
        .await
        .expect("room snapshot");
    snapshot
        .room
        .occupants
        .values()
        .any(|occupant| occupant.real_jid.to_bare().as_str() == account)
}

#[tokio::test]
async fn a_revoked_member_leaves_with_status_321_and_a_banned_one_with_status_301() {
    let Some(deployment) = Deployment::start("hg_revoke_admin", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let server = &deployment.replicas[0];
    let mut owner = server.client("owner", "desk").await;
    let mut alice = server.client("alice", "phone").await;
    let mut romeo = server.client("romeo", "phone").await;
    for (client, nick) in [
        (&mut owner, "owner"),
        (&mut alice, "alice"),
        (&mut romeo, "romeo"),
    ] {
        let joined = join(client, &room, nick).await;
        assert_eq!(error_condition(&joined), None, "{nick} joins: {joined:?}");
    }

    // XEP-0045 §9.4: in a members-only room a member whose membership is
    // revoked is removed, with status 321, and the others are told.
    let reply = round_trip(
        &mut owner,
        set("revoke", room.as_str(), admin_item("none", PEER)),
    )
    .await;
    assert!(is_result(&reply), "the owner's revoke: {reply:?}");
    let own = presence_from(&mut alice, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the revoked member's own removal");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"321".to_owned()),
        "{:?}",
        own.statuses
    );
    assert!(
        own.statuses.contains(&"110".to_owned()),
        "{:?}",
        own.statuses
    );
    assert_eq!(own.affiliation.as_deref(), Some("none"));
    assert_eq!(own.role.as_deref(), Some("none"));
    let seen = presence_from(&mut romeo, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the removal as another occupant sees it");
    let seen = room_presence(&seen);
    assert!(seen.unavailable);
    assert_eq!(seen.statuses, vec!["321".to_owned()]);
    assert!(!seated(&server.state, &room, PEER).await);

    // XEP-0045 §9.1: a banned occupant is removed with status 301. The
    // removal is the room administration's own: it names the reason the
    // owner gave, which the sweep that follows the same revoke would not.
    let reply = round_trip(
        &mut owner,
        set(
            "ban",
            room.as_str(),
            admin_item_with_reason("outcast", SENDER, "treason"),
        ),
    )
    .await;
    assert!(is_result(&reply), "the owner's ban: {reply:?}");
    let own = presence_from(&mut romeo, &room, "romeo", SWEEP_BUDGET)
        .await
        .expect("the banned member's own removal");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"301".to_owned()),
        "{:?}",
        own.statuses
    );
    assert_eq!(own.affiliation.as_deref(), Some("outcast"));
    assert_eq!(own.reason.as_deref(), Some("treason"));
    assert!(own.actor, "the ban names who acted");

    // Neither enters again: the first for want of membership, the second
    // because it is banned.
    let rejoin = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&rejoin).as_deref(),
        Some("registration-required")
    );
    let rejoin = join(&mut romeo, &room, "romeo").await;
    assert_eq!(error_condition(&rejoin).as_deref(), Some("forbidden"));
    drop((owner, alice, romeo));
    deployment.close().await;
}

#[tokio::test]
async fn a_revoke_recorded_outside_the_room_removes_the_seated_occupant() {
    let Some(deployment) = Deployment::start("hg_revoke_operator", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let server = &deployment.replicas[0];
    let mut owner = server.client("owner", "desk").await;
    let mut alice = server.client("alice", "phone").await;
    let mut romeo = server.client("romeo", "phone").await;
    for (client, nick) in [
        (&mut owner, "owner"),
        (&mut alice, "alice"),
        (&mut romeo, "romeo"),
    ] {
        let joined = join(client, &room, nick).await;
        assert_eq!(error_condition(&joined), None, "{nick} joins: {joined:?}");
    }

    // An operator's revoke is recorded in the store and reaches no room
    // actor by itself. The replica installs its generation and sweeps the
    // room it hosts.
    let receipt = deployment
        .grants
        .store
        .operator_revoke(
            &OperatorRevoke {
                operation: GrantOperationId::generate(),
                lifecycle: deployment.grants.scope.lifecycle,
                room: room.clone(),
                subject: subject(PEER),
                ban: false,
            },
            &host_grant_support::served(),
        )
        .await
        .expect("operator revoke");
    let acknowledged = deployment
        .grants
        .store
        .await_revocation_fence(
            deployment.grants.scope.lifecycle,
            receipt.record().generation,
            Duration::from_secs(10),
        )
        .await
        .expect("fence reading");
    assert_eq!(
        acknowledged,
        FenceAcknowledgement::Acknowledged { replicas: 1 }
    );
    let own = presence_from(&mut alice, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the revoked member's own removal");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"321".to_owned()),
        "{:?}",
        own.statuses
    );
    let seen = presence_from(&mut romeo, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the removal as another occupant sees it");
    assert!(room_presence(&seen).statuses.contains(&"321".to_owned()));
    assert!(!seated(&server.state, &room, PEER).await);

    // An operator's ban removes with status 301.
    deployment
        .grants
        .store
        .operator_revoke(
            &OperatorRevoke {
                operation: GrantOperationId::generate(),
                lifecycle: deployment.grants.scope.lifecycle,
                room: room.clone(),
                subject: subject(SENDER),
                ban: true,
            },
            &host_grant_support::served(),
        )
        .await
        .expect("operator ban");
    let own = presence_from(&mut romeo, &room, "romeo", SWEEP_BUDGET)
        .await
        .expect("the banned member's own removal");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"301".to_owned()),
        "{:?}",
        own.statuses
    );

    // A scope that is retired admits nobody: its room is emptied, and the
    // owner keeps the standing the store records.
    deployment
        .grants
        .store
        .retire_scope(&RetireScope {
            operation: GrantOperationId::generate(),
            lifecycle: deployment.grants.scope.lifecycle,
            expected_generation: deployment.grants.generation().await,
        })
        .await
        .expect("retirement");
    let own = presence_from(&mut owner, &room, "owner", SWEEP_BUDGET)
        .await
        .expect("the owner's removal from a retired scope's room");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"321".to_owned()),
        "{:?}",
        own.statuses
    );
    assert_eq!(
        deployment
            .grants
            .affiliations(OWNER)
            .await
            .map(|held| held.0),
        Some(RoomAffiliation::Owner)
    );
    drop((owner, alice, romeo));
    deployment.close().await;
}

#[tokio::test]
async fn a_revoke_is_acknowledged_on_every_replica_before_its_reply() {
    let Some(deployment) = Deployment::start("hg_revoke_pair", 2).await else {
        return;
    };
    let room = deployment.room().clone();
    let (first, second) = (&deployment.replicas[0], &deployment.replicas[1]);
    let mut owner = first.client("owner", "desk").await;
    let mut alice = second.client("alice", "phone").await;
    let mut romeo = second.client("romeo", "phone").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    for (client, nick) in [(&mut alice, "alice"), (&mut romeo, "romeo")] {
        let joined = join(client, &room, nick).await;
        assert_eq!(error_condition(&joined), None, "{nick} joins: {joined:?}");
    }
    assert!(seated(&second.state, &room, PEER).await);

    // A decision the second replica made before the revoke, for work it has
    // not done yet.
    let alice_jid: FullJid = alice
        .full_jid
        .as_deref()
        .expect("bound")
        .parse()
        .expect("jid");
    let before = decision(&second.state, &room).await;
    assert!(before.allows(&bare(PEER), HostGrantAction::Read));

    // The owner revokes the member through the first replica.
    let reply = round_trip(
        &mut owner,
        set("revoke", room.as_str(), admin_item("none", PEER)),
    )
    .await;
    assert!(is_result(&reply), "the owner's revoke: {reply:?}");

    // The reply followed the fence: both replicas have installed the
    // revoke's generation, so the second acts on nothing it decided before.
    let generation = deployment.grants.generation().await;
    assert_eq!(
        deployment
            .grants
            .store
            .revocation_fence(deployment.grants.scope.lifecycle, generation)
            .await
            .expect("fence reading"),
        FenceAcknowledgement::Acknowledged { replicas: 2 }
    );
    assert!(!before.fence.is_current());
    assert!(!before.allows(&bare(PEER), HostGrantAction::Read));

    // The second replica hosts the member's seat and removes it.
    let own = presence_from(&mut alice, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the revoked member's removal on the other replica");
    let own = room_presence(&own);
    assert!(own.unavailable);
    assert!(
        own.statuses.contains(&"321".to_owned()),
        "{:?}",
        own.statuses
    );
    let seen = presence_from(&mut romeo, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the removal as another occupant sees it");
    assert!(room_presence(&seen).statuses.contains(&"321".to_owned()));
    assert!(!seated(&second.state, &room, PEER).await);

    // A join that was decided before the revoke reaches the room actor
    // now, and seats nobody.
    let queued_join = stale_join(&second.state, &room, &alice_jid, &before).await;
    let outcome = room_actor(&second.state, &room)
        .await
        .ask(queued_join)
        .await;
    assert!(
        matches!(
            outcome,
            Err(SendError::HandlerError(
                RoomActorError::StaleAdmissionRevision
            ))
        ),
        "a join decided before the revoke: {:?}",
        outcome.map(|_| "seated")
    );
    assert!(!seated(&second.state, &room, PEER).await);

    // Every new use on the second replica is decided from the grants: the
    // archive, a join, and a message of another member, which the revoked
    // account no longer receives.
    let reply = round_trip(&mut alice, set("mam", room.as_str(), mam_query("q1"))).await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden)
            || is_error(&reply, DefinedCondition::ItemNotFound),
        "the revoked member's archive query: {reply:?}"
    );
    let rejoin = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&rejoin).as_deref(),
        Some("registration-required")
    );
    romeo
        .send(&groupchat(&room, "after-revoke", "for the members only"))
        .await
        .expect("send groupchat");
    assert!(receives_groupchat(&mut romeo, &room, "for the members only", SWEEP_BUDGET).await);
    assert!(!receives_groupchat(&mut alice, &room, "for the members only", QUIET).await);
    drop((owner, alice, romeo));
    deployment.close().await;
}

/// A join of `joiner` the replica admitted with `decided`, as the join
/// handler sends it to the room actor: with the decision's ticket.
async fn stale_join(
    state: &WebSocketState,
    room: &jid::BareJid,
    joiner: &FullJid,
    decided: &ManagedRoomGrants,
) -> JoinWithAffiliationIfAdmitted {
    let snapshot = room_actor(state, room)
        .await
        .ask(GetSnapshot)
        .await
        .expect("room snapshot");
    let ticket = decided.fence.clone();
    JoinWithAffiliationIfAdmitted {
        join: JoinWithAffiliation {
            sender_jid: joiner
                .to_bare()
                .with_resource_str("tablet")
                .expect("resource"),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_owned(),
            // The actor's own revision as it stands: only the decision's
            // ticket can refuse this join.
            admission_revision: snapshot.admission_revision,
            session: OccupancySessionGeneration::mint(),
        },
        admission: AdmissionCheck::new(move || ticket.is_current()),
    }
}

#[tokio::test]
async fn a_demotion_lowers_the_role_and_voids_an_administration_decided_before_it() {
    let Some(deployment) = Deployment::start("hg_revoke_role", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let server = &deployment.replicas[0];
    // The peer is an admin whose grant is effective.
    deployment
        .grants
        .set(OWNER, PEER, RoomAffiliation::Admin)
        .await
        .expect("admin grant");
    deployment
        .grants
        .activate_current(&deployment.grants.scope)
        .await;
    let mut alice = server.client("alice", "phone").await;
    let mut romeo = server.client("romeo", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    let joined = room_presence(&joined);
    assert_eq!(joined.affiliation.as_deref(), Some("admin"));
    assert_eq!(joined.role.as_deref(), Some("moderator"));
    let joined = join(&mut romeo, &room, "romeo").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");

    // The admin's request to remove another occupant is decided, and waits.
    let alice_jid: FullJid = alice
        .full_jid
        .as_deref()
        .expect("bound")
        .parse()
        .expect("jid");
    let before = decision(&server.state, &room).await;
    assert!(before.allows(&bare(PEER), HostGrantAction::Moderate));
    let kick = |decided: &ManagedRoomGrants| {
        let ticket = decided.fence.clone();
        ApplyAdminItemsIfAdmitted {
            apply: ApplyAdminItems {
                attempt: AdminMutationId::generate(),
                sender_jid: alice_jid.clone(),
                sender_affiliation: Affiliation::Admin,
                sender_role: Role::Moderator,
                items: vec![AdminItem {
                    jid: None,
                    nick: Some("romeo".to_owned()),
                    affiliation: None,
                    role: Some(Role::None),
                    reason: None,
                }],
            },
            admission: AdmissionCheck::new(move || ticket.is_current()),
        }
    };
    let queued = kick(&before);

    // The owner demotes the admin outside the room.
    let receipt = deployment
        .grants
        .set(OWNER, PEER, RoomAffiliation::Member)
        .await
        .expect("demotion");
    let acknowledged = deployment
        .grants
        .store
        .await_revocation_fence(
            deployment.grants.scope.lifecycle,
            receipt.record().generation,
            Duration::from_secs(10),
        )
        .await
        .expect("fence reading");
    assert!(acknowledged.acknowledged());

    // The sweep lowers what the room actor mirrored: the affiliation, and
    // with it the moderator role.
    let lowered = async {
        loop {
            let presence = presence_from(&mut romeo, &room, "alice", SWEEP_BUDGET)
                .await
                .expect("the demoted admin's presence");
            let presence = room_presence(&presence);
            if presence.affiliation.as_deref() == Some("member") {
                return presence;
            }
        }
    }
    .await;
    assert!(!lowered.unavailable);
    assert_eq!(lowered.role.as_deref(), Some("participant"));

    // The request decided before the demotion reaches the room actor after
    // it and changes nothing; the same request from the demoted account is
    // refused when it is decided now.
    let actor = room_actor(&server.state, &room).await;
    let outcome = actor.ask(queued).await.expect("room actor");
    assert!(
        outcome.is_none(),
        "a kick decided before the demotion applied"
    );
    assert!(seated(&server.state, &room, SENDER).await);
    let reply = round_trip(
        &mut alice,
        set("kick", room.as_str(), role_item("none", "romeo")),
    )
    .await;
    assert!(
        is_error(&reply, DefinedCondition::Forbidden),
        "a kick by the demoted admin: {reply:?}"
    );
    assert!(seated(&server.state, &room, SENDER).await);

    // A decision that is current is applied.
    let current = decision(&server.state, &room).await;
    let outcome = actor.ask(kick(&current)).await.expect("room actor");
    assert!(outcome.is_some());
    assert!(!seated(&server.state, &room, SENDER).await);
    drop((alice, romeo));
    deployment.close().await;
}

#[tokio::test]
async fn the_sweep_changes_nothing_while_the_grants_cannot_be_read() {
    let Some(deployment) = Deployment::start("hg_revoke_unreadable", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let server = &deployment.replicas[0];
    let mut alice = server.client("alice", "phone").await;
    let joined = join(&mut alice, &room, "alice").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");

    // A room in line with its grants is left alone.
    assert!(sweep_host_grant_room(&server.state, &room).await);
    assert!(seated(&server.state, &room, PEER).await);

    // The grants cannot be read (the host-grant relations are locked by
    // another session): the sweep reports that it did not finish and
    // removes nobody, however long that lasts.
    let blocker = sqlx::PgPool::connect(deployment.grants.ingress.db.database_url())
        .await
        .expect("blocking connection");
    let mut held = blocker.begin().await.expect("blocking transaction");
    sqlx::query("LOCK TABLE host_grants IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .expect("lock");
    assert!(!sweep_host_grant_room(&server.state, &room).await);
    assert!(seated(&server.state, &room, PEER).await);
    held.rollback().await.expect("release");
    blocker.close().await;
    assert!(
        presence_from(&mut alice, &room, "alice", QUIET)
            .await
            .is_none(),
        "an unreadable store removed an occupant"
    );
    assert!(sweep_host_grant_room(&server.state, &room).await);
    assert!(seated(&server.state, &room, PEER).await);
    drop(alice);
    deployment.close().await;
}

/// The database a second replica process serves, the deployment it is
/// enrolled for, and the file the process writes its endpoint to.
const REPLICA_DATABASE: &str = "WADDLE_REVOCATION_REPLICA_DATABASE";
const REPLICA_DEPLOYMENT: &str = "WADDLE_REVOCATION_REPLICA_DEPLOYMENT";
const REPLICA_ENDPOINT_FILE: &str = "WADDLE_REVOCATION_REPLICA_ENDPOINT_FILE";

/// The body of a second replica process: a replica of the deployment on the
/// given database, served until the process is killed. Run by the suite
/// itself it does nothing.
#[tokio::test]
async fn second_replica_process_entry() {
    let Ok(database) = std::env::var(REPLICA_DATABASE) else {
        return;
    };
    let variable = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is set"));
    let deployment = variable(REPLICA_DEPLOYMENT)
        .parse()
        .expect("deployment UUID");
    let endpoint_file = std::path::PathBuf::from(variable(REPLICA_ENDPOINT_FILE));
    let replica = Replica::start(&database, deployment).await;
    // The endpoint appears in one step, once the replica serves.
    let staged = endpoint_file.with_extension("staged");
    std::fs::write(&staged, &replica.url).expect("write the endpoint");
    std::fs::rename(&staged, &endpoint_file).expect("publish the endpoint");
    tokio::time::sleep(Duration::from_secs(600)).await;
}

/// A second replica of a deployment, as a separate operating-system process
/// of this test binary.
struct ReplicaProcess {
    process: tokio::process::Child,
    url: String,
    _directory: tempfile::TempDir,
}

impl ReplicaProcess {
    async fn start(deployment: &Deployment) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let endpoint_file = directory.path().join("endpoint");
        let process =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "second_replica_process_entry", "--nocapture"])
                .env(
                    REPLICA_DATABASE,
                    deployment.grants.ingress.db.database_url(),
                )
                .env(
                    REPLICA_DEPLOYMENT,
                    deployment.grants.ingress.deployment_uuid().to_string(),
                )
                .env(REPLICA_ENDPOINT_FILE, &endpoint_file)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("start the replica process");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let url = loop {
            if let Ok(url) = std::fs::read_to_string(&endpoint_file) {
                break url;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the replica process did not start serving"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        Self {
            process,
            url,
            _directory: directory,
        }
    }

    /// Stop or continue the process without ending it: a stopped process
    /// renews no lease, installs nothing and answers nothing, and knows
    /// nothing of the time that passes.
    fn signal(&self, signal: libc::c_int) {
        let pid = self.process.id().expect("a running replica process") as libc::pid_t;
        // SAFETY: the pid names a live child this value owns and has not
        // reaped, so the signal cannot reach a recycled pid.
        let result = unsafe { libc::kill(pid, signal) };
        assert_eq!(result, 0, "signalling the replica process failed");
    }
}

/// A ping of an occupant's own nick (XEP-0410).
fn self_ping(id: &str, room: &jid::BareJid, nick: &str) -> xmpp_parsers::iq::Iq {
    get(
        id,
        &format!("{room}/{nick}"),
        Element::builder("ping", NS_PING).build(),
    )
}

#[tokio::test]
async fn two_processes_a_stopped_replica_is_waited_for_and_admits_nothing_when_it_continues() {
    let Some(deployment) = Deployment::start("hg_revoke_processes", 1).await else {
        return;
    };
    let room = deployment.room().clone();
    let first = &deployment.replicas[0];
    let second = ReplicaProcess::start(&deployment).await;
    let lifecycle = deployment.grants.scope.lifecycle;

    // The owner is on this process's replica; two members sit in the room
    // actor of the other process.
    let mut owner = first.client("owner", "desk").await;
    let joined = join(&mut owner, &room, "owner").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    let mut alice = connect(&second.url, "alice", "phone").await;
    let mut romeo = connect(&second.url, "romeo", "phone").await;
    for (client, nick) in [(&mut alice, "alice"), (&mut romeo, "romeo")] {
        let joined = join(client, &room, nick).await;
        assert_eq!(error_condition(&joined), None, "{nick} joins: {joined:?}");
    }
    let reply = round_trip(&mut alice, self_ping("joined", &room, "alice")).await;
    assert!(is_result(&reply), "a seated member's self-ping: {reply:?}");

    // While both processes run, a revoke is acknowledged by both within a
    // tick of the other's fence, and the other process removes the seat it
    // hosts.
    let reply = round_trip(
        &mut owner,
        set("revoke-romeo", room.as_str(), admin_item("none", SENDER)),
    )
    .await;
    assert!(is_result(&reply), "the owner's revoke: {reply:?}");
    assert_eq!(
        deployment
            .grants
            .store
            .revocation_fence(lifecycle, deployment.grants.generation().await)
            .await
            .expect("fence reading"),
        FenceAcknowledgement::Acknowledged { replicas: 2 }
    );
    let removed = presence_from(&mut romeo, &room, "romeo", SWEEP_BUDGET)
        .await
        .expect("the revoked member's removal in the other process");
    assert!(room_presence(&removed).statuses.contains(&"321".to_owned()));

    // The other process stops: it holds a lease, cannot renew it and
    // installs nothing. The owner's next revoke is recorded at once and its
    // reply waits until that lease has run out, when the stopped process
    // can no longer be deciding anything.
    second.signal(libc::SIGSTOP);
    let started = Instant::now();
    let reply = round_trip(
        &mut owner,
        set("revoke-alice", room.as_str(), admin_item("none", PEER)),
    )
    .await;
    assert!(is_result(&reply), "the owner's revoke: {reply:?}");
    let waited = started.elapsed();
    assert!(
        waited >= FENCE_LEASE_TTL - Duration::from_millis(800),
        "the revoke was acknowledged after {waited:?}, before the stopped replica's lease ran out"
    );
    assert_eq!(
        deployment
            .grants
            .store
            .revocation_fence(lifecycle, deployment.grants.generation().await)
            .await
            .expect("fence reading"),
        FenceAcknowledgement::Acknowledged { replicas: 1 }
    );

    // It continues with everything it held when it stopped: the member's
    // seat, and a lease that ran out while it was not looking. It decides
    // nothing under that lease. From the first answer on the revoked member
    // is never told it is joined, and it is removed once the process has
    // registered again and swept its rooms.
    second.signal(libc::SIGCONT);
    let reply = round_trip(&mut alice, self_ping("continued", &room, "alice")).await;
    assert!(
        is_error(&reply, DefinedCondition::RemoteServerTimeout)
            || is_error(&reply, DefinedCondition::NotAcceptable),
        "the revoked member's self-ping when the process continues: {reply:?}"
    );
    let removed = presence_from(&mut alice, &room, "alice", SWEEP_BUDGET)
        .await
        .expect("the revoked member's removal once the process has registered again");
    let removed = room_presence(&removed);
    assert!(removed.unavailable);
    assert!(
        removed.statuses.contains(&"321".to_owned()),
        "{:?}",
        removed.statuses
    );
    let reply = round_trip(&mut alice, self_ping("removed", &room, "alice")).await;
    assert!(
        is_error(&reply, DefinedCondition::NotAcceptable),
        "the revoked member's self-ping after its removal: {reply:?}"
    );
    let rejoin = join(&mut alice, &room, "alice").await;
    assert_eq!(
        error_condition(&rejoin).as_deref(),
        Some("registration-required")
    );
    // The process serves again under a new registration.
    let generation = deployment.grants.generation().await;
    let reading = deployment
        .grants
        .store
        .await_revocation_fence(lifecycle, generation, Duration::from_secs(10))
        .await
        .expect("fence reading");
    assert_eq!(reading, FenceAcknowledgement::Acknowledged { replicas: 2 });
    drop((owner, alice, romeo, second));
    deployment.close().await;
}
