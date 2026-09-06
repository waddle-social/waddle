use super::super::effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind};
use super::super::effects::direct::DurableDirectEffect;
use super::super::effects::{
    DurableEffect, Effect, ExternalEffect, PlanEffectDependency, PlanSink, PlanSuppressionPolicy,
};
use super::*;
use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
use waddle_xmpp::mam::storage::InMemoryMamStorage;
use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::stream_management::SmSessionRegistry;
use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

fn sender_machine() -> XmppStateMachine {
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut sm = XmppStateMachine::new("example.com", dispatcher);
    sm.transition_to_ready("alice@example.com/web".parse().expect("sender"), false);
    sm
}

fn outgoing(to: jid::Jid) -> Message {
    let mut message = chat_msg(jid("alice@example.com/web"), to, "planned delivery");
    message.id = Some(xmpp_parsers::message::Id("plan-message".into()));
    message
}

fn assert_personal_writes(plan: &[super::super::effects::PlannedEffect], owner: &jid::BareJid) {
    assert!(plan.iter().any(|item| matches!(&item.effect,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect { archive, .. })) if archive == owner)));
    let projection = plan.iter().find(|item| matches!(&item.effect,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox { owner: projected, .. })) if projected == owner)).expect("inbox projection");
    assert!(
        projection
            .dependencies
            .iter()
            .any(|dependency| matches!(dependency,
        PlanEffectDependency::AfterArchive { archive, .. } if archive == owner)),
        "inbox identity must be rewritable after archive resolution"
    );
}

#[tokio::test]
async fn offline_dm_plans_both_archives_and_inboxes_without_writes() {
    let registry = test_registry();
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(InMemoryMamStorage::new()));
    let inbox: Arc<dyn InboxStorage> = Arc::new(poison::PoisonInbox(InMemoryInboxStorage::new()));
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(poison::PoisonPending(
        InMemoryPendingDeliveryStorage::new(waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited),
    ));
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        pending_delivery_storage: Some(&pending),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };
    let message = outgoing(jid("bob@example.com"));
    let plan = plan_message_dispatch(&mut sender_machine(), message.clone(), &deps).await;
    assert_personal_writes(&plan.plan, &"alice@example.com".parse().expect("sender"));
    assert_personal_writes(&plan.plan, &"bob@example.com".parse().expect("recipient"));
    let recipient_archive = plan.plan.iter().find(|item| matches!(&item.effect,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect { archive, .. })) if archive == &"bob@example.com".parse::<jid::BareJid>().expect("recipient"))).expect("recipient archive");
    for owner in ["alice@example.com", "bob@example.com"] {
        assert!(recipient_archive.dependencies.iter().any(|dependency| matches!(dependency,
            PlanEffectDependency::AfterArchive { archive, .. } if archive == &owner.parse::<jid::BareJid>().expect("archive owner"))), "recipient archive payload must track every embedded stanza ID");
    }

    let pending_effect = plan
        .plan
        .iter()
        .find(|item| {
            matches!(
                item.effect,
                Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::QueueOfflineDelivery { .. }
                ))
            )
        })
        .expect("pending delivery");
    assert!(pending_effect.dependencies.iter().any(|dependency| matches!(dependency,
        PlanEffectDependency::AfterArchive { archive, .. } if archive == &"bob@example.com".parse::<jid::BareJid>().expect("recipient"))), "pending payload must track recipient archive identity");
    assert_eq!(plan.sanitized_message.from, message.from);
    assert_eq!(plan.sanitized_message.to, message.to);
    assert_eq!(plan.sanitized_message.bodies, message.bodies);
    assert_eq!(plan.sanitized_message.id, message.id);
    let sender: jid::BareJid = "alice@example.com".parse().expect("sender");
    let sender_archive_id = plan
        .plan
        .iter()
        .find_map(|item| match &item.effect {
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive,
                message,
                ..
            })) if archive == &sender => Some(&message.id),
            _ => None,
        })
        .expect("sender archive");
    let canonical_id =
        waddle_xmpp_core::xep0359::extract_stanza_id_by(&plan.sanitized_message, &sender.into())
            .expect("canonical sender stanza ID");
    assert_eq!(&canonical_id, sender_archive_id);
    assert!(plan.error_reply.is_none());
    assert!(!plan.intents.is_empty());
    assert!(pending
        .list(&"bob@example.com".parse().expect("recipient"))
        .await
        .expect("pending rows")
        .is_empty());
}

