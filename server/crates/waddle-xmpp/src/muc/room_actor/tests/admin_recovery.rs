use super::*;
use crate::muc::durable::{
    AdminMutationId, AdminMutationReceipt, MucDurableFuture, MucDurableStore,
};
use crate::muc::{RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct AdminReceiptStore {
    receipts: Mutex<HashMap<AdminMutationId, AdminMutationReceipt>>,
    fail_reads: AtomicBool,
    /// When set, the next durable commit succeeds at these coordinates and
    /// records the batch's receipt; otherwise commits are refused.
    commit_next: Mutex<Option<RoomCommittedCoordinates>>,
    commit_reply_lost: AtomicBool,
    next_restore_coordinates: Mutex<Option<RoomCommittedCoordinates>>,
    restore_effects: Mutex<Vec<crate::muc::RoomMutationEffects>>,
    fail_restore_commits: AtomicBool,
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
        intent: crate::muc::RoomDurableMutation,
        effects: crate::muc::RoomMutationEffects,
    ) -> crate::muc::RoomCommitFuture<'a> {
        let is_restore = matches!(&intent, crate::muc::RoomDurableMutation::AffiliationBatch(entries) if entries.is_empty());
        let coordinates = if is_restore {
            if self.fail_restore_commits.load(Ordering::SeqCst) {
                return Box::pin(async { Err(crate::muc::RoomCommitError::OwnershipUnavailable) });
            }
            let mut next = self
                .next_restore_coordinates
                .lock()
                .expect("restore coordinates");
            let coordinates = next.expect("fixture configures restore coordinates");
            *next = Some(RoomCommittedCoordinates {
                revision: coordinates.revision.next().expect("restore revision"),
                ..coordinates
            });
            self.restore_effects
                .lock()
                .expect("restore effects")
                .push(effects.clone());
            coordinates
        } else {
            let Some(coordinates) = self.commit_next.lock().expect("commit").take() else {
                return Box::pin(async { Err(crate::muc::RoomCommitError::NotOwner) });
            };
            coordinates
        };
        if let Some(attempt) = effects.admin_mutation_id() {
            self.receipts.lock().expect("receipts").insert(
                attempt,
                AdminMutationReceipt::from_effects(coordinates, &effects),
            );
        }
        if self.commit_reply_lost.load(Ordering::SeqCst) {
            return Box::pin(async { Err(crate::muc::RoomCommitError::CommitOutcomeUnknown) });
        }
        Box::pin(async move {
            Ok(crate::muc::RoomCommitOutcome {
                coordinates,
                reservation: None,
            })
        })
    }

    fn load_admin_mutation_receipt<'a>(
        &'a self,
        _room: &'a BareJid,
        attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, Option<AdminMutationReceipt>> {
        Box::pin(async move {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(crate::XmppError::internal("receipt read unavailable"));
            }
            Ok(self
                .receipts
                .lock()
                .expect("receipts")
                .get(&attempt)
                .cloned())
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
    restore_attempt: AdminMutationId,
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
            restore_attempt: AdminMutationId::generate(),
        }
    }

    fn record_commit(&self) {
        self.store.receipts.lock().expect("receipts").insert(
            self.projection.attempt,
            AdminMutationReceipt {
                coordinates: self.coordinates,
                removed_sessions: self.projection.removed_sessions.clone(),
            },
        );
    }

    fn spawn(&self) -> ActorRef<RoomActor> {
        *self
            .store
            .next_restore_coordinates
            .lock()
            .expect("restore coordinates") = Some(RoomCommittedCoordinates {
            revision: self.coordinates.revision.next().expect("restore revision"),
            ..self.coordinates
        });
        let mut actor = RoomActor::new(self.authoritative.clone(), test_secret());
        actor.durable_coordinates = Some(self.coordinates);
        actor.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::Restored);
        actor.durable_store = Some(self.store.clone());
        actor.durable_claim_fence = Some(test_claim_fence(&self.authoritative.room_jid));
        RoomActor::spawn(actor)
    }

    fn restore(&self) -> RestoreLiveRoster {
        RestoreLiveRoster {
            room: self.source.clone(),
            occupancy_revision: 7,
            live_roster_restore_attempt: self.restore_attempt,
            departures: Default::default(),
            pending_admin_projection: Some(self.projection.clone()),
            admin_mutation_resolutions: Vec::new(),
            pending_affiliation_departures: Default::default(),
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
        snapshot.admin_mutation_resolution(fixture.projection.attempt),
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
        snapshot.admin_mutation_resolution(fixture.projection.attempt),
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
    fixture.store.receipts.lock().expect("receipts").insert(
        AdminMutationId::generate(),
        AdminMutationReceipt {
            coordinates: fixture.coordinates,
            removed_sessions: Vec::new(),
        },
    );
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
        fixture.store.receipts.lock().expect("receipts").insert(
            fixture.projection.attempt,
            AdminMutationReceipt {
                coordinates: receipt,
                removed_sessions: Vec::new(),
            },
        );
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
                .admin_mutation_resolution(fixture.projection.attempt),
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
            .admin_mutation_resolution(fixture.projection.attempt),
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

fn restore_without_projection(fixture: &AdminRecoveryFixture) -> RestoreLiveRoster {
    RestoreLiveRoster {
        room: fixture.source.clone(),
        occupancy_revision: 7,
        live_roster_restore_attempt: fixture.restore_attempt,
        departures: Default::default(),
        pending_admin_projection: None,
        admin_mutation_resolutions: Vec::new(),
        pending_affiliation_departures: Default::default(),
    }
}

fn add_unaffiliated_occupant(room: &mut MucRoom, nick: &str) {
    room.add_occupant(crate::muc::Occupant {
        real_jid: test_full_jid(nick),
        nick: nick.to_owned(),
        role: Role::Participant,
        affiliation: Affiliation::None,
        is_remote: false,
        home_server: None,
    });
}

/// Subject recovery restores without an admin projection. A ban another
/// owner committed while this actor was sealed still applies to that restore.
#[tokio::test]
async fn restoring_live_roster_without_admin_projection_enforces_durable_ban() {
    let fixture = AdminRecoveryFixture::new();
    let actor = fixture.spawn();
    actor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("restore subject-recovery roster");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.get_occupant("alice").is_some());
    assert!(
        snapshot.room.get_occupant("bob").is_none(),
        "a foreign owner's durable ban applies to every restore, not only admin recovery"
    );
    assert!(snapshot.admin_mutation_resolutions.is_empty());
}

/// Membership revocations are left to the paths that owe their presences
/// (group-DM leave reconciliation, members-only config enforcement), which
/// need the session present; only a durable ban is enforced at restore.
#[tokio::test]
async fn restoring_live_roster_without_admin_projection_keeps_revoked_members_for_owning_paths() {
    let mut fixture = AdminRecoveryFixture::new();
    add_unaffiliated_occupant(&mut fixture.source, "carol");
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    let actor = fixture.spawn();
    actor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("restore members-only roster");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.get_occupant("alice").is_some());
    assert!(
        snapshot.room.get_occupant("bob").is_none(),
        "a durable ban is enforced on every restore"
    );
    assert!(
        snapshot.room.get_occupant("carol").is_some(),
        "a non-member stays until the path owing its removal presence runs"
    );
}

