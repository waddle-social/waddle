use super::*;
use crate::server::routes::interpret::effects::{
    delivery::PreparedOfflineNotification, room::ExternalRoomEffect,
};
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::*;
use waddle_xmpp::protocol::CarbonKind;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Message, MessageType};

fn bare(s: &str) -> BareJid {
    s.parse().expect("bare JID")
}
fn full(s: &str) -> FullJid {
    s.parse().expect("full JID")
}
fn envelope(body: &str) -> MessageEnvelope {
    let mut message = Message::new(Some(bare("juliet@example.com").into()));
    message.from = Some(full("romeo@example.com/phone").into());
    message.type_ = MessageType::Chat;
    message.bodies.insert(Default::default(), body.into());
    MessageEnvelope::new(message)
}
fn route_intent(resources: &[&str]) -> IngressEffectIntent {
    IngressEffectIntent::RouteDirect {
        recipient: bare("juliet@example.com"),
        fanout: resources.iter().map(|s| full(s)).collect(),
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    }
}
fn progress_for(intent: &IngressEffectIntent) -> RouteProgress {
    let IngressEffectIntent::RouteDirect {
        recipient,
        fanout,
        route_identity,
    } = intent
    else {
        panic!("route")
    };
    RouteProgress {
        receipt: super::super::durable::receipt_key(intent).expect("receipt"),
        recipient: recipient.clone(),
        fanout: fanout.clone(),
        route_identity: route_identity.clone(),
        completed: vec![],
        received_at: None,
    }
}
fn run(
    envelope: &MessageEnvelope,
    recorded: &[IngressEffectIntent],
    pending: &[IngressEffectIntent],
) -> RebuiltRecovery {
    run_at(envelope, recorded, pending, Utc::now())
}
fn run_at(
    envelope: &MessageEnvelope,
    recorded: &[IngressEffectIntent],
    pending: &[IngressEffectIntent],
    created_at: chrono::DateTime<Utc>,
) -> RebuiltRecovery {
    rebuild(RecoveryInput {
        key: MessageKey::new(),
        envelope,
        created_at,
        recorded,
        unreceipted: pending,
        blocked_recipients: &[],
        route_progress: pending
            .iter()
            .filter(|i| matches!(i, IngressEffectIntent::RouteDirect { .. }))
            .map(progress_for)
            .collect(),
    })
    .expect("rebuild")
}
fn archive() -> IngressEffectIntent {
    let recipient = bare("juliet@example.com");
    IngressEffectIntent::ArchiveAuthoritative {
        archive: recipient.clone(),
        stanza_id: StanzaId::new("recipient-archive", recipient.clone().into()),
        by: recipient,
        archived_at: Utc::now(),
        ordinal: None,
    }
}
fn assert_detached(result: &RebuiltRecovery) {
    assert!(matches!(
        &result.decision.external[..],
        [ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached { .. }
        )]
    ));
    assert!(result.unrecoverable.is_empty());
}
#[test]
fn unreceipted_bare_target_route_rebuilds_a_detached_fanout_owned_by_the_arm() {
    let route = route_intent(&["juliet@example.com/phone"]);
    let receipt = progress_for(&route).receipt;
    let result = run(&envelope("hello"), &[route.clone(), archive()], &[route]);
    assert_detached(&result);
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { stanza, .. }) =
        &result.decision.external[0]
    else {
        panic!("detached")
    };
    let waddle_xmpp::Stanza::Message(message) = stanza.as_ref() else {
        panic!("message")
    };
    assert!(message
        .payloads
        .iter()
        .any(|p| p.name() == "stanza-id" && p.attr("id") == Some("recipient-archive")));
    assert_eq!(
        result.decision.external_receipts,
        vec![vec![receipt.clone()]]
    );
    assert_eq!(result.decision.arm_owned_receipts, vec![receipt.clone()]);
    assert_eq!(result.decision.receipts_pending, vec![receipt]);
}
fn full_envelope() -> MessageEnvelope {
    let mut message = envelope("hello").message().clone();
    message.to = Some(full("juliet@example.com/phone").into());
    MessageEnvelope::new(message)
}
#[test]
fn delegated_live_full_target_route_is_deferred() {
    let route = route_intent(&["juliet@example.com/phone"]);
    let result = run(
        &full_envelope(),
        std::slice::from_ref(&route),
        std::slice::from_ref(&route),
    );
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable, vec![IngressEffectKind::RouteDirect]);
}
#[test]
fn full_target_with_recorded_recipient_archive_rebuilds_direct_frame() {
    let route = route_intent(&["juliet@example.com/phone"]);
    assert_detached(&run(
        &full_envelope(),
        &[route.clone(), archive()],
        &[route],
    ));
}
#[test]
fn full_target_multi_resource_route_is_not_delegated() {
    let routes = [route_intent(&[
        "juliet@example.com/phone",
        "juliet@example.com/laptop",
    ])];
    assert_detached(&run(&full_envelope(), &routes, &routes));
}
fn deferred_type(kind: MessageType) {
    let mut message = envelope("hello").message().clone();
    message.type_ = kind;
    if message.type_ == MessageType::Headline {
        waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::Store);
    }
    let routes = [route_intent(&["juliet@example.com/phone"])];
    let result = run(&MessageEnvelope::new(message), &routes, &routes);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable, vec![IngressEffectKind::RouteDirect]);
}
#[test]
fn groupchat_route_direct_is_unrecoverable() {
    deferred_type(MessageType::Groupchat);
}
#[test]
fn headline_route_is_deferred() {
    deferred_type(MessageType::Headline);
}
#[test]
fn route_to_a_recipient_other_than_the_target_is_unrecoverable() {
    let mut message = envelope("hello").message().clone();
    message.to = Some(bare("other@example.com").into());
    let routes = [route_intent(&["juliet@example.com/phone"])];
    let result = run(&MessageEnvelope::new(message), &routes, &routes);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable, vec![IngressEffectKind::RouteDirect]);
}
#[test]
fn receipted_route_is_not_rebuilt() {
    let result = run(
        &envelope("hello"),
        &[route_intent(&["juliet@example.com/phone"])],
        &[],
    );
    assert!(result.decision.external.is_empty());
    assert!(result.unrecoverable.is_empty());
}
#[test]
fn partially_completed_fanout_keeps_only_remaining_resources() {
    let routes = [route_intent(&[
        "juliet@example.com/phone",
        "juliet@example.com/laptop",
    ])];
    let mut progress = progress_for(&routes[0]);
    progress.completed.push(full("juliet@example.com/phone"));
    let result = rebuild(RecoveryInput {
        key: MessageKey::new(),
        envelope: &envelope("hello"),
        created_at: Utc::now(),
        recorded: &routes,
        unreceipted: &routes,
        blocked_recipients: &[],
        route_progress: vec![progress.clone()],
    })
    .expect("rebuild");
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) =
        &result.decision.external[0]
    else {
        panic!("detached")
    };
    assert_eq!(resources, &[full("juliet@example.com/laptop")]);
    assert_eq!(result.decision.arm_owned_receipts, vec![progress.receipt]);
}
#[test]
fn pending_delivery_with_candidate_rebuilds_offline_row() {
    let recipient = bare("juliet@example.com");
    let id = StanzaId::new("offline", recipient.clone().into());
    let intents = vec![
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
                archive_stanza_id: id.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: id.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient,
                archive_stanza_id: id,
            },
        },
    ];
    let result = run(&envelope("hello"), &intents, &intents);
    assert!(matches!(
        &result.decision.external[..],
        [ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification: PreparedOfflineNotification::Prepared(_),
                ..
            }
        )]
    ));
    assert_eq!(result.decision.external_receipts[0].len(), 3);
    assert!(result.unrecoverable.is_empty());
}
fn observer() -> IngressEffectIntent {
    IngressEffectIntent::RoomObserver {
        room: bare("room@conference.example.com"),
        requester: bare("romeo@example.com"),
        sender: full("romeo@example.com/phone"),
        plugin: waddle_extensions::PluginId::new("fixture").expect("plugin"),
    }
}
#[test]
fn room_observer_without_observer_envelope_is_unrecoverable() {
    let intents = [observer()];
    let result = run(&envelope("hello"), &intents, &intents);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable, vec![IngressEffectKind::RoomObserver]);
}
#[test]
fn room_observer_with_envelope_rebuilds_observe_effect() {
    let message = envelope("hello").message().clone();
    let envelope = MessageEnvelope::with_room_observer(message.clone(), message);
    let intents = [observer()];
    let result = run(&envelope, &intents, &intents);
    assert!(matches!(
        &result.decision.external[..],
        [ExternalEffect::Room(
            ExternalRoomEffect::ObserveRoomMessage { .. }
        )]
    ));
    assert!(result.unrecoverable.is_empty());
}
#[test]
fn groupchat_notification_recovery_is_delegated_not_executed() {
    let room = bare("room@conference.example.com");
    let recipient = bare("juliet@example.com");
    let id = StanzaId::new("room-id", room.clone().into());
    let intents = vec![
        IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: GroupchatNotificationRecoveryMutation {
                recipient: recipient.clone(),
                room: room.clone(),
                thread_id: None,
                archive_stanza_id: id.clone(),
                sender: full("romeo@example.com/phone").into(),
                is_live_occupant: true,
                room_members_only: true,
                sender_can_broadcast_channel_mention: false,
                created_at_ms: 42,
                action: GroupchatNotificationRecoveryAction::Completed,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient,
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: room,
                archive_stanza_id: id,
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
    ];
    let result = run(&envelope("hello"), &intents, &intents);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.delegated.len(), 1);
    assert!(result.unrecoverable.is_empty());
}
#[test]
fn carbons_are_reported_unrecoverable() {
    let intents = [IngressEffectIntent::Carbons {
        carbon_recipients: vec![full("romeo@example.com/laptop")],
        excluded_source: full("romeo@example.com/phone"),
        kind: CarbonKind::Sent,
    }];
    let result = run(&envelope("hello"), &intents, &intents);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable, vec![IngressEffectKind::Carbons]);
}
fn pin_intents(both: bool) -> Vec<IngressEffectIntent> {
    let sender = bare("romeo@example.com");
    let recipient = bare("juliet@example.com");
    let mut intents = vec![
        IngressEffectIntent::DmPinMutation {
            pair: (recipient.clone(), sender.clone()),
            target_stanza_id: StanzaId::new("target", sender.clone().into()),
            action: DmPinMutationAction::Unpin,
        },
        IngressEffectIntent::RouteDirect {
            recipient: recipient.clone(),
            fanout: vec![full("juliet@example.com/phone")],
            route_identity: EffectMessageIdentity::StanzaId(StanzaId::new(
                "pin-event",
                recipient.into(),
            )),
        },
    ];
    if both {
        intents.push(IngressEffectIntent::RouteDirect {
            recipient: sender.clone(),
            fanout: vec![full("romeo@example.com/phone")],
            route_identity: EffectMessageIdentity::StanzaId(StanzaId::new(
                "pin-event",
                sender.into(),
            )),
        });
    }
    intents
}
fn assert_pin_deferred(both: bool) {
    let intents = pin_intents(both);
    let result = run(&envelope("hello"), &intents, &intents);
    assert!(result.decision.external.is_empty());
    assert_eq!(result.unrecoverable.len(), 2);
    assert!(result
        .unrecoverable
        .contains(&IngressEffectKind::DmPinMutation));
    assert!(result
        .unrecoverable
        .contains(&IngressEffectKind::RouteDirect));
}
#[test]
fn pin_owned_stanza_id_routes_are_never_generically_rebuilt() {
    assert_pin_deferred(true);
}
#[test]
fn unreceipted_dm_pin_mutation_and_its_routes_are_deferred() {
    assert_pin_deferred(false);
}
#[test]
fn receipted_dm_pin_mutation_is_not_replayed_but_its_routes_run() {
    let intents = pin_intents(true);
    let result = run(&envelope("hello"), &intents, &intents[1..2]);
    assert!(matches!(
        &result.decision.external[..],
        [ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { .. }
        )]
    ));
    assert!(result
        .decision
        .external_dependencies
        .iter()
        .flatten()
        .all(|d| !matches!(d, PlanEffectDependency::AfterDmPinMutation { .. })));
    assert_eq!(
        result.decision.arm_owned_receipts,
        vec![progress_for(&intents[1]).receipt]
    );
    assert!(result.unrecoverable.is_empty());
}
#[test]
fn muc_decline_claim_is_bound_to_the_canonical_key_and_receipt_time() {
    let room = bare("room@conference.example.com");
    let mut message = envelope("decline").message().clone();
    message.to = Some(room.clone().into());
    message.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .build(),
            )
            .build(),
    );
    let generation_at = Utc::now() + chrono::Duration::minutes(1);
    let mut intents = [IngressEffectIntent::MucInviteLedger {
        mutation: MucInviteLedgerMutation {
            room,
            invitee: bare("romeo@example.com"),
            inviter: bare("juliet@example.com"),
            action: MucInviteLedgerAction::Claimed,
            recorded_at: Some(generation_at),
        },
    }];
    let created_at = Utc::now() - chrono::Duration::hours(1);
    let envelope = MessageEnvelope::new(message);
    let result = run_at(&envelope, &intents, &intents, created_at);
    let ExternalEffect::InviteLedger(crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation::Claim { message_key, not_after, .. }) = &result.decision.external[0] else { panic!("claim") };
    assert_eq!(*message_key, result.decision.message_key);
    assert!(message_key.is_some());
    assert_eq!(*not_after, Some(generation_at));
    assert_eq!(result.decision.external_receipts[0].len(), 1);
    assert!(result.unrecoverable.is_empty());

    // Older recorded rows carry no observed generation: retain the receipt
    // cutoff while still associating the canonical claim receipt.
    let IngressEffectIntent::MucInviteLedger { mutation } = &mut intents[0] else {
        panic!("decline intent")
    };
    mutation.recorded_at = None;
    let legacy = run_at(&envelope, &intents, &intents, created_at);
    let ExternalEffect::InviteLedger(crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation::Claim { not_after, .. }) = &legacy.decision.external[0] else { panic!("legacy claim") };
    assert_eq!(*not_after, Some(created_at));
    assert_eq!(legacy.decision.external_receipts[0].len(), 1);
    assert!(legacy.unrecoverable.is_empty());
}

