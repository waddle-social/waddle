//! The acknowledged revocation fence of the host-grant authority (ADR-0020
//! D5; gates G12 and G16), at the canonical store and the replica's gate.
//!
//! A revoke takes effect at its PostgreSQL commit and is acknowledged once
//! every replica that may still decide has installed its generation: a
//! replica holds a lease, installs each scope's new generation before it
//! records it, and acts on a decision only while the decision's ticket is
//! current. These cases drive two replicas' gates on one database: the
//! acknowledgement of a live pair, the wait for a replica that stopped
//! renewing and what that replica may still do, the return of a lapsed
//! replica under a new registration, and the queue in front of the scope's
//! guard that keeps overlapping acceptances from holding a revoke back.
//!
//! They need PostgreSQL only, and no SpiceDB runs beside them: the
//! acknowledgement never consults the projection. A lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them without PostgreSQL.
#![cfg(feature = "test-support")]

// The shared host-grant fixture serves several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_support;
// The shared ingress fixture serves several suites; this one uses a subset.
#[allow(dead_code)]
mod ingress_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use host_grant_support::{bare, served, GrantFixture, OWNER, PEER, SENDER};
use jid::BareJid;
use tokio::sync::Notify;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::IngressDecisionClass;
use waddle_server::permissions::host_grant::{
    EnrolledScopes, FenceAcknowledgement, GrantGeneration, HostGrantAction, HostGrantAttachment,
    HostGrantGate, HostGrantStore, ManagedRoomGrants, MutationReceipt, RoomAffiliation,
    RoomBinding, RoomGate, FENCE_ACKNOWLEDGEMENT_DEADLINE, FENCE_EXPIRY_GRACE, FENCE_LEASE_MARGIN,
    FENCE_LEASE_TTL,
};
use waddle_server::test_support::with_host_grant_admission_gate;

/// One replica of the fixture's deployment: a store on a pool of its own,
/// its enrolled set, and a gate that holds an attachment.
struct Replica {
    gate: HostGrantGate,
}

impl Replica {
    async fn attach(fixture: &GrantFixture, name: &str) -> Self {
        let database = Database::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Postgres, fixture.ingress.db.database_url()),
        )
        .await
        .expect("replica database");
        let store = HostGrantStore::open(database).expect("replica store");
        let enrolled = EnrolledScopes::watch(store.clone());
        enrolled.refresh().await.expect("enrolled set");
        let gate = HostGrantGate::with_enrolled_scopes(store.clone(), enrolled);
        assert!(gate.attach(
            HostGrantAttachment::unfenced_for_test(&store, served())
                .await
                .expect("attachment")
        ));
        Self { gate }
    }

    /// The replica's decision for the fixture's room.
    async fn decide(&self, room: &BareJid) -> ManagedRoomGrants {
        match self.gate.room(RoomBinding::Room(room)).await {
            RoomGate::Managed(grants) => grants,
            other => panic!("the replica does not decide the managed room: {other:?}"),
        }
    }

    async fn unavailable(&self, room: &BareJid) -> bool {
        matches!(
            self.gate.room(RoomBinding::Room(room)).await,
            RoomGate::Unavailable
        )
    }

    fn suspend(&self, suspended: bool) {
        self.gate
            .fence()
            .expect("a PostgreSQL gate has a fence")
            .suspend_for_test(suspended);
    }
}

/// Record a revoke of `account` by the room's owner and answer its
/// generation.
async fn revoke(fixture: &GrantFixture, account: &str) -> GrantGeneration {
    let receipt = fixture
        .set(OWNER, account, RoomAffiliation::None)
        .await
        .expect("revoke");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    receipt.record().generation
}