/// Verdicts are keyed by attempt: a restore carries the predecessor's
/// retained verdicts and adds its own without displacing them.
#[tokio::test]
async fn restored_admin_verdicts_are_retained_per_attempt() {
    let fixture = AdminRecoveryFixture::new();
    let earlier_attempt = AdminMutationId::generate();
    let earlier = AdminMutationResolution::Committed {
        attempt: earlier_attempt,
        coordinates: fixture.coordinates,
    };
    let actor = fixture.spawn();
    let mut restore = fixture.restore();
    restore.admin_mutation_resolutions = vec![earlier];
    actor
        .ask(restore)
        .await
        .expect("restore with transferred verdicts");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(
        snapshot.admin_mutation_resolution(earlier_attempt),
        Some(earlier)
    );
    assert_eq!(
        snapshot.admin_mutation_resolution(fixture.projection.attempt),
        Some(AdminMutationResolution::NotCommitted {
            attempt: fixture.projection.attempt
        }),
        "the restore's own verdict is added alongside the transferred one"
    );
    assert_eq!(
        snapshot.admin_mutation_resolution(AdminMutationId::generate()),
        None
    );
}

/// The reviewer's case: a later durable batch on the recovered actor must not
/// overwrite an earlier attempt's verdict before that attempt's caller reads it.
#[tokio::test]
async fn later_durable_batch_does_not_displace_an_unconsumed_verdict() {
    let mut fixture = AdminRecoveryFixture::new();
    let alice = test_full_jid("alice");
    fixture
        .authoritative
        .set_affiliation(alice.to_bare(), Affiliation::Owner);
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore rolled-back batch");
    let later_coordinates = RoomCommittedCoordinates {
        revision: fixture.coordinates.revision.next().expect("later revision"),
        ..fixture.coordinates
    };
    *fixture.store.commit_next.lock().expect("commit") = Some(later_coordinates);
    let later_attempt = AdminMutationId::generate();
    actor
        .ask(ApplyAdminItems {
            attempt: later_attempt,
            sender_jid: alice,
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: vec![AdminItem {
                jid: Some(test_full_jid("carol").to_bare()),
                nick: None,
                affiliation: Some(Affiliation::Outcast),
                role: None,
                reason: None,
            }],
        })
        .await
        .expect("later batch commits");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(
        snapshot.admin_mutation_resolution(fixture.projection.attempt),
        Some(AdminMutationResolution::NotCommitted {
            attempt: fixture.projection.attempt
        }),
        "the earlier verdict survives a later batch"
    );
    assert_eq!(
        snapshot.admin_mutation_resolution(later_attempt),
        Some(AdminMutationResolution::Committed {
            attempt: later_attempt,
            coordinates: later_coordinates,
        })
    );
}

