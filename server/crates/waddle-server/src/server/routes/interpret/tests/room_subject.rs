use super::*;

// XEP-0045 §8.1 — PersistRoomSubject interpreter arm
// -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum SubjectMutationStoreMode {
    Succeed = 0,
    NotOwner = 1,
    OwnershipUnavailable = 2,
    PersistFailed = 3,
    OwnershipLostDuringPersist = 4,
}

pub(super) struct SubjectMutationStore {
    mode: std::sync::atomic::AtomicU8,
    claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>,
    durable_parent_rows: std::sync::atomic::AtomicUsize,
    fanout_owned: std::sync::atomic::AtomicBool,
    fanout_check_barrier: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl SubjectMutationStore {
    fn new(claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>) -> Self {
        Self {
            mode: std::sync::atomic::AtomicU8::new(SubjectMutationStoreMode::Succeed as u8),
            claim_store,
            durable_parent_rows: std::sync::atomic::AtomicUsize::new(1),
            fanout_owned: std::sync::atomic::AtomicBool::new(true),
            fanout_check_barrier: std::sync::Mutex::new(None),
        }
    }

    fn durable_parent_row_count(&self) -> usize {
        self.durable_parent_rows
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn remove_durable_parent(&self) {
        self.durable_parent_rows
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(feature = "clustering")]
    fn set_fanout_owned(&self, owned: bool) {
        self.fanout_owned
            .store(owned, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(feature = "clustering")]
    fn block_fanout_checks(&self, count: usize) -> Arc<tokio::sync::Barrier> {
        let barrier = Arc::new(tokio::sync::Barrier::new(count + 1));
        *self
            .fanout_check_barrier
            .lock()
            .expect("fanout barrier lock") = Some(Arc::clone(&barrier));
        barrier
    }

    fn set_mode(&self, mode: SubjectMutationStoreMode) {
        self.mode
            .store(mode as u8, std::sync::atomic::Ordering::SeqCst);
    }

    fn mode(&self) -> SubjectMutationStoreMode {
        match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
            value if value == SubjectMutationStoreMode::Succeed as u8 => {
                SubjectMutationStoreMode::Succeed
            }
            value if value == SubjectMutationStoreMode::NotOwner as u8 => {
                SubjectMutationStoreMode::NotOwner
            }
            value if value == SubjectMutationStoreMode::OwnershipUnavailable as u8 => {
                SubjectMutationStoreMode::OwnershipUnavailable
            }
            value if value == SubjectMutationStoreMode::PersistFailed as u8 => {
                SubjectMutationStoreMode::PersistFailed
            }
            value if value == SubjectMutationStoreMode::OwnershipLostDuringPersist as u8 => {
                SubjectMutationStoreMode::OwnershipLostDuringPersist
            }
            value => panic!("invalid subject mutation store mode: {value}"),
        }
    }

    async fn exact_fence_matches(
        &self,
        room_jid: &jid::BareJid,
        fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> Result<bool, waddle_xmpp::XmppError> {
        use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType};

        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity {
            return Ok(false);
        }
        self.claim_store
            .fence(&fence.entity, &fence.owner, fence.epoch)
            .await
            .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))
    }
}

impl waddle_xmpp::muc::MucDurableStore for SubjectMutationStore {
    fn check_fenced_fanout<'a>(
        &'a self,
        _room_jid: &'a jid::BareJid,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let barrier = self
                .fanout_check_barrier
                .lock()
                .expect("fanout barrier lock")
                .clone();
            if let Some(barrier) = barrier {
                barrier.wait().await;
                barrier.wait().await;
            }
            Ok(self.fanout_owned.load(std::sync::atomic::Ordering::SeqCst))
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(None)
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn save_config_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _waddle_id: &'a str,
        _channel_id: &'a str,
        _config: &'a waddle_xmpp::muc::RoomConfig,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                self.durable_parent_rows
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn save_subject_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        let mode = self.mode();
        Box::pin(async move {
            if !self.exact_fence_matches(room_jid, fence).await? {
                return Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                });
            }
            if self.durable_parent_row_count() == 0 {
                return Err(waddle_xmpp::XmppError::DurableRoomStateMissing {
                    entity: fence.entity.clone(),
                });
            }
            match mode {
                SubjectMutationStoreMode::PersistFailed => Err(waddle_xmpp::XmppError::internal(
                    "subject persist failed in interpreter test",
                )),
                SubjectMutationStoreMode::OwnershipLostDuringPersist => {
                    use waddle_xmpp::ownership::ClaimStore;

                    self.claim_store
                        .release_exact(&fence.entity, &fence.owner, fence.epoch)
                        .await
                        .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))?;
                    Err(waddle_xmpp::XmppError::OwnershipLost {
                        entity: fence.entity.clone(),
                    })
                }
                _ => Ok(()),
            }
        })
    }

    fn save_affiliation_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn delete_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, bool> {
        let mode = self.mode();
        Box::pin(async move {
            match mode {
                SubjectMutationStoreMode::Succeed
                | SubjectMutationStoreMode::PersistFailed
                | SubjectMutationStoreMode::OwnershipLostDuringPersist => {
                    self.exact_fence_matches(room_jid, fence).await
                }
                SubjectMutationStoreMode::NotOwner => Ok(false),
                SubjectMutationStoreMode::OwnershipUnavailable => Err(
                    waddle_xmpp::XmppError::internal("ownership probe failed in interpreter test"),
                ),
            }
        })
    }
}

