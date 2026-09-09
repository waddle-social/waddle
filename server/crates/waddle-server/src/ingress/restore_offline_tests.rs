use super::*;
use crate::ingress::suppression::external_effect_indices;
use crate::ingress_uow::ReconcileVerdict;
use crate::server::routes::interpret::effects::{
    invite::MucUserRoute, PlanEffectDependency, PlanSuppressionPolicy, RoomExecutionPath,
};
use jid::BareJid;
use waddle_xmpp::ingress::{
    GroupDmHistoryVisibility, GroupDmMembershipGrant, MucInviteLedgerAction,
    MucInviteLedgerMutation, MucInviteMembershipGrant,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

fn bare(value: &str) -> BareJid {
    value.parse().expect("bare JID")
}

fn canonical() -> MessageEnvelope {
    let mut message = Message::new(Some(bare("recipient@example.test").into()));
    message.from = Some("sender@example.test/device".parse().expect("sender"));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "canonical body".into());
    MessageEnvelope::new(message)
}

fn created_at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp")
}

fn row(archived: bool) -> PendingRow {
    let recipient = bare("recipient@example.test");
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: created_at(),
        payload: if archived {
            PendingPayload::Archived(StanzaId::new("recorded", recipient.into()))
        } else {
            PendingPayload::Transient(Box::new(canonical().message().clone()))
        },
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn pending(row: &PendingRow) -> IngressEffectIntent {
    IngressEffectIntent::PendingDelivery {
        mutation: match &row.payload {
            PendingPayload::Archived(archive_stanza_id) => PendingDeliveryMutation::Archived {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
            },
            PendingPayload::Transient(_) => PendingDeliveryMutation::Transient {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
            },
        },
    }
}

fn notification(row: &PendingRow, candidate: bool) -> IngressEffectIntent {
    let PendingPayload::Archived(archive_stanza_id) = &row.payload else {
        panic!("archived fixture")
    };
    IngressEffectIntent::NotificationActivityPreview {
        owner: row.recipient.clone(),
        mutation: if candidate {
            NotificationActivityMutation::NotificationCandidate {
                conversation: row.recipient.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            }
        } else {
            NotificationActivityMutation::OfflineDelivery {
                conversation: row.recipient.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
            }
        },
    }
}

fn plan() -> IngressPlan {
    IngressPlan {
        failure: None,
        rejection: None,
        plan: vec![],
        intents: vec![],
        sanitized_message: canonical().message().clone(),
        error_reply: None,
        room_execution: RoomExecutionPath::None,
    }
}

fn offline(row: PendingRow) -> PlannedEffect {
    PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
        ExternalDeliveryEffect::QueueOfflineDelivery {
            prepared_notification: PreparedOfflineNotification::Suppressed,
            row,
            original_message: Box::new(Message::new(None::<jid::Jid>)),
        },
    )))
    .with_suppression(PlanSuppressionPolicy::SenderOnly)
}

#[test]
fn specialized_recorded_invitations_keep_ordinary_restorer_out() {
    let room = bare("room@conference.example.test");
    let invitee = bare("recipient@example.test");
    let inviter = bare("sender@example.test");
    let group = GroupDmMembershipGrant {
        room: room.clone(),
        invitee: invitee.clone(),
        inviter: inviter.clone(),
        history_visibility: GroupDmHistoryVisibility::Full,
    };
    let specialized = [
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: room.clone(),
                invitee: invitee.clone(),
                inviter: inviter.clone(),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(created_at()),
            },
        },
        IngressEffectIntent::MucInviteMembershipGrant {
            grant: MucInviteMembershipGrant {
                room,
                invitee,
                inviter,
            },
        },
        IngressEffectIntent::GroupDmInviteLedger {
            grant: group.clone(),
        },
        IngressEffectIntent::GroupDmMembershipGrant { grant: group },
    ];
    for specialized in specialized {
        let pending = pending(&row(false));
        let mut plan = plan();
        assert!(!restore_recorded_offline_deliveries(
            &mut plan,
            &[pending.clone(), specialized],
            &[pending],
            &canonical(),
            created_at(),
        ));
        assert!(plan.plan.is_empty());
        assert!(plan.intents.is_empty());
    }
}

#[test]
fn invitation_and_decline_fallbacks_stay_specialized_even_without_ledger() {
    for decline in [false, true] {
        for live in [false, true] {
            let row = row(false);
            let pending = pending(&row);
            let mut message = canonical().message().clone();
            let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
            message.payloads.push(
                minidom::Element::builder("x", ns)
                    .append(
                        minidom::Element::builder(if decline { "decline" } else { "invite" }, ns)
                            .attr(
                                minidom::rxml::xml_ncname!("from").to_owned(),
                                "sender@example.test",
                            )
                            .build(),
                    )
                    .build(),
            );
            let route = MucUserRoute {
                route_identity: None,
                recipient: row.recipient.clone(),
                resources: if live {
                    vec!["recipient@example.test/device".parse().expect("resource")]
                } else {
                    vec![]
                },
                message: Box::new(message),
                fallback: row,
                failure: None,
            };
            let mut plan = plan();
            plan.plan.push(PlannedEffect::new(Effect::External(if live {
                ExternalEffect::RouteToPeer(route)
            } else {
                ExternalEffect::QueueOfflineDelivery(route)
            })));
            for unreceipted in [vec![pending.clone()], vec![]] {
                assert!(!restore_recorded_offline_deliveries(
                    &mut plan,
                    std::slice::from_ref(&pending),
                    &unreceipted,
                    &canonical(),
                    created_at(),
                ));
                assert_eq!(plan.plan.len(), 1);
            }
        }
    }
}