/// An unproven projection proves nothing about anyone: a member whose
/// affiliation another flow already revoked stays for that flow's leave.
#[tokio::test]
async fn restoring_live_roster_with_unproven_projection_keeps_revoked_members() {
    let mut fixture = AdminRecoveryFixture::new();
    add_unaffiliated_occupant(&mut fixture.source, "carol");
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore rolled-back batch");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(snapshot.room.get_occupant("alice").is_some());
    assert!(snapshot.room.get_occupant("bob").is_none(), "durable ban");
    assert!(
        snapshot.room.get_occupant("carol").is_some(),
        "a NotCommitted projection must not prune untouched members"
    );
}

/// A proven batch prunes only the JIDs it touched.
#[tokio::test]
async fn restoring_live_roster_with_committed_projection_prunes_only_touched_jids() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.record_commit();
    add_unaffiliated_occupant(&mut fixture.source, "carol");
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("restore proven batch");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(
        snapshot.room.get_occupant("alice").is_none(),
        "intermediate ban"
    );
    assert!(snapshot.room.get_occupant("bob").is_none(), "final ban");
    assert!(
        snapshot.room.get_occupant("carol").is_some(),
        "an untouched non-member is owed its own leave presence elsewhere"
    );
}

#[tokio::test]
async fn ambiguous_admin_batch_keeps_noop_removal_for_pending_leave() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.source.config.members_only = true;
    fixture
        .source
        .set_affiliation(test_full_jid("alice").to_bare(), Affiliation::Owner);
    add_unaffiliated_occupant(&mut fixture.source, "carol");
    fixture.authoritative = fixture.source.clone();
    let predecessor = fixture.spawn();
    let committed = RoomCommittedCoordinates {
        revision: fixture.coordinates.revision.next().expect("next revision"),
        ..fixture.coordinates
    };
    *fixture.store.commit_next.lock().expect("commit") = Some(committed);
    fixture
        .store
        .commit_reply_lost
        .store(true, Ordering::SeqCst);
    let attempt = AdminMutationId::generate();
    let result = predecessor
        .ask(ApplyAdminItems {
            attempt,
            sender_jid: test_full_jid("alice"),
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items: ["bob", "carol"]
                .into_iter()
                .map(|nick| AdminItem {
                    jid: Some(test_full_jid(nick).to_bare()),
                    nick: None,
                    affiliation: Some(Affiliation::None),
                    role: None,
                    reason: None,
                })
                .collect(),
        })
        .await;
    assert!(matches!(
        result,
        Err(kameo::error::SendError::HandlerError(
            AdminApplyError::CommitOutcomeUnknown
        ))
    ));
    let snapshot = predecessor.ask(GetSnapshot).await.expect("sealed snapshot");
    fixture
        .authoritative
        .set_affiliation(test_full_jid("bob").to_bare(), Affiliation::None);
    fixture.coordinates = committed;
    let successor = fixture.spawn();
    successor
        .ask(RestoreLiveRoster {
            room: snapshot.room,
            occupancy_revision: snapshot.occupancy_revision,
            live_roster_restore_attempt: snapshot.live_roster_restore_attempt,
            departures: snapshot.departures,
            pending_admin_projection: snapshot.pending_admin_projection,
            admin_mutation_resolutions: snapshot.admin_mutation_resolutions,
            pending_affiliation_departures: snapshot.pending_affiliation_departures,
        })
        .await
        .expect("restore committed batch");
    let restored = successor
        .ask(GetSnapshot)
        .await
        .expect("successor snapshot");
    assert!(
        restored.room.get_occupant("bob").is_none(),
        "the real membership removal is projected"
    );
    assert!(
        restored.room.get_occupant("carol").is_some(),
        "the no-op removal owes no presence; retain its pending leave session"
    );
    assert_eq!(
        restored.admin_mutation_resolution(attempt),
        Some(AdminMutationResolution::Committed {
            attempt,
            coordinates: committed
        })
    );
}

