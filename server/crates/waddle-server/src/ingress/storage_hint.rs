//! No-store exact-resource messages have one live attempt, never a ledger retry.
use jid::FullJid;
use waddle_xmpp::xep::xep0334::{add_hint, has_hint, Hint};
use xmpp_parsers::message::Message;

/// The hint applies even when the exact resource is already absent at capture.
pub(crate) fn transient_target(message: &Message) -> Option<&FullJid> {
    let target = message.to.as_ref()?.try_as_full().ok()?;
    (matches!(
        message.type_,
        xmpp_parsers::message::MessageType::Chat
            | xmpp_parsers::message::MessageType::Normal
            | xmpp_parsers::message::MessageType::Headline
    ) && message.from.as_ref()?.to_bare() != target.to_bare()
        && has_hint(message, Hint::NoStore)
        && !has_hint(message, Hint::Store))
    .then_some(target)
}

/// Full room occupants and room-owned work retain their specialized authority.
/// An ordinary offline direct plan may legitimately have no route intent.
pub(super) fn protects_plan(plan: &super::IngressPlan) -> bool {
    use waddle_xmpp::ingress::IngressEffectIntent;
    transient_target(&plan.sanitized_message).is_some()
        && matches!(plan.room_execution, super::RoomExecutionPath::None)
        && !plan.intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteOccupantPm { .. }
                    | IngressEffectIntent::RouteMucGroupchat { .. }
                    | IngressEffectIntent::RouteMucSystemBroadcast { .. }
                    | IngressEffectIntent::DispatchToRoomRemote { .. }
            )
        })
}

/// Omitting durable custody must not widen the existing intake size limit.
/// Encode only an ephemeral validation copy through the same intent codec;
/// the admitted plan continues to carry `prepared: None` into persistence.
pub(super) fn validate_capture(
    plan: &super::IngressPlan,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    use crate::server::routes::interpret::effects::{delivery::ExternalDeliveryEffect, Effect};
    use waddle_xmpp::{
        ingress::{IngressEffectIntent, StoredMessagePayload},
        Stanza,
    };
    if !protects_plan(plan) {
        return Ok(());
    }
    for intent in plan
        .intents
        .iter()
        .filter(|intent| protects_intent(&plan.sanitized_message, intent))
    {
        let Some(progress) = super::recorded::RouteProgress::from_intent(intent, None, Vec::new())?
        else {
            continue;
        };
        for planned in &plan.plan {
            let Effect::External(effect) = &planned.effect else {
                continue;
            };
            if !progress.matches(effect) {
                continue;
            }
            let super::ExternalEffect::Delivery(
                ExternalDeliveryEffect::RouteToPeer { stanza, .. }
                | ExternalDeliveryEffect::QueueDetached { stanza, .. }
                | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
                | ExternalDeliveryEffect::HostOwnedCopy { stanza, .. },
            ) = effect
            else {
                continue;
            };
            let Stanza::Message(message) = stanza.as_ref() else {
                continue;
            };
            let mut validation = intent.clone();
            if let IngressEffectIntent::RouteDirect { prepared, .. } = &mut validation {
                *prepared = Some(StoredMessagePayload::new(message.clone()).map_err(|_| {
                    crate::ingress_uow::IngressUowError::Plan(
                        super::effects::PlanFailure::InvalidPreparedMessage,
                    )
                })?);
            }
            validation.with_encoded_v1(|_, _| ())?;
        }
    }
    Ok(())
}

pub(super) async fn received_at(
    uow: &crate::ingress_uow::IngressUnitOfWork,
    key: waddle_xmpp::ingress::MessageKey,
) -> Result<chrono::DateTime<chrono::Utc>, crate::ingress_uow::IngressUowError> {
    let mut tx = uow.begin().await?;
    let received_at =
        crate::ingress_uow::CanonicalMessageRepository::created_at(&mut tx, key).await?;
    tx.commit().await?;
    Ok(received_at)
}

