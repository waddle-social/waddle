//! RFC 0018 §2: pool stores are tripwires; only the authority transaction writes.
use super::*;
use crate::ingress::{
    commit::commit_submission, execute::execute_effects, test_support::IngressFixture,
    ImmediateSink, IngressDecisionClass,
};
#[cfg(not(feature = "clustering"))]
use crate::server::routes::websocket::tests::create_test_websocket_state;
use crate::server::routes::websocket::{
    interpret_loop::build_interpret_deps,
    tests::{create_test_session, register_test_connection},
    ResolvedPrincipal,
};
use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget};
use waddle_xmpp::muc::{
    room_actor::{ChangeAffiliation, GetSnapshot, Join},
    room_registry_actor::CreateRoom,
};

fn machine(sender: &jid::FullJid) -> XmppStateMachine {
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut sm = XmppStateMachine::new("example.com", dispatcher);
    sm.transition_to_ready(sender.clone(), false);
    sm
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    OfflineDm,
    LiveFullDm,
    LocalRoom,
    Subject,
    Invite,
    Pin,
    Retraction,
    #[cfg(feature = "clustering")]
    RemoteRoom,
}

async fn phases_a_b_with_poisoned_stores(scenario: Scenario) {
    // The pool-facing fixture is deliberately distinct from the authority's
    // transaction fixture: poisoning pool writes must not poison legitimate
    // transaction-taking repository writes in Phase B.
    let fixture = IngressFixture::sqlite().await;
    #[cfg(not(feature = "clustering"))]
    let state = create_test_websocket_state().await;
    #[cfg(feature = "clustering")]
    let state = {
        use waddle_xmpp::ownership::{
            ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
        };
        let store = Arc::new(InProcessClaimStore::new());
        let poison: Arc<dyn waddle_xmpp::muc::MucDurableStore> =
            Arc::new(super::poison_muc::PoisonMuc::new(store.clone()));
        let mut clustering = crate::clustering::ClusteringHandles {
            muc_durable_store: Some(poison),
            ..Default::default()
        };
        if matches!(scenario, Scenario::RemoteRoom) {
            store
                .acquire(
                    &Entity::new(EntityType::RoomActor, "authority@muc.example.com"),
                    &NodeIdentity::new("remote", "epoch"),
                )
                .await
                .expect("remote claim");
            clustering.claim_store = Some(store);
            clustering.node_identity =
                Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch")));
        }
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            clustering,
            Arc::new(InMemorySmSessionRegistry::new()),
        )
        .await
    };
    let session = create_test_session(&state, "romeo").await;
    create_test_session(&state, "juliet").await;
    create_test_session(&state, "guest").await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let room: jid::BareJid = "authority@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "authority".into(),
            channel_id: "authority".into(),
            config: waddle_xmpp::muc::RoomConfig {
                members_only: true,
                ..Default::default()
            },
        })
        .await
        .expect("room");
    for (nick, occupant) in [("romeo", &sender), ("juliet", &recipient)] {
        actor
            .ask(ChangeAffiliation {
                jid: occupant.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Admin,
            })
            .await
            .expect("affiliation");
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: occupant.clone(),
                role: waddle_xmpp::Role::Moderator,
                affiliation: waddle_xmpp::Affiliation::Admin,
            })
            .await
            .expect("join");
    }
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(16);
    let (recipient_tx, mut recipient_rx) = tokio::sync::mpsc::channel(16);
    register_test_connection(&state, &sender, sender_tx).await;
    if !matches!(scenario, Scenario::OfflineDm) {
        register_test_connection(&state, &recipient, recipient_tx).await;
    }
    let mut submission = fixture.submission(Some("poisoned-pool-authority"), "body");
    let mut message = submission.plan.sanitized_message.clone();
    match scenario {
        Scenario::OfflineDm => {}
        Scenario::LiveFullDm => message.to = Some(recipient.clone().into()),
        #[cfg(feature = "clustering")]
        Scenario::RemoteRoom => {
            message.to = Some(room.clone().into());
            message.type_ = XmppMessageType::Groupchat;
        }
        Scenario::LocalRoom
        | Scenario::Subject
        | Scenario::Invite
        | Scenario::Pin
        | Scenario::Retraction => {
            message.to = Some(room.clone().into());
            message.type_ = XmppMessageType::Groupchat;
            if matches!(scenario, Scenario::Subject) {
                message.bodies.clear();
                message
                    .subjects
                    .insert(xmpp_parsers::message::Lang::new(), "planned subject".into());
            }
            if matches!(scenario, Scenario::Pin) {
                message.bodies.clear();
                message
                    .payloads
                    .push(waddle_xmpp::xep::xep_waddle_pin::build_pinned_element(
                        &waddle_xmpp_core::xep0359::StanzaId::new(
                            "target-original",
                            room.clone().into(),
                        ),
                    ));
            }
            if matches!(scenario, Scenario::Retraction) {
                message.bodies.clear();
                message
                    .payloads
                    .push(waddle_xmpp::xep::xep0424::build_retract_element(
                        "target-original",
                    ));
            }
            if matches!(scenario, Scenario::Invite) {
                message.bodies.clear();
                message.type_ = XmppMessageType::Normal;
                let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
                message.payloads.push(
                    minidom::Element::builder("x", ns)
                        .append(
                            minidom::Element::builder("invite", ns)
                                .attr(
                                    minidom::rxml::xml_ncname!("to").to_owned(),
                                    "guest@example.com",
                                )
                                .build(),
                        )
                        .build(),
                );
            }
        }
    }
    let memory = InMemoryMamStorage::new();
    if matches!(scenario, Scenario::Retraction) {
        let original = waddle_xmpp::mam::ArchivedMessage {
            id: "target-original".into(),
            body: Some("original content".into()),
            message_type: XmppMessageType::Groupchat,
            ..waddle_xmpp::mam::ArchivedMessage::for_test(
                room.with_resource_str("romeo").expect("nickname").into(),
                room.clone().into(),
            )
        };
        memory
            .store_message(&room, &original)
            .await
            .expect("seed lookup");
        waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("archive")
            .store_message(&room, &original)
            .await
            .expect("seed durable target");
    }
    let initial_archive_rows = fixture.count("mam_messages").await;
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(memory));
    let inbox: Arc<dyn InboxStorage> = Arc::new(poison::PoisonInbox(InMemoryInboxStorage::new()));
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(poison::PoisonPending(
        InMemoryPendingDeliveryStorage::new(waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited),
    ));
    let deps = Deps {
        mam_storage: Some(&mam),
        inbox_storage: Some(&inbox),
        pending_delivery_storage: Some(&pending),
        ..build_interpret_deps(
            &state,
            Some(ResolvedPrincipal::from_authenticated_session(&session)),
        )
    };
    #[cfg(feature = "clustering")]
    let deps = {
        let mut deps = deps;
        if matches!(scenario, Scenario::RemoteRoom) {
            let entity = waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::UserActor,
                sender.to_bare().to_string(),
            );
            deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
                kind: OrderedRelayRouteOriginKind::Entity(entity.clone()),
                sender_entity: entity,
                inbound_sequence: 1,
                handoff: None,
            });
        }
        deps
    };
    let database = state.deps.app_state.db_pool.global();
    super::sqlite_writes::install(database).await;
    submission.target = NormalizedTarget::Bare(message.to.as_ref().expect("target").to_bare());
    submission.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    submission.plan = plan_message_dispatch(&mut machine(&sender), message.clone(), &deps).await;
    assert!(
        submission.plan.rejection.is_none(),
        "{scenario:?}: {:?}",
        submission.plan.rejection
    );
    assert!(
        !submission.plan.intents.is_empty(),
        "exercise actual intents for {scenario:?}"
    );
    if matches!(scenario, Scenario::Invite) {
        assert!(
            submission.plan.plan.iter().any(|item| matches!(
                item.effect,
                Effect::External(ExternalEffect::RoomMembershipMutation(_))
            )),
            "invite must plan a membership write"
        );
    }
    if matches!(scenario, Scenario::Pin | Scenario::Subject) {
        assert!(
            submission.plan.plan.iter().any(|item| matches!(
                item.effect,
                Effect::External(ExternalEffect::Room(
                    super::super::super::effects::room::ExternalRoomEffect::RoomActorMutation { .. }
                ))
            )),
            "room state mutation must be planned"
        );
    }
    assert!(sender_rx.try_recv().is_err());
    assert!(recipient_rx.try_recv().is_err());
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("mam_messages").await, initial_archive_rows);
    super::sqlite_writes::assert_untouched(database).await;
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("commit plan");
    assert_eq!(
        decision.class,
        IngressDecisionClass::Accepted,
        "{scenario:?}"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert!(fixture.count("ingress_effect_intents").await > 0);
    assert!(sender_rx.try_recv().is_err(), "Phase B sent a sender frame");
    assert!(
        recipient_rx.try_recv().is_err(),
        "Phase B reached recipient pipeline"
    );
    super::sqlite_writes::assert_untouched(database).await;
    match scenario {
        Scenario::OfflineDm => assert_eq!(fixture.count("mam_messages").await, 2),
        Scenario::LiveFullDm | Scenario::LocalRoom => {
            assert_eq!(fixture.count("mam_messages").await, 1)
        }
        #[cfg(feature = "clustering")]
        Scenario::RemoteRoom => {
            assert_eq!(fixture.count("mam_messages").await, 0);
            assert!(submission.plan.intents.iter().any(|intent| matches!(
                intent,
                waddle_xmpp::ingress::IngressEffectIntent::DispatchToRoomRemote { .. }
            )));
        }
        Scenario::Pin => assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetPinList)
            .await
            .expect("pins")
            .is_empty()),
        Scenario::Retraction => {
            assert_eq!(
                mam.get_message("target-original")
                    .await
                    .expect("lookup")
                    .expect("original")
                    .body
                    .as_deref(),
                Some("original content")
            );
            let archive = waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
                .await
                .expect("archive");
            assert!(archive
                .get_message("target-original")
                .await
                .expect("lookup")
                .expect("tombstone")
                .body
                .is_none());
        }
        Scenario::Subject => assert!(actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .subject
            .is_none()),
        Scenario::Invite => assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .get_affiliation(&"guest@example.com".parse().expect("guest")),
            waddle_xmpp::Affiliation::None
        ),
    }
    if matches!(scenario, Scenario::LiveFullDm) {
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            std::time::Duration::from_secs(5),
        )
        .await;
        assert!(!report.outcomes.is_empty());
        let delivered = recipient_rx.try_recv().expect("post-commit peer delivery");
        assert_eq!(
            delivered.kind,
            waddle_xmpp::registry::DeliveryKind::PeerStanza
        );
        let Stanza::Message(delivered) = delivered.stanza else {
            panic!("message")
        };
        assert_eq!(delivered.to, Some(recipient.into()));
        assert_eq!(delivered.bodies, message.bodies);
        assert!(waddle_xmpp_core::xep0359::extract_stanza_id_by(
            &delivered,
            &sender.to_bare().into()
        )
        .is_some());
        // The destination still owns its recipient archive pass; no eager
        // recipient archive has been inserted by planning/committing/enqueueing.
        assert_eq!(fixture.count("mam_messages").await, 1);
    }
    fixture.close().await;
}