pub(super) async fn spawn_subject_mutation_test_room() -> (
    kameo::actor::ActorRef<RoomRegistryActor>,
    kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    jid::BareJid,
    Arc<waddle_xmpp::ownership::InProcessClaimStore>,
    waddle_xmpp::muc::RoomClaimFenceContext,
    Arc<SubjectMutationStore>,
) {
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, WireClusteringClaims};
    use waddle_xmpp::ownership::{InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::new(b"subject-fail-closed-test-secret-32b".to_vec())
            .expect("test secret meets length floor"),
    ));
    let claim_store = Arc::new(InProcessClaimStore::new());
    let store = Arc::new(SubjectMutationStore::new(claim_store.clone()));
    room_registry
        .ask(WireClusteringClaims {
            claim_store: claim_store.clone(),
            node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                "subject-test-node",
                "subject-test-epoch",
            )),
            durable_store: Some(store.clone()),
            rollout_backoff: None,
        })
        .await
        .expect("wire subject mutation test store");
    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let room_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-subject".to_string(),
            channel_id: "c-subject".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create subject mutation test room");
    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: "alice@example.com/web".parse().expect("sender full jid"),
        })
        .await
        .expect("subject mutation room snapshot");
    let claim_fence = snapshot
        .claim_fence
        .expect("durable subject mutation room has an exact fence");
    (
        room_registry,
        room_actor,
        room_jid,
        claim_store,
        claim_fence,
        store,
    )
}

pub(super) fn subject_change_message(
    room: &jid::BareJid,
    sender: &jid::FullJid,
    text: &str,
) -> Message {
    let mut message = Message::new(Some(jid::Jid::from(room.clone())));
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = XmppMessageType::Groupchat;
    message
        .subjects
        .insert(xmpp_parsers::message::Lang::new(), text.to_string());
    message
}

pub(super) fn persist_subject_event(
    room: &jid::BareJid,
    sender: &jid::FullJid,
    text: &str,
    claim_fence: waddle_xmpp::muc::RoomClaimFenceContext,
) -> OutboundEvent {
    use chrono::TimeZone;

    OutboundEvent::PersistRoomSubject {
        room: room.clone(),
        claim_fence: Some(claim_fence),
        texts: waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), text.to_string())]),
        setter: sender.to_bare(),
        sender: sender.clone(),
        message: Box::new(subject_change_message(room, sender, text)),
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    }
}

#[tokio::test]
async fn xep_0045_persist_room_subject_writes_state_via_room_actor() {
    // Per-arm coverage for `OutboundEvent::PersistRoomSubject`
    // (Copilot review, PR #319). Drives the event through
    // `interpret(...)` against a real `RoomRegistryActor` and a
    // pre-created room actor, then queries the room snapshot to
    // confirm the actor wrote `MucRoom.subject` to a `SubjectState`
    // matching the event payload.
    use chrono::TimeZone;
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let registry = ConnectionRegistry::new();
    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::new(b"persist-subject-arm-test-secret-32b".to_vec())
            .expect("test secret meets length floor"),
    ));
    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let _room_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");

    let deps = Deps {
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: Some(&room_registry),
        web_socket_state: None,
        authenticated_principal: None,
        local_domain: "example.com",
        blocking_storage: None,
        message_dispatcher: None,
        pending_delivery_storage: None,
        ordered_relay_origin: None,
        sfu: None,
    };

    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

    let events = vec![OutboundEvent::PersistRoomSubject {
        room: room_jid.clone(),
        claim_fence: None,
        texts: texts.clone(),
        setter: setter.clone(),
        sender: sender.clone(),
        message: Box::new(subject_change_message(
            &room_jid,
            &sender,
            "Default subject",
        )),
        setter_nick: "alice-nick".to_string(),
        set_at,
    }];
    let _outcome = interpret(events, &deps).await;

    // Verify the room actor wrote `SubjectState` matching the event payload.
    let actor = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room actor present");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let stored = snapshot
        .room
        .subject
        .expect("PersistRoomSubject must land a SubjectState");
    assert_eq!(stored.texts, texts);
    assert_eq!(stored.setter, setter);
    assert_eq!(stored.setter_nick, "alice-nick");
    assert_eq!(stored.set_at, set_at);
}