#[test]
fn transient_replacement_restores_identity_payload_time_and_dependencies() {
    let saved = row(false);
    let saved_intent = pending(&saved);
    let mut fresh = row(false);
    fresh.original_receipt_at += chrono::Duration::hours(1);
    fresh.payload = PendingPayload::Transient(Box::new(Message::new(None::<jid::Jid>)));
    let fresh_intent = pending(&fresh);
    let dependency = PlanEffectDependency::AfterArchive {
        archive: saved.recipient.clone(),
        minted: StanzaId::new("dependency", saved.recipient.clone().into()),
    };
    let mut plan = plan();
    plan.intents.push(fresh_intent);
    plan.plan
        .push(offline(fresh).with_dependency(dependency.clone()));
    assert!(restore_recorded_offline_deliveries(
        &mut plan,
        std::slice::from_ref(&saved_intent),
        std::slice::from_ref(&saved_intent),
        &canonical(),
        created_at(),
    ));
    assert_eq!(plan.plan.len(), 1);
    assert_eq!(plan.intents, vec![saved_intent]);
    assert_eq!(plan.plan[0].dependencies, vec![dependency]);
    assert_eq!(plan.plan[0].suppression, PlanSuppressionPolicy::SenderOnly);
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
        row,
        original_message,
        prepared_notification,
    })) = &plan.plan[0].effect
    else {
        panic!("offline effect")
    };
    assert_eq!(row.id, saved.id);
    assert_eq!(row.original_receipt_at, created_at());
    assert_eq!(original_message.as_ref(), canonical().message());
    assert!(
        matches!(&row.payload, PendingPayload::Transient(message) if message.as_ref() == canonical().message())
    );
    assert!(matches!(
        prepared_notification,
        PreparedOfflineNotification::Suppressed
    ));
}

#[test]
fn notification_only_repair_rebuilds_candidate_from_canonical_message() {
    let row = row(true);
    let pending = pending(&row);
    let candidate = notification(&row, true);
    let mut plan = plan();
    plan.plan.push(offline(row.clone()));
    assert!(restore_recorded_offline_deliveries(
        &mut plan,
        &[pending, candidate.clone()],
        std::slice::from_ref(&candidate),
        &canonical(),
        created_at(),
    ));
    assert_eq!(plan.plan.len(), 1);
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
        row: restored,
        prepared_notification: PreparedOfflineNotification::Prepared(candidate),
        ..
    })) = &plan.plan[0].effect
    else {
        panic!("prepared offline candidate")
    };
    assert_eq!(restored.id, row.id);
    assert_eq!(candidate.last_message_body(), Some("canonical body"));
    assert_eq!(
        candidate.sender_jid(),
        canonical().message().from.as_ref().expect("sender")
    );
    assert_eq!(candidate.recipient_bare_jid(), &row.recipient);
}

#[test]
fn ordinary_duplicate_admission_uses_pending_or_correlated_notification_without_reconstruction() {
    let row = row(true);
    let saved_pending = pending(&row);
    let mut plan = plan();
    plan.intents.push(saved_pending.clone());
    plan.plan.push(offline(row.clone()));
    // No restorer runs: this is the fresh SenderOnly effect on an ordinary duplicate.
    for obligation in [
        saved_pending,
        notification(&row, true),
        notification(&row, false),
    ] {
        assert_eq!(
            external_effect_indices(
                &plan,
                &ReconcileVerdict::Consistent,
                &[],
                &[obligation],
                &[]
            ),
            vec![0]
        );
    }
    assert!(
        external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[], &[]).is_empty()
    );
    let mut unrelated = row.clone();
    unrelated.id = PendingRowId::fresh();
    unrelated.payload = PendingPayload::Archived(StanzaId::new(
        "different",
        unrelated.recipient.clone().into(),
    ));
    for obligation in [
        pending(&unrelated),
        notification(&unrelated, true),
        notification(&unrelated, false),
    ] {
        assert!(external_effect_indices(
            &plan,
            &ReconcileVerdict::Consistent,
            &[],
            &[obligation],
            &[]
        )
        .is_empty());
    }
}

#[test]
fn marker_only_repair_does_not_recreate_an_already_receipted_candidate() {
    let row = row(true);
    let marker = notification(&row, false);
    let mut plan = plan();
    assert!(restore_recorded_offline_deliveries(
        &mut plan,
        &[pending(&row), notification(&row, true), marker.clone()],
        std::slice::from_ref(&marker),
        &canonical(),
        created_at(),
    ));
    assert!(matches!(
        &plan.plan[0].effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification: PreparedOfflineNotification::Suppressed,
                ..
            }
        ))
    ));
    assert_eq!(
        external_effect_indices(&plan, &ReconcileVerdict::Consistent, &[], &[marker], &[]),
        vec![0]
    );
}