/// A caller must pass the frozen audience, not a current reachability snapshot.
pub(crate) fn forbids_direct_handoff(message: &Message, fanout: &[FullJid]) -> bool {
    transient_target(message).is_some_and(|target| {
        !fanout.is_empty()
            && fanout
                .iter()
                .all(|resource| resource.to_bare() == target.to_bare())
    })
}

pub(super) fn envelope(message: &Message) -> crate::ingress_substrate::MessageEnvelope {
    let mut metadata = Message::new(message.to.clone());
    metadata.from = message.from.clone();
    metadata.id = message.id.clone();
    metadata.type_ = message.type_.clone();
    add_hint(&mut metadata, Hint::NoStore);
    crate::ingress_substrate::MessageEnvelope::new(metadata)
}

pub(super) fn protects_intent(
    message: &Message,
    intent: &waddle_xmpp::ingress::IngressEffectIntent,
) -> bool {
    matches!(intent, waddle_xmpp::ingress::IngressEffectIntent::RouteDirect { recipient, fanout, .. }
        if transient_target(message).is_some_and(|target| target.to_bare() == *recipient)
            && forbids_direct_handoff(message, fanout))
}

/// Preserve actual delivery/custody evidence before recording policy completion.
pub(super) async fn settle(
    tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    key: waddle_xmpp::ingress::MessageKey,
    intent: &waddle_xmpp::ingress::IngressEffectIntent,
    receipt: &super::EffectReceiptKey,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    use crate::ingress_uow::{
        DeliveryProgressRepository, EffectReceiptRepository, PolicyDiscardReason,
        SendAttemptRepository, SendAttemptStatus, SendObligation,
    };
    if let waddle_xmpp::ingress::IngressEffectIntent::RouteDirect { fanout, .. } = intent {
        let completed = DeliveryProgressRepository::load(tx, key, receipt).await?;
        let mut proven = !fanout.is_empty();
        for resource in fanout {
            let obligation = SendObligation {
                message: key,
                receipt: receipt.clone(),
                recipient: resource.clone(),
            };
            proven &= completed.contains(resource)
                || SendAttemptRepository::has_custody(tx, &obligation).await?
                || matches!(
                    SendAttemptRepository::status(tx, &obligation).await?,
                    Some(SendAttemptStatus::Completed)
                );
        }
        if proven {
            crate::ingress_uow::settle_recorded(tx, key, std::slice::from_ref(intent)).await?;
            return Ok(());
        }
    }
    EffectReceiptRepository::record_policy_discard(
        tx,
        key,
        receipt,
        PolicyDiscardReason::StorageHintForbidsHandoff,
    )
    .await
}