#[tokio::test]
async fn xep_0045_persist_room_subject_with_no_registry_bounces_and_halts_batch() {
    // A subject effect cannot safely complete without its room registry.
    // Reject it and suppress all later effects from the same dispatch batch.
    use chrono::TimeZone;

    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);

    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let texts =
        waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), "ignored".to_string())]);
    let events = vec![
        OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            claim_fence: None,
            texts,
            setter,
            sender: sender.clone(),
            message: Box::new(subject_change_message(&room_jid, &sender, "ignored")),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        },
        OutboundEvent::CloseTransport,
    ];
    let outcome = interpret(events, &deps).await;
    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
}

#[tokio::test]
async fn xep_0045_stale_subject_effect_cannot_mutate_same_jid_successor() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DemoteRoomIfExactActor};
    use waddle_xmpp::ownership::{ClaimStore, ExactReleaseOutcome};

    let (room_registry, original_actor, room_jid, claim_store, original_fence, _store) =
        spawn_subject_mutation_test_room().await;

    assert_eq!(
        claim_store
            .release_exact(
                &original_fence.entity,
                &original_fence.owner,
                original_fence.epoch,
            )
            .await
            .expect("release original exact claim"),
        ExactReleaseOutcome::Released,
    );
    assert!(room_registry
        .ask(DemoteRoomIfExactActor {
            room_jid: room_jid.clone(),
            actor_ref: original_actor,
        })
        .await
        .expect("remove original actor"));
    let successor_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-subject-successor".to_string(),
            channel_id: "c-subject-successor".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create same-JID successor");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let successor_snapshot = successor_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender.clone(),
        })
        .await
        .expect("successor chain snapshot");
    assert_ne!(
        successor_snapshot.claim_fence.as_ref(),
        Some(&original_fence),
        "the replacement must have a distinct exact authority"
    );

    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let outcome = interpret(
        vec![
            persist_subject_event(
                &room_jid,
                &sender,
                "stale predecessor subject",
                original_fence,
            ),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
    let current_actor = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("lookup successor")
        .expect("successor remains registered");
    assert_eq!(current_actor.id(), successor_actor.id());
    assert!(
        successor_actor
            .ask(GetSnapshot)
            .await
            .expect("successor state snapshot")
            .room
            .subject
            .is_none(),
        "the predecessor's subject must never reach the successor"
    );
}

#[tokio::test]
async fn xep_0045_new_clustered_room_subject_fails_closed_without_a_durable_parent() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;

    // #1352 owns atomic complete room-plus-Owner initialization. Until that
    // prerequisite lands, the UPDATE-only subject path must fail before
    // acknowledging or applying state that cannot survive actor replacement.
    let (room_registry, room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.remove_durable_parent();
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "first subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
    assert_eq!(
        store.durable_parent_row_count(),
        0,
        "the rejected subject must not create a partial durable parent"
    );
    let snapshot = room_actor.ask(GetSnapshot).await.expect("room snapshot");
    assert!(
        snapshot.room.subject.is_none(),
        "the rejected subject must not be applied in actor memory"
    );
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep_0045_concurrent_non_serving_fanout_preserves_successor_and_suppresses_effects() {
    use waddle_xmpp::mam::{MamArchiveKind, MamQuery};
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DemoteRoomIfExactActor};
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::ownership::InProcessClaimStore;
    use waddle_xmpp::{Affiliation, Role};

    let claim_store = Arc::new(InProcessClaimStore::new());
    let store = Arc::new(SubjectMutationStore::new(claim_store));
    store.set_fanout_owned(false);
    let fanout_barrier = store.block_fanout_checks(2);
    let clustering = crate::clustering::ClusteringHandles {
        muc_durable_store: Some(Arc::clone(&store) as Arc<dyn waddle_xmpp::muc::MucDurableStore>),
        ..Default::default()
    };
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            clustering,
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
    let room_registry = &state.deps.protocol.room_registry;
    let room_jid: jid::BareJid = "rotated@muc.example.com".parse().expect("room JID");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender JID");
    let actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-rotated".to_string(),
            channel_id: "c-rotated".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");
    actor
        .ask(Join {
            nick: "alice".to_string(),
            real_jid: sender.clone(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join room");
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(
        &state.deps.protocol.connection_registry,
        &state.deps.protocol.user_registry,
        &sender,
        sender_tx,
    )
    .await;

    let deps = Deps {
        connection_registry: &state.deps.protocol.connection_registry,
        user_registry: Some(&state.deps.protocol.user_registry),
        sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
        mam_storage: Some(&state.deps.protocol.mam_storage),
        inbox_storage: Some(&state.deps.protocol.inbox_storage),
        extension_manager: Some(&state.deps.protocol.extension_manager),
        room_registry: Some(room_registry),
        web_socket_state: Some(state.as_ref()),
        authenticated_principal: None,
        local_domain: state.deps.auth_state.xmpp_domain.as_str(),
        blocking_storage: Some(&state.deps.protocol.blocking_storage),
        message_dispatcher: Some(&state.deps.protocol.dispatcher),
        pending_delivery_storage: Some(&state.deps.protocol.pending_delivery_storage),
        ordered_relay_origin: None,
        sfu: None,
    };
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(sender));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "must not fan out".to_string(),
    );

    let first_dispatch = interpret(
        vec![OutboundEvent::DispatchToRoom {
            room: room_jid.clone(),
            message: Box::new(message.clone()),
        }],
        &deps,
    );
    let second_dispatch = interpret(
        vec![OutboundEvent::DispatchToRoom {
            room: room_jid.clone(),
            message: Box::new(message),
        }],
        &deps,
    );
    let replace_while_checking = async {
        // Both dispatches have captured the old actor/snapshot and reached
        // the legacy cache check before the replacement is published.
        fanout_barrier.wait().await;
        assert!(room_registry
            .ask(DemoteRoomIfExactActor {
                room_jid: room_jid.clone(),
                actor_ref: actor,
            })
            .await
            .expect("demote original actor"));
        let successor = room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w-successor".to_string(),
                channel_id: "c-successor".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create same-JID successor");
        fanout_barrier.wait().await;
        successor
    };
    let (first_outcome, second_outcome, successor) =
        tokio::join!(first_dispatch, second_dispatch, replace_while_checking);

    for outcome in [&first_outcome, &second_outcome] {
        assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
        assert!(outcome.frames[0].contains("resource-constraint"));
    }
    let archive = state
        .deps
        .protocol
        .mam_storage
        .query_messages(&room_jid, MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("room archive query");
    assert!(
        archive.messages.is_empty(),
        "the rejected message is not archived"
    );
    assert!(
        matches!(
            sender_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "neither stale dispatch reflects the rejected message to the occupant"
    );
    let registered = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("room lookup")
        .expect("same-JID successor survives exact demotion");
    assert_eq!(registered.id(), successor.id());
}

#[tokio::test]
async fn xep_0045_subject_not_owner_bounces_demotes_exact_actor_and_halts_batch() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::NotOwner);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "rejected subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(
        !outcome.close,
        "the event following rejected subject persistence must not be interpreted"
    );
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after demotion")
            .is_none(),
        "the exact actor that proved ownership loss must be demoted"
    );
}