#[tokio::test]
async fn live_full_dm_plans_destination_processing_without_sending() {
    let registry = test_registry();
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    register_into_both_tiers(&registry, &user_registry, &target, tx).await;
    let deps = Deps {
        user_registry: Some(&user_registry),
        ..Deps::registry_only(&registry)
    };
    let plan = plan_message_dispatch(
        &mut sender_machine(),
        outgoing(target.clone().into()),
        &deps,
    )
    .await;
    assert!(plan.plan.iter().any(|item| matches!(&item.effect,
        Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, kind: PeerDeliveryKind::PeerStanza, .. })) if jid == &target)));
    let route = plan.plan.iter().find(|item| matches!(&item.effect,
        Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. })) if jid == &target)).expect("peer route");
    assert_eq!(route.suppression, PlanSuppressionPolicy::SenderOnly);
    assert!(
        rx.try_recv().is_err(),
        "planning must not enqueue a live stanza"
    );
}

#[tokio::test]
async fn detached_full_dm_plans_recipient_writes_and_defers_replay_append() {
    let registry = test_registry();
    let target: jid::FullJid = "bob@example.com/phone".parse().expect("recipient");
    let sessions = Arc::new(InMemorySmSessionRegistry::new());
    sessions
        .store_session(detached_dm_session("plan-detached", &target))
        .await
        .expect("session");
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(InMemoryMamStorage::new()));
    let inbox: Arc<dyn InboxStorage> = Arc::new(poison::PoisonInbox(InMemoryInboxStorage::new()));
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sessions),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };
    let plan = plan_message_dispatch(
        &mut sender_machine(),
        outgoing(target.clone().into()),
        &deps,
    )
    .await;
    assert_personal_writes(&plan.plan, &target.to_bare());
    let queued = plan.plan.iter().find(|item| matches!(&item.effect,
        Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })) if resources.contains(&target))).expect("detached append");
    assert!(
        queued
            .dependencies
            .iter()
            .any(|dependency| matches!(dependency,
        PlanEffectDependency::AfterArchive { archive, .. } if archive == &target.to_bare())),
        "replay copy must track recipient archive identity"
    );
    assert!(sessions
        .peek_session("plan-detached")
        .await
        .expect("peek")
        .expect("session")
        .unacked_stanzas
        .is_empty());
}

#[tokio::test]
async fn plan_sink_defers_sender_frames_in_order() {
    let registry = test_registry();
    let sink = PlanSink::new();
    let deps = Deps {
        effects: &sink,
        ..Deps::registry_only(&registry)
    };
    let outcome = interpret(
        vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(result_iq("first"))))),
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(result_iq("second"))))),
        ],
        &deps,
    )
    .await;
    assert!(outcome.frames.is_empty());
    let plan = sink.snapshot();
    assert!(plan
        .iter()
        .all(|effect| effect.suppression == PlanSuppressionPolicy::Always));
    let ids: Vec<_> = plan
        .iter()
        .map(|item| {
            let Effect::External(ExternalEffect::Frame(stanza)) = &item.effect else {
                panic!("sender frame")
            };
            let Stanza::Iq(iq) = stanza.as_ref() else {
                panic!("IQ frame")
            };
            iq.id()
        })
        .collect();
    assert_eq!(ids, ["first", "second"]);
}

#[tokio::test]
async fn retraction_plans_tombstone_and_scrub_without_mutating_archive() {
    use super::super::effects::direct::ExternalDirectEffect;
    use waddle_xmpp_core::xep0359::StanzaId;
    let registry = test_registry();
    let archive: jid::BareJid = "alice@example.com".parse().expect("archive");
    let memory = InMemoryMamStorage::new();
    let row = waddle_xmpp::mam::ArchivedMessage {
        id: "archive-original".into(),
        timestamp: chrono::Utc::now(),
        from: jid("alice@example.com/web"),
        to: jid("bob@example.com"),
        body: Some("original".into()),
        stanza_id: Some(StanzaId::new("wire-original", archive.clone().into())),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    memory.store_message(&archive, &row).await.expect("seed");
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(memory));
    let sink = PlanSink::new();
    let deps = Deps {
        mam_storage: Some(&mam),
        effects: &sink,
        ..Deps::registry_only(&registry)
    };
    let mut retraction = outgoing(jid("bob@example.com"));
    retraction.bodies.clear();
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(
            "wire-original",
        ));
    let planned_id = super::super::direct_retraction::apply_retraction_tombstone(
        &deps,
        &archive,
        "wire-original",
        &retraction,
    )
    .await;
    assert_eq!(
        planned_id,
        Some(StanzaId::new("archive-original", archive.clone().into()))
    );
    let plan = sink.snapshot();
    assert!(plan.iter().any(|item| matches!(&item.effect,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::RetractionTombstone { archive: owner, .. })) if owner == &archive)));
    assert!(plan.iter().any(|item| matches!(
        item.effect,
        Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::ScrubReplayForTombstone { .. }
        ))
    )));
    let stored = mam
        .get_message("archive-original")
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(stored.body, row.body);
    assert!(stored.rich.is_none());
}

mod invariant;
mod poison;
mod sqlite_writes;

mod errors;
