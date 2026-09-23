use super::*;
use crate::muc::durable::{AdminMutationId, MucDurableFuture, MucDurableStore};
use crate::muc::{RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct AdminReceiptStore {
    receipts: Mutex<HashMap<AdminMutationId, RoomCommittedCoordinates>>,
    fail_reads: AtomicBool,
}

impl MucDurableStore for AdminReceiptStore {
    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a crate::muc::RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<crate::muc::DurableRoomState>> {
        Box::pin(async { Ok(None) })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a crate::muc::RoomClaimFenceContext,
        _intent: crate::muc::RoomDurableMutation,
        _effects: crate::muc::RoomMutationEffects,
    ) -> crate::muc::RoomCommitFuture<'a> {
        Box::pin(async { Err(crate::muc::RoomCommitError::NotOwner) })
    }

    fn load_admin_mutation_receipt<'a>(
        &'a self,
        _room: &'a BareJid,
        attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, Option<RoomCommittedCoordinates>> {
        Box::pin(async move {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(crate::XmppError::internal("receipt read unavailable"));
            }
            Ok(self
                .receipts
                .lock()
                .expect("receipts")
                .get(&attempt)
                .copied())
        })
    }

    fn delete_admin_mutation_receipt<'a>(
        &'a self,
        _room: &'a BareJid,
        attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            self.receipts.lock().expect("receipts").remove(&attempt);
            Ok(())
        })
    }
}

struct AdminRecoveryFixture {
    source: MucRoom,
    authoritative: MucRoom,
    coordinates: RoomCommittedCoordinates,
    projection: PendingAdminProjection,
    store: Arc<AdminReceiptStore>,
}

impl AdminRecoveryFixture {
    fn new() -> Self {
        let mut source = test_room();
        let mut authoritative = test_room();
        for nick in ["alice", "bob"] {
            let jid = test_full_jid(nick);
            source.set_affiliation(jid.to_bare(), Affiliation::Member);
            authoritative.set_affiliation(jid.to_bare(), Affiliation::Member);
            source.add_occupant(crate::muc::Occupant {
                real_jid: jid,
                nick: nick.to_owned(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
                is_remote: false,
                home_server: None,
            });
        }
        authoritative.set_affiliation(test_full_jid("bob").to_bare(), Affiliation::Outcast);
        let previous_coordinates = RoomCommittedCoordinates {
            lifecycle: RoomLifecycleId::generate(),
            revision: RoomRevision::initial(),
        };
        Self {
            source,
            authoritative,
            coordinates: RoomCommittedCoordinates {
                revision: previous_coordinates.revision.next().expect("revision"),
                ..previous_coordinates
            },
            // The original batch was [alice outcast, alice member, bob outcast].
            projection: PendingAdminProjection {
                attempt: AdminMutationId::generate(),
                previous_coordinates,
                expected_affiliations: vec![
                    (test_full_jid("alice").to_bare(), Affiliation::Member),
                    (test_full_jid("bob").to_bare(), Affiliation::Outcast),
                ],
                removed_sessions: vec![test_full_jid("alice"), test_full_jid("bob")],
                changed_roles: Vec::new(),
                moderated: false,
            },
            store: Arc::new(AdminReceiptStore::default()),
        }
    }

    fn record_commit(&self) {
        self.store
            .receipts
            .lock()
            .expect("receipts")
            .insert(self.projection.attempt, self.coordinates);
    }

    fn spawn(&self) -> ActorRef<RoomActor> {
        let mut actor = RoomActor::new(self.authoritative.clone(), test_secret());
        actor.durable_coordinates = Some(self.coordinates);
        actor.durable_store = Some(self.store.clone());
        RoomActor::spawn(actor)
    }

    fn restore(&self) -> RestoreLiveRoster {
        RestoreLiveRoster {
            room: self.source.clone(),
            occupancy_revision: 7,
            departures: Default::default(),
            pending_admin_projection: Some(self.projection.clone()),
        }
    }
}

#[tokio::test]
async fn restoring_live_roster_keeps_rolled_back_intermediate_ban_after_foreign_commit() {
    let fixture = AdminRecoveryFixture::new();
    // No receipt: the original transaction rolled back. A foreign owner
    // committed only bob's ban at the same next revision and final state.
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore rolled-back batch");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.get_occupant("alice").is_some());
    assert!(
        snapshot.room.get_occupant("bob").is_none(),
        "the current durable ban still applies"
    );
    assert_eq!(snapshot.occupancy_revision, 7);
    assert_eq!(
        snapshot.admin_mutation_resolution,
        Some(AdminMutationResolution::NotCommitted {
            attempt: fixture.projection.attempt
        })
    );
}

#[tokio::test]
async fn restoring_live_roster_applies_exact_commit_after_later_affiliation_change() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.record_commit();
    fixture.coordinates.revision = fixture.coordinates.revision.next().expect("later revision");
    fixture
        .authoritative
        .set_affiliation(test_full_jid("bob").to_bare(), Affiliation::Member);
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore proven batch after foreign unban");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(
        snapshot.admin_mutation_resolution,
        Some(AdminMutationResolution::Committed {
            attempt: fixture.projection.attempt,
            coordinates: RoomCommittedCoordinates {
                revision: fixture
                    .projection
                    .previous_coordinates
                    .revision
                    .next()
                    .expect("committed revision"),
                ..fixture.coordinates
            },
        }),
        "the verdict names the original commit rather than the later owner's revision"
    );
    assert!(snapshot.room.get_occupant("alice").is_none());
    assert!(
        snapshot.room.get_occupant("bob").is_none(),
        "restored membership does not undo an earlier session removal"
    );
    assert!(
        !fixture.store.receipts.lock().expect("receipts").is_empty(),
        "unpublished preparation retains its proof"
    );
}