#[tokio::test]
async fn xep_0045_subject_ownership_loss_during_persist_bounces_and_demotes() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::OwnershipLostDuringPersist);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let mut subject_event =
        persist_subject_event(&room_jid, &sender, "stale in-memory subject", claim_fence);
    let OutboundEvent::PersistRoomSubject { message, .. } = &mut subject_event else {
        unreachable!("helper always builds a subject event")
    };
    message.payloads.push(waddle_xmpp::xep::build_hint_element(
        waddle_xmpp::xep::Hint::NoStore,
    ));

    let outcome = interpret(vec![subject_event, OutboundEvent::CloseTransport], &deps).await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "the post-subject batch must be suppressed");
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after write-adjacent loss")
            .is_none(),
        "the actor whose write-adjacent fence failed must be demoted"
    );
}

#[tokio::test]
async fn xep_0045_subject_ownership_unavailable_bounces_without_demotion_and_halts_batch() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::OwnershipUnavailable);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "ambiguous subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(
        !outcome.close,
        "the event following ambiguous subject persistence must not be interpreted"
    );
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after ambiguous mutation")
            .is_some(),
        "an ambiguous ownership probe must not demote the actor"
    );
}

#[tokio::test]
async fn xep_0045_subject_persist_failure_bounces_before_apply_and_halts_batch() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;

    let (room_registry, room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::PersistFailed);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "rejected subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
    let snapshot = room_actor.ask(GetSnapshot).await.expect("room snapshot");
    assert!(
        snapshot.room.subject.is_none(),
        "failed durable persistence must leave in-memory subject unchanged"
    );
}