#[tokio::test]
async fn restoring_live_roster_prunes_foreign_membership_revocation() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    fixture
        .authoritative
        .set_affiliation(test_full_jid("alice").to_bare(), Affiliation::None);
    let actor = fixture.spawn();
    actor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("restore subject roster");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert!(
        snapshot.room.get_occupant("alice").is_none(),
        "foreign membership revocation must not restore occupancy"
    );
}

#[tokio::test]
async fn ambiguous_local_affiliation_removal_retains_departure_custody() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.source.config.members_only = true;
    fixture.authoritative = fixture.source.clone();
    let predecessor = fixture.spawn();
    *fixture.store.commit_next.lock().expect("next commit") = Some(fixture.coordinates);
    fixture
        .store
        .commit_reply_lost
        .store(true, Ordering::SeqCst);
    let leaver = test_full_jid("alice");
    assert!(matches!(
        predecessor
            .ask(ChangeAffiliation {
                jid: leaver.to_bare(),
                affiliation: Affiliation::None
            })
            .await,
        Err(kameo::error::SendError::HandlerError(
            AffiliationMutationError::CommitOutcomeUnknown
        ))
    ));
    let before = predecessor.ask(GetSnapshot).await.expect("sealed snapshot");
    assert!(before
        .pending_affiliation_departures
        .contains(&leaver.to_bare()));
    fixture
        .authoritative
        .set_affiliation(leaver.to_bare(), Affiliation::None);
    let successor = fixture.spawn();
    successor
        .ask(RestoreLiveRoster {
            room: before.room,
            occupancy_revision: before.occupancy_revision,
            live_roster_restore_attempt: before.live_roster_restore_attempt,
            departures: before.departures,
            pending_admin_projection: before.pending_admin_projection,
            admin_mutation_resolutions: before.admin_mutation_resolutions,
            pending_affiliation_departures: before.pending_affiliation_departures,
        })
        .await
        .expect("restore local leave");
    let after = successor.ask(GetSnapshot).await.expect("restored snapshot");
    assert!(
        after.room.find_occupant_by_real_jid(&leaver).is_some(),
        "local leave still owes removal presence"
    );
    assert_eq!(
        after.room.get_affiliation(&leaver.to_bare()),
        Affiliation::None
    );
    assert!(
        after.pending_affiliation_departures.is_empty(),
        "projecting the durable affiliation consumes the ambiguity marker"
    );
    fixture
        .store
        .commit_reply_lost
        .store(false, Ordering::SeqCst);
    *fixture.store.commit_next.lock().expect("leave commit") = Some(RoomCommittedCoordinates {
        revision: fixture.coordinates.revision.next().expect("leave revision"),
        ..fixture.coordinates
    });
    assert!(matches!(
        successor
            .ask(LeaveByRealJid {
                sender_jid: leaver,
                cause: crate::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: LeaveOrigin::Fresh,
            })
            .await
            .expect("finish local leave"),
        LeaveDisposition::Left(_)
    ));
}