#[test]
fn muc_decline_fallback_keeps_the_canonical_receipt_time() {
    let room = bare("room@conference.example.com");
    let inviter = bare("juliet@example.com");
    let mut message = envelope("decline").message().clone();
    message.to = Some(room.clone().into());
    message.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .build(),
            )
            .build(),
    );
    let row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let intents = [
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room,
                invitee: bare("romeo@example.com"),
                inviter: inviter.clone(),
                action: MucInviteLedgerAction::Claimed,
                recorded_at: None,
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: inviter.clone(),
            fanout: vec!["juliet@example.com/phone"
                .parse()
                .expect("inviter resource")],
            route_identity: EffectMessageIdentity::capture_ordinal(3),
        },
        IngressEffectIntent::PendingDelivery {
            mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                recipient: inviter,
                row_id,
            },
        },
    ];
    let created_at = Utc::now() - chrono::Duration::minutes(7);
    let result = run_at(
        &MessageEnvelope::new(message),
        &intents,
        &intents,
        created_at,
    );
    let route = result
        .decision
        .external
        .iter()
        .find_map(|effect| match effect {
            ExternalEffect::RouteToPeer(route) => Some(route),
            _ => None,
        })
        .expect("inviter route with its offline fallback");
    assert_eq!(
        route.fallback.original_receipt_at, created_at,
        "a recovered decline is as old as its canonical acceptance, not as new as the pass"
    );
}