/// These effects carry only the original in-memory copy. Its policy receipt
/// deliberately does not authorize another delivery or a keyed ingress append.
pub(super) fn is_transient_delivery(effect: &super::ExternalEffect) -> bool {
    use super::ExternalEffect;
    use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
    use waddle_xmpp::Stanza;
    match effect {
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid {
                target: jid,
                stanza,
                ..
            }
            | ExternalDeliveryEffect::HostOwnedCopy {
                target: jid,
                stanza,
            },
        ) => {
            matches!(stanza.as_ref(), Stanza::Message(message) if forbids_direct_handoff(message, std::slice::from_ref(jid)))
        }
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            resources,
            stanza,
            ..
        }) => {
            matches!(stanza.as_ref(), Stanza::Message(message) if forbids_direct_handoff(message, resources))
        }
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::Carbons { message, .. }
            | ExternalDeliveryEffect::RelayCarbons { message, .. },
        ) => message
            .to
            .as_ref()
            .and_then(|to| to.try_as_full().ok())
            .is_some_and(|target| forbids_direct_handoff(message, std::slice::from_ref(target))),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xep0334_full_nonself_no_store_route_forbids_handoff() {
        let target: FullJid = "juliet@example.com/phone".parse().expect("target");
        let mut message = Message::new(Some(target.clone().into()));
        message.from = Some("romeo@example.com/web".parse().expect("sender"));
        assert!(!forbids_direct_handoff(
            &message,
            std::slice::from_ref(&target)
        ));
        add_hint(&mut message, Hint::NoStore);
        assert!(forbids_direct_handoff(
            &message,
            std::slice::from_ref(&target)
        ));
        assert!(!forbids_direct_handoff(&message, &[]));
        let other: FullJid = "juliet@example.com/newcomer".parse().expect("other");
        assert!(forbids_direct_handoff(
            &message,
            &[target.clone(), other.clone()]
        ));
        assert!(forbids_direct_handoff(&message, &[other]));
        let foreign: FullJid = "other@example.com/phone".parse().expect("foreign");
        assert!(!forbids_direct_handoff(&message, &[foreign]));
        let mut bare = message.clone();
        bare.to = Some(target.to_bare().into());
        assert!(!forbids_direct_handoff(
            &bare,
            std::slice::from_ref(&target)
        ));
        let mut self_message = message.clone();
        self_message.from = Some("juliet@example.com/laptop".parse().expect("self"));
        assert!(!forbids_direct_handoff(
            &self_message,
            std::slice::from_ref(&target)
        ));
        for type_ in [
            xmpp_parsers::message::MessageType::Groupchat,
            xmpp_parsers::message::MessageType::Error,
        ] {
            let mut specialized = message.clone();
            specialized.type_ = type_;
            assert!(transient_target(&specialized).is_none());
        }
        let mut temporarily_storable = message.clone();
        temporarily_storable.payloads.clear();
        add_hint(&mut temporarily_storable, Hint::NoPermanentStore);
        assert!(transient_target(&temporarily_storable).is_none());
        add_hint(&mut message, Hint::Store);
        assert!(!forbids_direct_handoff(&message, &[target]));
    }

    #[test]
    fn no_store_primary_policy_excludes_occupant_private_messages() {
        let mut message = Message::new(Some(
            "room@muc.example.com/juliet".parse().expect("occupant"),
        ));
        message.from = Some("romeo@example.com/web".parse().expect("sender"));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        add_hint(&mut message, Hint::NoStore);
        let mut plan = crate::ingress::IngressPlan {
            sanitized_message: message,
            intents: vec![],
            plan: vec![],
            failure: None,
            rejection: None,
            error_reply: None,
            room_canonical_message: None,
            room_execution: crate::ingress::RoomExecutionPath::None,
        };
        assert!(
            protects_plan(&plan),
            "ordinary offline primary plan may be empty"
        );
        plan.intents
            .push(waddle_xmpp::ingress::IngressEffectIntent::RouteOccupantPm {
                recipient: "juliet@example.com/phone".parse().expect("target"),
                sender: "room@muc.example.com/romeo"
                    .parse()
                    .expect("occupant sender"),
            });
        assert!(
            !protects_plan(&plan),
            "room private-message authority stays specialized"
        );
    }

    #[test]
    fn transient_envelope_keeps_policy_and_identity_without_payload() {
        let mut message = Message::new(Some("juliet@example.com/phone".parse().expect("target")));
        message.from = Some("romeo@example.com/web".parse().expect("sender"));
        message
            .bodies
            .insert(Default::default(), "secret body".into());
        message
            .subjects
            .insert(Default::default(), "secret subject".into());
        message.payloads.push(
            minidom::Element::builder("secret", "urn:waddle:test")
                .append("secret extension")
                .build(),
        );
        add_hint(&mut message, Hint::NoStore);
        let stored = envelope(&message);
        assert_eq!(stored.message().from, message.from);
        assert_eq!(stored.message().to, message.to);
        assert!(stored.message().bodies.is_empty());
        assert!(stored.message().subjects.is_empty());
        assert!(stored.message().thread.is_none());
        assert_eq!(stored.message().payloads.len(), 1);
        assert!(has_hint(stored.message(), Hint::NoStore));
    }
}