#[tokio::test]
async fn live_roster_recovery_persists_foreign_removals_for_every_resource() {
    use crate::muc::{AdminPresenceKind, RoomEffect};
    let mut fixture = AdminRecoveryFixture::new();
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    fixture.source.add_occupant_with_affiliation(
        test_full_jid_resource("bob", "phone"),
        "bob".to_owned(),
        Some("example.com"),
        OccupancyWatermark::initial(),
        test_session_generation(),
    );
    fixture
        .source
        .set_affiliation(test_full_jid("carol").to_bare(), Affiliation::Member);
    fixture.source.add_occupant_with_affiliation(
        test_full_jid("carol"),
        "carol".to_owned(),
        Some("example.com"),
        OccupancyWatermark::initial(),
        test_session_generation(),
    );
    let actor = fixture.spawn();
    actor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("durable recovery");
    let effects = fixture.store.restore_effects.lock().expect("effects");
    assert_eq!(effects.len(), 1);
    let [RoomEffect::AdminSelfNotify { updates }, RoomEffect::AdminRemainingBroadcast {
        presence_updates,
        removed_sessions,
        ..
    }] = effects[0].effects()
    else {
        panic!("recovery must own typed self/broadcast effects");
    };
    assert_eq!(
        removed_sessions.len(),
        3,
        "both Bob resources and Carol leave SFU"
    );
    assert!(removed_sessions.contains(&test_full_jid("bob")));
    assert!(removed_sessions.contains(&test_full_jid_resource("bob", "phone")));
    assert!(removed_sessions.contains(&test_full_jid("carol")));
    for (nick, kind, affiliation) in [
        ("bob", AdminPresenceKind::Banned, Affiliation::Outcast),
        (
            "carol",
            AdminPresenceKind::AffiliationRemoved,
            Affiliation::None,
        ),
    ] {
        let self_updates: Vec<_> = updates
            .iter()
            .filter(|update| update.nick.as_str() == nick && update.is_self)
            .collect();
        assert_eq!(self_updates.len(), if nick == "bob" { 2 } else { 1 });
        assert!(self_updates
            .iter()
            .all(|update| update.kind == kind && update.affiliation == affiliation));
        assert!(presence_updates
            .iter()
            .any(|update| update.nick.as_str() == nick
                && update.recipient == test_full_jid("alice")
                && !update.is_self
                && update.kind == kind));
    }
    assert!(
        updates.iter().all(|update| !presence_updates
            .iter()
            .any(|other| other.recipient == update.recipient)),
        "outbox ordinals have disjoint audiences"
    );
}

#[tokio::test]
async fn live_roster_recovery_replays_lost_commit_after_later_membership_regrant() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture
        .store
        .commit_reply_lost
        .store(true, Ordering::SeqCst);
    let first = fixture.spawn();
    assert!(matches!(
        first.ask(restore_without_projection(&fixture)).await,
        Err(SendError::HandlerError(
            LiveRosterRestoreError::RemovalEffectsUnavailable
        ))
    ));
    assert_eq!(
        first
            .ask(GetSnapshot)
            .await
            .expect("unpublished snapshot")
            .occupancy_revision,
        0
    );
    let receipt =
        fixture.store.receipts.lock().expect("receipts")[&fixture.restore_attempt].clone();
    fixture.coordinates = receipt.coordinates;
    fixture
        .authoritative
        .set_affiliation(test_full_jid("bob").to_bare(), Affiliation::Member);
    fixture
        .store
        .commit_reply_lost
        .store(false, Ordering::SeqCst);
    let successor = fixture.spawn();
    successor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("retry exact committed removal");
    let snapshot = successor.ask(GetSnapshot).await.expect("snapshot");
    assert!(
        snapshot.room.get_occupant("bob").is_none(),
        "later regrant cannot resurrect a departed session"
    );
    assert_ne!(
        snapshot.live_roster_restore_attempt, fixture.restore_attempt,
        "a later handoff owns a fresh attempt"
    );
    assert_eq!(
        fixture.store.restore_effects.lock().expect("effects").len(),
        1,
        "lost reply cannot duplicate the outbox"
    );
    assert!(fixture
        .store
        .receipts
        .lock()
        .expect("receipts")
        .contains_key(&fixture.restore_attempt));
    successor
        .ask(AcknowledgeAdminProjection)
        .await
        .expect("publish");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while fixture
            .store
            .receipts
            .lock()
            .expect("receipts")
            .contains_key(&fixture.restore_attempt)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publication cleans proof");
}

