//! XEP-0045 §9.1/§9.2: durable bans and membership removals recovered onto
//! an old live roster retain unavailable-presence and media cleanup custody.

use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use kameo::actor::Spawn;
use waddle_xmpp::muc::room_actor::{
    GetSnapshot, RestoreDurableRoomState, RestoreLiveRoster, RoomActor,
};
use waddle_xmpp::muc::{
    AdminMutationId, AdminMutationReceipt, AdminPresenceKind, DurableRoomState, MucDurableFuture,
    MucDurableStore, MucRoom, Occupant, RoomClaimFenceContext, RoomCommitFuture, RoomCommitOutcome,
    RoomCommittedCoordinates, RoomConfig, RoomDurableMutation, RoomEffect, RoomLifecycleId,
    RoomMutationEffects, RoomRevision,
};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
use waddle_xmpp::{Affiliation, Role};

struct RecoveryStore {
    state: DurableRoomState,
    effects: Mutex<Vec<RoomMutationEffects>>,
}

impl MucDurableStore for RecoveryStore {
    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async { Ok(Some(self.state.clone())) })
    }
    fn load_admin_mutation_receipt<'a>(
        &'a self,
        _room: &'a BareJid,
        _attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, Option<AdminMutationReceipt>> {
        Box::pin(async { Ok(None) })
    }
    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
        intent: RoomDurableMutation,
        effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async move {
            assert!(
                matches!(intent, RoomDurableMutation::AffiliationBatch(ref entries) if entries.is_empty()),
                "recovery owns effects without rewriting foreign affiliations"
            );
            self.effects.lock().expect("effects").push(effects);
            let previous = self.state.coordinates.expect("durable coordinates");
            Ok(RoomCommitOutcome {
                coordinates: RoomCommittedCoordinates {
                    revision: previous.revision.next().expect("effect revision"),
                    ..previous
                },
                reservation: None,
            })
        })
    }
}

#[tokio::test]
async fn recovered_bans_and_revocations_own_unavailable_presence_and_sfu_removal() {
    for (affiliation, expected_kind) in [
        (Affiliation::Outcast, AdminPresenceKind::Banned),
        (Affiliation::None, AdminPresenceKind::AffiliationRemoved),
    ] {
        let room_jid: BareJid = "recovery@muc.example.com".parse().expect("room");
        let member: FullJid = "bob@example.com/web".parse().expect("member");
        let watcher: FullJid = "alice@example.com/web".parse().expect("watcher");
        let config = RoomConfig {
            members_only: true,
            ..Default::default()
        };
        let mut source = MucRoom::new(
            room_jid.clone(),
            "waddle".to_owned(),
            "channel".to_owned(),
            config.clone(),
        );
        for (nick, session) in [("alice", &watcher), ("bob", &member)] {
            source.set_affiliation(session.to_bare(), Affiliation::Member);
            source.add_occupant(Occupant {
                nick: nick.to_owned(),
                real_jid: session.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
                is_remote: false,
                home_server: None,
            });
        }
        let mut authoritative = source.clone();
        authoritative.set_affiliation(member.to_bare(), affiliation);
        let store = Arc::new(RecoveryStore {
            state: DurableRoomState {
                coordinates: Some(RoomCommittedCoordinates {
                    lifecycle: RoomLifecycleId::generate(),
                    revision: RoomRevision::initial(),
                }),
                config_coordinates: None,
                waddle_id: "waddle".to_owned(),
                channel_id: "channel".to_owned(),
                config,
                subject: None,
                affiliations: authoritative.get_all_affiliations(),
            },
            effects: Mutex::new(Vec::new()),
        });
        let actor = RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                room_jid.clone(),
                "waddle".to_owned(),
                "channel".to_owned(),
                RoomConfig::default(),
            ),
            OccupantIdSecret::new(vec![3; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("secret"),
        ));
        actor
            .ask(RestoreDurableRoomState {
                store: store.clone(),
                claim_fence: RoomClaimFenceContext::new(
                    Entity::new(EntityType::RoomActor, room_jid.to_string()),
                    NodeIdentity::new("node", "epoch"),
                    ClaimEpoch(1),
                ),
            })
            .await
            .expect("fenced durable restore");
        actor
            .ask(RestoreLiveRoster {
                room: source,
                occupancy_revision: 7,
                live_roster_restore_attempt: AdminMutationId::generate(),
                departures: Default::default(),
                pending_admin_projection: None,
                admin_mutation_resolutions: Vec::new(),
                pending_affiliation_departures: Default::default(),
            })
            .await
            .expect("restore live roster");
        assert!(actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .find_occupant_by_real_jid(&member)
            .is_none());
        let effects = store.effects.lock().expect("effects");
        assert_eq!(effects.len(), 1);
        let [RoomEffect::AdminSelfNotify { updates }, RoomEffect::AdminRemainingBroadcast {
            presence_updates,
            removed_sessions,
            ..
        }] = effects[0].effects()
        else {
            panic!("typed admin effects");
        };
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].kind, expected_kind);
        assert_eq!(updates[0].affiliation, affiliation);
        assert!(updates[0].is_self);
        assert_eq!(updates[0].recipient, member);
        assert_eq!(presence_updates.len(), 1);
        assert_eq!(presence_updates[0].kind, expected_kind);
        assert_eq!(presence_updates[0].recipient, watcher);
        assert!(!presence_updates[0].is_self);
        assert_eq!(removed_sessions, &[member]);
    }
}
