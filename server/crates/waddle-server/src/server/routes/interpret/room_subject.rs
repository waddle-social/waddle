use super::*;
use kameo::error::SendError;
use waddle_xmpp::muc::room_actor::SetSubjectError;
use waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor;
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

    match room_actor
        .ask(SetSubject {
            texts,
            setter: setter.clone(),
            setter_nick,
            set_at,
        })
        .await
    {
        Ok(()) => PersistRoomSubjectEventOutcome::Committed,
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