/// RFC 0018 §2; XEP-0313 §3: both offline archives use the authority transaction.
#[tokio::test]
async fn ingress_phase_ab_offline_dm_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::OfflineDm).await;
}

/// RFC 0018 §2: the live full-JID recipient pass is only enqueued after commit.
#[tokio::test]
async fn ingress_phase_ab_live_full_jid_delivery_starts_after_commit() {
    phases_a_b_with_poisoned_stores(Scenario::LiveFullDm).await;
}

/// RFC 0018 §2; XEP-0045 §7.4: room archive/fanout are planned before commit.
#[tokio::test]
async fn ingress_phase_ab_local_muc_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::LocalRoom).await;
}

/// RFC 0018 §2; XEP-0045 §8.1: subject mutation remains post-commit.
#[tokio::test]
async fn ingress_phase_ab_subject_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::Subject).await;
}

/// RFC 0018 §2; XEP-0045 §7.8: membership and invite ledger remain post-commit.
#[tokio::test]
async fn ingress_phase_ab_invite_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::Invite).await;
}

/// RFC 0018 §2: pin actor mutations and system frames stay outside Phase B.
#[tokio::test]
async fn ingress_phase_ab_pin_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::Pin).await;
}

/// RFC 0018 §2; XEP-0424 §3: tombstones use the transaction-taking repository.
#[tokio::test]
async fn ingress_phase_ab_retraction_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::Retraction).await;
}

/// RFC 0018 §2: remote MUC responsibility commits before any relay ask.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_phase_ab_remote_muc_bypasses_poisoned_pool_stores() {
    phases_a_b_with_poisoned_stores(Scenario::RemoteRoom).await;
}