/// Read the fence until it answers `expected`, for at most `limit`.
async fn fence_reads(
    fixture: &GrantFixture,
    generation: GrantGeneration,
    expected: FenceAcknowledgement,
    limit: Duration,
) {
    let deadline = Instant::now() + limit;
    loop {
        let reading = fixture
            .store
            .revocation_fence(fixture.scope.lifecycle, generation)
            .await
            .expect("fence reading");
        if reading == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fence reads {reading:?}, not {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_revoke_is_acknowledged_once_every_replica_has_installed_it() {
    let Some(fixture) = GrantFixture::active("hg_fence_pair").await else {
        return;
    };
    let room = fixture.scope.room.clone();
    let first = Replica::attach(&fixture, "fence-first").await;
    let second = Replica::attach(&fixture, "fence-second").await;
    let peer = bare(PEER);

    // Both replicas decide under a lease, and both admit the member.
    let before_first = first.decide(&room).await;
    let before_second = second.decide(&room).await;
    assert!(before_first.allows(&peer, HostGrantAction::Read));
    assert!(before_second.allows(&peer, HostGrantAction::Read));

    let generation = revoke(&fixture, PEER).await;
    let started = Instant::now();
    let reading = fixture
        .store
        .await_revocation_fence(
            fixture.scope.lifecycle,
            generation,
            FENCE_ACKNOWLEDGEMENT_DEADLINE,
        )
        .await
        .expect("fence reading");
    assert_eq!(reading, FenceAcknowledgement::Acknowledged { replicas: 2 });
    // Live replicas record a generation within a tick; nothing waits for a
    // lease to run out.
    assert!(started.elapsed() < FENCE_LEASE_TTL - FENCE_LEASE_MARGIN);

    // After the acknowledgement no replica acts on a decision read before
    // the revoke, however long its caller held it.
    for stale in [&before_first, &before_second] {
        assert!(!stale.fence.is_current());
        assert!(!stale.allows(&peer, HostGrantAction::Read));
        assert_eq!(stale.standing(&peer), RoomAffiliation::None);
        assert_eq!(stale.fence.admit(|| ()), None);
        // The other member's decision is void as well: the generation is
        // the scope's.
        assert!(!stale.allows(&bare(SENDER), HostGrantAction::Read));
    }
    // A decision read now sees the revoke and keeps the other member.
    for replica in [&first, &second] {
        let after = replica.decide(&room).await;
        assert!(after.fence.is_current());
        assert!(!after.allows(&peer, HostGrantAction::Read));
        assert!(after.allows(&bare(SENDER), HostGrantAction::Read));
        assert_eq!(after.fence.admit(|| 7), Some(7));
    }
    drop((first, second));
    fixture.close().await;
}

#[tokio::test]
async fn a_replica_that_stops_renewing_is_waited_for_until_it_can_no_longer_decide() {
    let Some(fixture) = GrantFixture::active("hg_fence_lapse").await else {
        return;
    };
    let room = fixture.scope.room.clone();
    let live = Replica::attach(&fixture, "fence-live").await;
    let stalled = Replica::attach(&fixture, "fence-stalled").await;
    let peer = bare(PEER);
    live.decide(&room).await;
    let before = stalled.decide(&room).await;
    assert!(before.allows(&peer, HostGrantAction::Read));

    // The stalled replica keeps its lease and installs nothing more: it is
    // a replica that lost its database or stopped running its fence.
    stalled.suspend(true);
    let stalled_at = Instant::now();
    let generation = revoke(&fixture, PEER).await;

    // The live replica records the revoke; the stalled one holds a lease
    // and has not, so the revoke is not acknowledged. Its earlier decision
    // is still one it would act on, which is why.
    fence_reads(
        &fixture,
        generation,
        FenceAcknowledgement::Pending {
            replicas: 2,
            lagging: 1,
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(before.fence.is_current());
    assert!(before.allows(&peer, HostGrantAction::Read));

    let reading = fixture
        .store
        .await_revocation_fence(
            fixture.scope.lifecycle,
            generation,
            FENCE_ACKNOWLEDGEMENT_DEADLINE,
        )
        .await
        .expect("fence reading");
    assert_eq!(reading, FenceAcknowledgement::Acknowledged { replicas: 1 });
    // The acknowledgement waited out the stalled replica's lease: it was
    // last renewed at most a tick before it stalled, and counts until the
    // grace after its end.
    let waited = stalled_at.elapsed();
    assert!(
        waited >= FENCE_LEASE_TTL - Duration::from_millis(600),
        "acknowledged after {waited:?}"
    );
    assert!(waited < FENCE_LEASE_TTL + FENCE_EXPIRY_GRACE + Duration::from_secs(2));

    // By then the stalled replica has stopped deciding on its own clock: its
    // earlier decision admits nothing and it makes no new one.
    assert!(!before.fence.is_current());
    assert!(!before.allows(&peer, HostGrantAction::Read));
    assert_eq!(before.fence.admit(|| ()), None);
    assert!(stalled.unavailable(&room).await);

    // It returns under a new registration, reads the revoke, and what it
    // decided under the lapsed one stays void.
    stalled.suspend(false);
    let after = stalled.decide(&room).await;
    assert!(!after.allows(&peer, HostGrantAction::Read));
    assert!(after.allows(&bare(SENDER), HostGrantAction::Read));
    assert!(!before.fence.is_current());
    let next = revoke(&fixture, SENDER).await;
    let reading = fixture
        .store
        .await_revocation_fence(
            fixture.scope.lifecycle,
            next,
            FENCE_ACKNOWLEDGEMENT_DEADLINE,
        )
        .await
        .expect("fence reading");
    assert_eq!(reading, FenceAcknowledgement::Acknowledged { replicas: 2 });
    drop((live, stalled));
    fixture.close().await;
}

#[tokio::test]
async fn a_replica_that_serves_no_scope_or_stopped_is_not_waited_for() {
    let Some(fixture) = GrantFixture::active("hg_fence_release").await else {
        return;
    };
    let room = fixture.scope.room.clone();
    // No replica has decided anything: nothing holds a lease.
    let generation = fixture.generation().await;
    assert_eq!(
        fixture
            .store
            .revocation_fence(fixture.scope.lifecycle, generation)
            .await
            .expect("fence reading"),
        FenceAcknowledgement::Acknowledged { replicas: 0 }
    );

    let replica = Replica::attach(&fixture, "fence-released").await;
    replica.decide(&room).await;
    let fence = replica.gate.fence().expect("fence").clone();
    assert!(fence.leased());
    assert_eq!(fence.installed(fixture.scope.lifecycle), Some(generation));
    fence_reads(
        &fixture,
        generation,
        FenceAcknowledgement::Acknowledged { replicas: 1 },
        Duration::from_secs(2),
    )
    .await;

    // A replica that stops releases its lease, so a revoke made right after
    // does not wait for the lease to run out.
    drop((replica, fence));
    let generation = revoke(&fixture, PEER).await;
    fence_reads(
        &fixture,
        generation,
        FenceAcknowledgement::Acknowledged { replicas: 0 },
        Duration::from_secs(2),
    )
    .await;
    fixture.close().await;
}

#[tokio::test]
async fn a_replica_without_an_attachment_holds_no_lease() {
    let Some(fixture) = GrantFixture::active("hg_fence_unattached").await else {
        return;
    };
    let enrolled = EnrolledScopes::watch(fixture.store.clone());
    enrolled.refresh().await.expect("enrolled set");
    let gate = HostGrantGate::with_enrolled_scopes(fixture.store.clone(), enrolled);
    assert!(matches!(
        gate.room(RoomBinding::Room(&fixture.scope.room)).await,
        RoomGate::Unavailable
    ));
    let fence = gate.fence().expect("fence");
    fence.start();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!fence.leased());
    assert_eq!(
        fixture
            .store
            .revocation_fence(fixture.scope.lifecycle, fixture.generation().await)
            .await
            .expect("fence reading"),
        FenceAcknowledgement::Acknowledged { replicas: 0 }
    );
    fixture.close().await;
}

/// Wait until a backend of the fixture's database waits for a lock that
/// another backend holds.
async fn wait_for_blocked_backend(observer: &sqlx::PgPool, at_least: i64) {
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
        if blocked >= at_least {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fewer than {at_least} backends waited for the scope guard"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn an_acceptance_that_arrives_behind_a_waiting_revoke_does_not_overtake_it() {
    let Some(fixture) = GrantFixture::active("hg_fence_queue").await else {
        return;
    };
    let observer = sqlx::PgPool::connect(fixture.ingress.db.database_url())
        .await
        .expect("observer connection");
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());

    // An acceptance is admitted and holds the scope's guard, shared.
    let first = fixture.room_submission("queue-first", "accepted before the revoke");
    let mut held = tokio::spawn({
        let uow = fixture.ingress.uow.clone();
        let (entered, proceed) = (Arc::clone(&entered), Arc::clone(&proceed));
        async move {
            with_host_grant_admission_gate(entered, proceed, async {
                commit_submission(&uow, &first, 1).await
            })
            .await
        }
    });
    tokio::select! {
        () = entered.notified() => {}
        outcome = &mut held => panic!("the acceptance finished before its gate: {outcome:?}"),
        () = tokio::time::sleep(Duration::from_secs(10)) => panic!("no acceptance reached its gate"),
    }

    // A revoke of the sender waits for it.
    let command = fixture.command(OWNER, SENDER, RoomAffiliation::None).await;
    let revoke = tokio::spawn({
        let store = fixture.store.clone();
        let policy = fixture.policy.clone();
        let command = command.clone();
        async move { store.set_affiliation(&command, &policy).await }
    });
    wait_for_blocked_backend(&observer, 1).await;
    assert!(!revoke.is_finished());

    // A second acceptance of the same sender arrives while the revoke
    // waits. It shares the guard's mode with the first, and a row lock alone
    // would admit it beside the first and keep the revoke waiting. The
    // queue in front of the guard puts it behind the revoke instead: it is
    // not admitted under the grant the waiting revoke removes.
    let second = fixture.room_submission("queue-second", "arrived behind the revoke");
    let late = commit_submission(&fixture.ingress.uow, &second, 1).await;
    if let Ok(decision) = &late {
        assert_ne!(
            decision.class,
            IngressDecisionClass::Accepted,
            "{decision:?}"
        );
        if let Some(key) = decision.message_key {
            assert!(fixture.manifest(key).await.is_none());
        }
    }
    assert!(!revoke.is_finished());
    assert_eq!(fixture.generation().await, command.expected_generation);

    // The first acceptance resolves, the revoke commits next, and the
    // sender's later stanza is refused.
    proceed.notify_one();
    let decision = held.await.expect("acceptance task").expect("acceptance");
    assert_eq!(
        decision.class,
        IngressDecisionClass::Accepted,
        "{decision:?}"
    );
    let receipt = revoke.await.expect("revoke task").expect("revoke commits");
    assert!(matches!(receipt, MutationReceipt::Recorded(_)));
    assert_eq!(
        fixture.affiliations(SENDER).await,
        Some((RoomAffiliation::None, RoomAffiliation::None))
    );
    let third = fixture.room_submission("queue-third", "after the revoke");
    let refused = commit_submission(&fixture.ingress.uow, &third, 1)
        .await
        .expect("committed refusal");
    assert_eq!(refused.class, IngressDecisionClass::AuthorizationDenied);
    observer.close().await;
    fixture.close().await;
}
