use super::*;
use kameo::error::SendError;
use waddle_xmpp::muc::room_actor::GetSnapshot;
use waddle_xmpp::muc::room_actor::SetSubjectError;
use waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor;
use waddle_xmpp::muc::room_registry_actor::GetOrCreateRoom;
use waddle_xmpp::muc::{RoomClaimFenceContext, RoomSubjectTexts};
use waddle_xmpp::ownership::{Entity, EntityType};

pub(super) enum PersistRoomSubjectEventOutcome {
    Committed,
    BounceAndHalt(Box<Message>),
}

pub(super) struct PersistRoomSubjectRequest {
    pub room: BareJid,
    pub claim_fence: Option<RoomClaimFenceContext>,
    pub texts: RoomSubjectTexts,
    pub setter: BareJid,
    pub sender: FullJid,
    pub message: Box<Message>,
    pub setter_nick: String,
    pub set_at: chrono::DateTime<chrono::Utc>,
}

pub(super) async fn persist_room_subject_event(
    deps: &Deps<'_>,
    request: PersistRoomSubjectRequest,
) -> PersistRoomSubjectEventOutcome {
    let PersistRoomSubjectRequest {
        room,
        claim_fence,
        texts,
        setter,
        sender,
        message,
        setter_nick,
        set_at,
    } = request;
    let Some(room_registry) = deps.room_registry else {
        warn!(
            room = %room,
            "PersistRoomSubject: no room_registry in Deps; rejecting subject change"
        );
        return retryable_subject_bounce(
            &message,
            &room,
            &sender,
            "This room is temporarily unavailable; please retry the subject change.",
        );
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            debug!(
                room = %room,
                "PersistRoomSubject: room not registered; rejecting subject change"
            );
            return retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry the subject change.",
            );
        }
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "PersistRoomSubject: room registry lookup failed; rejecting subject change"
            );
            return retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry the subject change.",
            );
        }
    };

    // The event was emitted from a frozen snapshot. A same-JID successor may
    // have replaced that actor before this interpreter arm runs, so bind the
    // mutation to the exact snapshot fence instead of trusting a fresh room
    // lookup alone. Both `None` is the single-node/no-durable-store shape;
    // every mixed or differing pair fails closed without demoting the healthy
    // actor currently registered under the room JID.
    let actor_snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "PersistRoomSubject: exact actor snapshot failed; rejecting subject change"
            );
            return retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry the subject change.",
            );
        }
    };
    let expected_entity = Entity::new(EntityType::RoomActor, room.to_string());
    let exact_actor_matches = match (claim_fence.as_ref(), actor_snapshot.claim_fence.as_ref()) {
        (None, None) => true,
        (Some(event_fence), Some(actor_fence)) => {
            event_fence.entity == expected_entity && event_fence == actor_fence
        }
        _ => false,
    };
    if !exact_actor_matches {
        warn!(
            room = %room,
            event_fence = ?claim_fence,
            actor_fence = ?actor_snapshot.claim_fence,
            "PersistRoomSubject: event authority does not match the current actor; rejecting subject change"
        );
        return retryable_subject_bounce(
            &message,
            &room,
            &sender,
            "This room is temporarily unavailable; please retry.",
        );
    }
    if deps.effects.is_planning() {
        super::effects::room::external(
            deps,
            super::effects::room::ExternalRoomEffect::RoomActorMutation {
                room,
                mutation: super::effects::room::RoomActorMutation::SetSubject {
                    claim_fence,
                    subject: waddle_xmpp::muc::SubjectState {
                        texts,
                        setter,
                        setter_nick,
                        set_at,
                    },
                },
            },
            super::effects::PlanSuppressionPolicy::Always,
        );
        return PersistRoomSubjectEventOutcome::Committed;
    }
    let durable_recovery_snapshot = room_actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
        .ok();

    match room_actor
        .ask(SetSubject {
            texts: texts.clone(),
            setter: setter.clone(),
            setter_nick: setter_nick.clone(),
            set_at,
        })
        .await
    {
        Ok(()) => PersistRoomSubjectEventOutcome::Committed,
        Err(SendError::HandlerError(SetSubjectError::CommitOutcomeUnknown)) => {
            warn!(
                room = %room,
                setter = %setter,
                "PersistRoomSubject: exact room subject commit was ambiguous; reconciling exact durable subject before replying"
            );
            if reconcile_ambiguous_subject_commit(
                room_registry,
                &room,
                &room_actor,
                durable_recovery_snapshot.as_ref(),
                waddle_xmpp::muc::SubjectState {
                    texts: texts.clone(),
                    setter: setter.clone(),
                    setter_nick: setter_nick.to_string(),
                    set_at,
                },
            )
            .await
            {
                PersistRoomSubjectEventOutcome::Committed
            } else {
                retryable_subject_bounce(
                    &message,
                    &room,
                    &sender,
                    "This room's subject change is being reconciled; please retry.",
                )
            }
        }
        Err(SendError::HandlerError(SetSubjectError::NotOwner)) => {
            warn!(
                room = %room,
                setter = %setter,
                "PersistRoomSubject: exact actor lost room ownership; rejecting subject change"
            );
            match room_registry
                .ask(DemoteRoomIfExactActor {
                    room_jid: room.clone(),
                    actor_ref: room_actor,
                })
                .await
            {
                Ok(true) => {}
                Ok(false) => warn!(
                    room = %room,
                    "PersistRoomSubject: exact actor demotion found a different room incarnation"
                ),
                Err(error) => warn!(
                    room = %room,
                    error = ?error,
                    "PersistRoomSubject: exact actor demotion request failed"
                ),
            }
            retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry.",
            )
        }
        Err(SendError::HandlerError(
            SetSubjectError::OwnershipUnavailable | SetSubjectError::PersistFailedBeforeApply,
        )) => {
            warn!(
                room = %room,
                setter = %setter,
                "PersistRoomSubject: exact room ownership could not be confirmed; rejecting subject change"
            );
            retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry the subject change.",
            )
        }
        Err(error) => {
            warn!(
                room = %room,
                setter = %setter,
                error = ?error,
                "PersistRoomSubject: SetSubject actor transport failed; rejecting subject change"
            );
            retryable_subject_bounce(
                &message,
                &room,
                &sender,
                "This room is temporarily unavailable; please retry the subject change.",
            )
        }
    }
}

