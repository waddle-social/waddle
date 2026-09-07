use super::*;
use chrono::TimeZone;

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
    CommitOutcomeUnknown = 5,
}

pub(super) struct SubjectMutationStore {
    mode: std::sync::atomic::AtomicU8,
    claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>,
    durable_parent_rows: std::sync::atomic::AtomicUsize,
    stored_state: std::sync::Mutex<Option<waddle_xmpp::muc::DurableRoomState>>,
    fanout_owned: std::sync::atomic::AtomicBool,
    fanout_check_barrier: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl SubjectMutationStore {
    pub(super) fn new(claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>) -> Self {
        Self {
            mode: std::sync::atomic::AtomicU8::new(SubjectMutationStoreMode::Succeed as u8),
            claim_store,
            durable_parent_rows: std::sync::atomic::AtomicUsize::new(1),
            stored_state: std::sync::Mutex::new(None),
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

    /// Simulate the durable destruction of the current room era so an
    /// exclusive same-JID create can mint a fresh successor.
    fn clear_stored_room_state(&self) {
        *self.stored_state.lock().expect("stored state lock") = None;
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
            value if value == SubjectMutationStoreMode::CommitOutcomeUnknown as u8 => {
                SubjectMutationStoreMode::CommitOutcomeUnknown
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
        let stored_state = self.stored_state.lock().expect("stored state lock").clone();
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(stored_state)
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        let mode = self.mode();
        let stored_state = &self.stored_state;
        Box::pin(async move {
            if !self
                .exact_fence_matches(room_jid, fence)
                .await
                .map_err(|_| waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable)?
            {
                return Err(waddle_xmpp::muc::RoomCommitError::NotOwner);
            }
            if !matches!(intent, waddle_xmpp::muc::RoomDurableMutation::Create { .. })
                && self.durable_parent_row_count() == 0
            {
                return Err(waddle_xmpp::muc::RoomCommitError::StateMissing);
            }
            match mode {
                SubjectMutationStoreMode::PersistFailed => {
                    Err(waddle_xmpp::muc::RoomCommitError::Database(
                        waddle_xmpp::muc::RoomCommitDatabaseError::sanitized(),
                    ))
                }
                SubjectMutationStoreMode::OwnershipLostDuringPersist => {
                    use waddle_xmpp::ownership::ClaimStore;

                    self.claim_store
                        .release_exact(&fence.entity, &fence.owner, fence.epoch)
                        .await
                        .map_err(|_| waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable)?;
                    Err(waddle_xmpp::muc::RoomCommitError::NotOwner)
                }
                SubjectMutationStoreMode::CommitOutcomeUnknown => {
                    // One-shot ambiguity: the commit itself landed but its
                    // outcome was lost. The store is healthy again by the
                    // time reconciliation re-prepares the room.
                    self.set_mode(SubjectMutationStoreMode::Succeed);
                    persist_subject_store_intent(stored_state, intent);
                    Err(waddle_xmpp::muc::RoomCommitError::CommitOutcomeUnknown)
                }
                _ => {
                    persist_subject_store_intent(stored_state, intent);
                    Ok(waddle_xmpp::muc::RoomCommitOutcome {
                        coordinates: waddle_xmpp::muc::RoomCommittedCoordinates {
                            lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                            revision: waddle_xmpp::muc::RoomRevision::initial(),
                        },
                        reservation: None,
                    })
                }
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
                | SubjectMutationStoreMode::OwnershipLostDuringPersist
                | SubjectMutationStoreMode::CommitOutcomeUnknown => {
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

fn persist_subject_store_intent(
    stored_state: &std::sync::Mutex<Option<waddle_xmpp::muc::DurableRoomState>>,
    intent: waddle_xmpp::muc::RoomDurableMutation,
) {
    match intent {
        waddle_xmpp::muc::RoomDurableMutation::Create {
            waddle_id,
            channel_id,
            config,
            initial_affiliations,
        } => {
            *stored_state.lock().expect("stored state lock") =
                Some(waddle_xmpp::muc::DurableRoomState {
                    coordinates: None,
                    config_coordinates: None,
                    waddle_id: waddle_id.into_string(),
                    channel_id: channel_id.into_string(),
                    config,
                    subject: None,
                    affiliations: initial_affiliations
                        .into_iter()
                        .filter_map(|entry| {
                            entry.affiliation.map(|affiliation| {
                                waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                                    entry.jid,
                                    affiliation,
                                )
                            })
                        })
                        .collect(),
                });
        }
        waddle_xmpp::muc::RoomDurableMutation::Subject(subject) => {
            if let Some(state) = stored_state.lock().expect("stored state lock").as_mut() {
                state.subject = subject;
            }
        }
        _ => {}
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
        ingress_effect_capture: None,
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
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
async fn xep_0045_persist_room_subject_committed_records_subject_mutation_intent() {
    use chrono::TimeZone;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let registry = ConnectionRegistry::new();
    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::new(b"persist-subject-capture-test-secret".to_vec())
            .expect("test secret meets length floor"),
    ));
    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let _room_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-subject-intent".to_string(),
            channel_id: "c-subject-intent".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");
    let capture = crate::ingress::IngressEffectCapture::new();
    let deps = Deps {
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
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
        ingress_effect_capture: Some(capture.clone()),
    };
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("fr".to_string(), "Sujet".to_string()),
    ]);

    let _outcome = interpret(
        vec![OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            claim_fence: None,
            texts: texts.clone(),
            setter: sender.to_bare(),
            sender: sender.clone(),
            message: Box::new(subject_change_message(
                &room_jid,
                &sender,
                "Default subject",
            )),
            setter_nick: "alice-nick".to_string(),
            set_at,
        }],
        &deps,
    )
    .await;

    assert!(capture.snapshot().intents.iter().any(|intent| {
        matches!(
            intent,
            waddle_xmpp::ingress::IngressEffectIntent::RoomSubjectMutation { room, state }
                if room == &room_jid
                    && state.texts == texts
                    && state.setter == sender.to_bare()
                    && state.setter_nick == "alice-nick"
                    && state.set_at == set_at
        )
    }));
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
async fn xep_0045_rejected_room_subject_does_not_record_subject_mutation_intent() {
    let registry = ConnectionRegistry::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = Deps::registry_only(&registry);
    deps.ingress_effect_capture = Some(capture.clone());

    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let outcome = interpret(
        vec![OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            claim_fence: None,
            texts: waddle_xmpp::muc::RoomSubjectTexts::from_iter([(
                String::new(),
                "ignored".to_string(),
            )]),
            setter: sender.to_bare(),
            sender: sender.clone(),
            message: Box::new(subject_change_message(&room_jid, &sender, "ignored")),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        }],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(
        !capture.snapshot().intents.iter().any(|intent| matches!(
            intent,
            waddle_xmpp::ingress::IngressEffectIntent::RoomSubjectMutation { room, .. }
                if room == &room_jid
        )),
        "bounce paths must not capture a committed subject mutation intent"
    );
}

#[tokio::test]
async fn xep_0045_rejected_room_subject_records_error_reply_intent() {
    use xmpp_parsers::stanza_error::StanzaError;

    let registry = ConnectionRegistry::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = Deps::registry_only(&registry);
    deps.ingress_effect_capture = Some(capture.clone());

    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let outcome = interpret(
        vec![OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            claim_fence: None,
            texts: waddle_xmpp::muc::RoomSubjectTexts::from_iter([(
                String::new(),
                "ignored".to_string(),
            )]),
            setter: sender.to_bare(),
            sender: sender.clone(),
            message: Box::new(subject_change_message(&room_jid, &sender, "ignored")),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        }],
        &deps,
    )
    .await;

    let emitted: minidom::Element = outcome.frames[0].parse().expect("emitted bounce");
    let emitted = Message::try_from(emitted).expect("message bounce");
    let emitted_error = emitted
        .payloads
        .iter()
        .find_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .expect("bounce stanza error");
    let expected_error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&emitted_error)
        .expect("server-built stanza error should freeze");
    assert!(capture.snapshot().intents.iter().any(|intent| {
        matches!(
            intent,
            waddle_xmpp::ingress::IngressEffectIntent::ErrorReply { recipient, error }
                if recipient == &sender && error == &expected_error
        )
    }));
}

#[tokio::test]
async fn xep_0045_stale_subject_effect_cannot_mutate_same_jid_successor() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DemoteRoomIfExactActor};
    use waddle_xmpp::ownership::{ClaimStore, ExactReleaseOutcome};

    let (room_registry, original_actor, room_jid, claim_store, original_fence, store) =
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
    store.clear_stored_room_state();
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
        ingress_effect_capture: None,
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
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

#[tokio::test]
async fn xep_0045_subject_commit_outcome_unknown_reconciles_and_allows_broadcast() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;

    let (room_registry, room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::CommitOutcomeUnknown);
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

    assert!(
        outcome.frames.is_empty(),
        "a reconciled subject commit must not bounce the sender: {:?}",
        outcome.frames
    );
    let current_actor = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("lookup after reconciliation")
        .expect("room remains registered");
    assert_ne!(
        current_actor.id(),
        room_actor.id(),
        "the stale actor must be demoted before exact subject reconciliation"
    );
    let snapshot = current_actor.ask(GetSnapshot).await.expect("room snapshot");
    assert_eq!(
        snapshot
            .room
            .subject
            .expect("reconciled successor restored subject")
            .texts,
        waddle_xmpp::muc::RoomSubjectTexts::from_iter([(
            String::new(),
            "ambiguous subject".to_string()
        )])
    );
}