#[tokio::test]
async fn live_roster_recovery_chains_new_foreign_revocations_across_ambiguous_retries() {
    let mut fixture = AdminRecoveryFixture::new();
    fixture.source.config.members_only = true;
    fixture.authoritative.config.members_only = true;
    fixture
        .store
        .commit_reply_lost
        .store(true, Ordering::SeqCst);
    let first = fixture.spawn();
    assert!(first
        .ask(restore_without_projection(&fixture))
        .await
        .is_err());
    fixture.coordinates =
        fixture.store.receipts.lock().expect("receipts")[&fixture.restore_attempt].coordinates;
    fixture
        .authoritative
        .set_affiliation(test_full_jid("bob").to_bare(), Affiliation::Member);
    fixture
        .authoritative
        .set_affiliation(test_full_jid("alice").to_bare(), Affiliation::None);
    let second = fixture.spawn();
    assert!(second
        .ask(restore_without_projection(&fixture))
        .await
        .is_err());
    let next_attempt = fixture.restore_attempt.next_restore_attempt();
    let receipt = fixture.store.receipts.lock().expect("receipts")[&next_attempt].clone();
    assert_eq!(receipt.removed_sessions, vec![test_full_jid("alice")]);
    fixture.coordinates = receipt.coordinates;
    fixture
        .authoritative
        .set_affiliation(test_full_jid("alice").to_bare(), Affiliation::Member);
    fixture
        .store
        .commit_reply_lost
        .store(false, Ordering::SeqCst);
    let third = fixture.spawn();
    third
        .ask(restore_without_projection(&fixture))
        .await
        .expect("replay both proven departures");
    assert_eq!(
        third
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .occupant_count(),
        0
    );
    assert_eq!(
        fixture.store.restore_effects.lock().expect("effects").len(),
        2
    );
}

#[tokio::test]
async fn live_roster_recovery_cannot_publish_without_receipt_and_effect_custody() {
    let fixture = AdminRecoveryFixture::new();
    let actor = fixture.spawn();
    fixture.store.fail_reads.store(true, Ordering::SeqCst);
    assert!(matches!(
        actor.ask(restore_without_projection(&fixture)).await,
        Err(SendError::HandlerError(
            LiveRosterRestoreError::RemovalEffectsUnavailable
        ))
    ));
    fixture.store.fail_reads.store(false, Ordering::SeqCst);
    fixture
        .store
        .fail_restore_commits
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        actor.ask(restore_without_projection(&fixture)).await,
        Err(SendError::HandlerError(
            LiveRosterRestoreError::RemovalEffectsUnavailable
        ))
    ));
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .occupancy_revision,
        0
    );
    assert!(fixture
        .store
        .restore_effects
        .lock()
        .expect("effects")
        .is_empty());
    fixture
        .store
        .fail_restore_commits
        .store(false, Ordering::SeqCst);
    actor
        .ask(restore_without_projection(&fixture))
        .await
        .expect("retry after storage recovers");
    assert_eq!(
        fixture.store.restore_effects.lock().expect("effects").len(),
        1
    );
}

#[tokio::test]
async fn live_roster_recovery_does_not_duplicate_proven_local_admin_effects() {
    let fixture = AdminRecoveryFixture::new();
    fixture.record_commit();
    let actor = fixture.spawn();
    actor
        .ask(fixture.restore())
        .await
        .expect("project local admin effects");
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .occupant_count(),
        0
    );
    assert!(
        fixture
            .store
            .restore_effects
            .lock()
            .expect("effects")
            .is_empty(),
        "the proven local batch already owns both removals"
    );
}

#[tokio::test]
async fn live_roster_recovery_rejects_incompatible_or_nonprogressing_receipts() {
    for mismatch in 0..4 {
        let fixture = AdminRecoveryFixture::new();
        let mut receipt = AdminMutationReceipt {
            coordinates: fixture.coordinates,
            removed_sessions: vec![test_full_jid("bob")],
        };
        match mismatch {
            0 => receipt.coordinates.lifecycle = RoomLifecycleId::generate(),
            1 => {
                receipt.coordinates.revision = receipt
                    .coordinates
                    .revision
                    .next()
                    .expect("future revision")
            }
            2 => receipt.removed_sessions.clear(),
            _ => receipt.removed_sessions = vec![test_full_jid("stranger")],
        }
        fixture
            .store
            .receipts
            .lock()
            .expect("receipts")
            .insert(fixture.restore_attempt, receipt);
        let actor = fixture.spawn();
        assert!(matches!(
            actor.ask(restore_without_projection(&fixture)).await,
            Err(SendError::HandlerError(
                LiveRosterRestoreError::RemovalEffectsUnavailable
            ))
        ));
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .occupancy_revision,
            0
        );
    }
}