async fn reconcile_ambiguous_subject_commit(
    room_registry: &ActorRef<RoomRegistryActor>,
    room: &BareJid,
    stale_actor: &ActorRef<RoomActor>,
    durable_recovery_snapshot: Option<&waddle_xmpp::muc::room_actor::RoomSnapshot>,
    intended_subject: waddle_xmpp::muc::SubjectState,
) -> bool {
    let Some(recovery_snapshot) = durable_recovery_snapshot else {
        return false;
    };
    let _ = room_registry
        .ask(DemoteRoomIfExactActor {
            room_jid: room.clone(),
            actor_ref: stale_actor.clone(),
        })
        .await;
    let recovered_room = match room_registry
        .ask(GetOrCreateRoom {
            room_jid: room.clone(),
            waddle_id: recovery_snapshot.room.waddle_id.clone(),
            channel_id: recovery_snapshot.room.channel_id.clone(),
            config: recovery_snapshot.room.config.clone(),
        })
        .await
    {
        Ok(room) => room,
        Err(error) => {
            warn!(room = %room, %error, "ambiguous subject reconciliation could not restore the room");
            return false;
        }
    };
    recovered_room
        .actor_ref
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
        .map(|snapshot| snapshot.room.subject == Some(intended_subject))
        .unwrap_or(false)
}

fn retryable_subject_bounce(
    message: &Message,
    room: &BareJid,
    sender: &FullJid,
    text: &str,
) -> PersistRoomSubjectEventOutcome {
    PersistRoomSubjectEventOutcome::BounceAndHalt(Box::new(build_message_error_reply(
        message,
        room,
        sender,
        resource_constraint_error(text),
    )))
}

#[cfg(test)]
mod plan_tests {
    use super::super::effects::room::{ExternalRoomEffect, RoomActorMutation};
    use super::super::effects::{Effect, ExternalEffect, PlanSink, PlanSuppressionPolicy};
    use super::*;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;

    #[tokio::test]
    async fn plan_subject_records_mutation_without_changing_room_actor() {
        let state = create_test_websocket_state().await;
        let room: BareJid = "planned-subject@muc.example.com".parse().expect("room");
        let sender: FullJid = "alice@example.com/web".parse().expect("sender");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "test".to_owned(),
                channel_id: "test".to_owned(),
                config: Default::default(),
            })
            .await
            .expect("create room");
        let sink = PlanSink::new();
        let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry);
        deps.room_registry = Some(&state.deps.protocol.room_registry);
        deps.effects = &sink;
        let texts = RoomSubjectTexts::from_message_subjects(&std::collections::BTreeMap::from([(
            xmpp_parsers::message::Lang::default(),
            "planned subject".to_owned(),
        )]));
        let result = persist_room_subject_event(
            &deps,
            PersistRoomSubjectRequest {
                room,
                claim_fence: None,
                texts,
                setter: sender.to_bare(),
                sender,
                message: Box::new(Message::new(None::<Jid>)),
                setter_nick: "alice".to_owned(),
                set_at: chrono::Utc::now(),
            },
        )
        .await;
        assert!(matches!(result, PersistRoomSubjectEventOutcome::Committed));
        assert!(actor
            .ask(GetSnapshot)
            .reply_timeout(std::time::Duration::from_secs(5))
            .await
            .expect("snapshot")
            .room
            .subject
            .is_none());
        let (plan, _) = sink.take();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].suppression, PlanSuppressionPolicy::Always);
        assert!(matches!(
            &plan[0].effect,
            Effect::External(ExternalEffect::Room(
                ExternalRoomEffect::RoomActorMutation {
                    mutation: RoomActorMutation::SetSubject { .. },
                    ..
                }
            ))
        ));
        let effect = plan.into_iter().next().expect("subject effect");
        let executed = super::super::effects::EffectSink::execute(
            &super::super::effects::ImmediateSink,
            effect,
            &deps,
        )
        .await;
        assert!(matches!(
            executed,
            super::super::effects::EffectOutcome::Completed
        ));
        assert!(actor
            .ask(GetSnapshot)
            .reply_timeout(std::time::Duration::from_secs(5))
            .await
            .expect("executed snapshot")
            .room
            .subject
            .is_some());
    }
}