#[test]
fn expired_muc_decline_is_not_rebuilt() {
    let room = bare("room@conference.example.com");
    let mut message = envelope("decline").message().clone();
    message.to = Some(room.clone().into());
    message.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .build(),
            )
            .build(),
    );
    let intents = [IngressEffectIntent::MucInviteLedger {
        mutation: MucInviteLedgerMutation {
            room,
            invitee: bare("romeo@example.com"),
            inviter: bare("juliet@example.com"),
            action: MucInviteLedgerAction::Claimed,
            recorded_at: None,
        },
    }];
    let expired = Utc::now()
        - crate::server::routes::websocket::muc_invites::INVITE_TTL
        - chrono::Duration::hours(1);
    let result = run_at(&MessageEnvelope::new(message), &intents, &intents, expired);
    assert!(
        result.decision.external.is_empty(),
        "an expired decline must not claim a possibly newer invitation"
    );
    assert_eq!(
        result.unrecoverable,
        vec![IngressEffectKind::MucInviteLedger]
    );
}

#[test]
fn blocked_recipient_discards_a_pre_restored_muc_decline_route() {
    let room = bare("room@conference.example.com");
    let inviter = bare("juliet@example.com");
    let mut message = envelope("decline").message().clone();
    message.to = Some(room.clone().into());
    message.payloads.push(
        minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .build(),
            )
            .build(),
    );
    let row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let intents = [
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room,
                invitee: bare("romeo@example.com"),
                inviter: inviter.clone(),
                action: MucInviteLedgerAction::Claimed,
                recorded_at: None,
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: inviter.clone(),
            fanout: vec!["juliet@example.com/phone"
                .parse()
                .expect("inviter resource")],
            route_identity: EffectMessageIdentity::capture_ordinal(3),
        },
        IngressEffectIntent::PendingDelivery {
            mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                recipient: inviter,
                row_id,
            },
        },
    ];
    let message = MessageEnvelope::new(message);
    let pending = &intents[..2];
    let result = rebuild(RecoveryInput {
        key: MessageKey::new(),
        envelope: &message,
        created_at: Utc::now(),
        recorded: &intents,
        unreceipted: pending,
        route_progress: vec![progress_for(&intents[1])],
        blocked_recipients: &[bare("juliet@example.com")],
    })
    .expect("blocked rebuild");
    assert!(matches!(
        &result.decision.external[..],
        [ExternalEffect::InviteLedger(_)]
    ));
    assert_eq!(
        result.discarded_receipts,
        vec![progress_for(&intents[1]).receipt]
    );
    assert!(result.unsupported_receipts.is_empty());
    assert!(result.unrecoverable.is_empty());
}