#[tokio::test]
async fn restoring_live_roster_requires_matching_attempt_receipt() {
    let fixture = AdminRecoveryFixture::new();
    fixture
        .store
        .receipts
        .lock()
        .expect("receipts")
        .insert(AdminMutationId::generate(), fixture.coordinates);
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore without original attempt proof");
    assert!(actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room
        .get_occupant("alice")
        .is_some());
}

#[tokio::test]
async fn restoring_live_roster_rejects_receipt_from_wrong_lifecycle_or_revision() {
    for mismatch in 0..3 {
        let fixture = AdminRecoveryFixture::new();
        let mut receipt = fixture.coordinates;
        match mismatch {
            0 => receipt.lifecycle = RoomLifecycleId::generate(),
            1 => receipt.revision = RoomRevision::initial(),
            _ => receipt.revision = receipt.revision.next().expect("future revision"),
        }
        fixture
            .store
            .receipts
            .lock()
            .expect("receipts")
            .insert(fixture.projection.attempt, receipt);
        let actor = fixture.spawn();
        actor
            .ask(fixture.restore())
            .await
            .expect("restore without compatible receipt");
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .admin_mutation_resolution,
            None
        );
        assert!(actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_occupant("alice")
            .is_some());
    }
}

#[tokio::test]
async fn restoring_live_roster_fails_closed_until_receipt_read_recovers() {
    let fixture = AdminRecoveryFixture::new();
    fixture.record_commit();
    fixture.store.fail_reads.store(true, Ordering::SeqCst);
    let actor = fixture.spawn();
    assert!(matches!(
        actor.ask(fixture.restore()).await,
        Err(SendError::HandlerError(
            LiveRosterRestoreError::AdminReceiptUnavailable
        ))
    ));
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("unpublished snapshot")
            .occupancy_revision,
        0
    );
    assert!(!fixture.store.receipts.lock().expect("receipts").is_empty());
    fixture.store.fail_reads.store(false, Ordering::SeqCst);
    actor
        .ask(fixture.restore())
        .await
        .expect("retry after read recovers");
    assert!(!fixture.store.receipts.lock().expect("receipts").is_empty());
    actor
        .ask(AcknowledgeAdminProjection)
        .await
        .expect("publication acknowledgment");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !fixture.store.receipts.lock().expect("receipts").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("published projection releases receipt");
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("published snapshot")
            .admin_mutation_resolution,
        Some(AdminMutationResolution::Committed {
            attempt: fixture.projection.attempt,
            coordinates: fixture.coordinates,
        }),
        "publication cleanup must retain the exact IQ verdict"
    );
}

#[tokio::test]
async fn restoring_live_roster_applies_roles_only_when_hydrated_authorization_matches() {
    for affiliation in [Affiliation::Member, Affiliation::Admin] {
        let mut fixture = AdminRecoveryFixture::new();
        fixture
            .source
            .occupants
            .get_mut("alice")
            .expect("alice")
            .role = Role::Moderator;
        fixture.projection.removed_sessions.clear();
        fixture.projection.changed_roles = vec![(test_full_jid("alice"), Role::Participant)];
        fixture.record_commit();
        fixture
            .authoritative
            .set_affiliation(test_full_jid("alice").to_bare(), affiliation);
        let actor = fixture.spawn();
        actor
            .ask(fixture.restore())
            .await
            .expect("restore exact role delta");
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
        assert_eq!(
            snapshot.room.get_occupant("alice").expect("alice").role,
            if affiliation == Affiliation::Member {
                Role::Participant
            } else {
                Role::Moderator
            }
        );
    }
}
