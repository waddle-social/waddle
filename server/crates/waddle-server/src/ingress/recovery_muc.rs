//! Rebuild only frozen room copies whose mutation and archive proofs are complete.
use super::RecoveryInput;
use crate::{
    ingress::room_canonical::{self, CanonicalSourceError},
    ingress_substrate::MessageEnvelope,
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan, PlannedEffect,
    },
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    Stanza,
};
use xmpp_parsers::message::Message;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum MucRecoveryError {
    #[error(transparent)]
    Source(#[from] CanonicalSourceError),
    #[error("prerequisite_pending")]
    PrerequisitePending,
}

pub(super) fn restore_muc_routes(
    plan: &mut IngressPlan,
    input: &RecoveryInput<'_>,
) -> Result<(), IngressUowError> {
    for intent in input.unreceipted {
        if !matches!(
            intent,
            IngressEffectIntent::RouteMucGroupchat { .. }
                | IngressEffectIntent::RouteMucSystemBroadcast { .. }
        ) {
            continue;
        }
        let source = match authorized_source(
            input.envelope,
            intent,
            input.recorded,
            input.unreceipted,
        ) {
            Ok(source) => source,
            Err(reason) => {
                tracing::debug!(key = ?input.key, %reason, "MUC recovery cannot rebuild frozen route");
                continue;
            }
        };
        let receipt = crate::ingress::receipt_key(intent)?;
        let Some(progress) = input
            .route_progress
            .iter()
            .find(|progress| progress.receipt == receipt)
        else {
            continue;
        };
        for occupant in progress
            .fanout
            .iter()
            .filter(|occupant| !progress.completed.contains(occupant))
        {
            plan.plan.push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                    route_identity: None,
                    call_setup: None,
                    bare: occupant.to_bare(),
                    resources: vec![occupant.clone()],
                    stanza: Box::new(Stanza::Message(room_canonical::occupant_copy_message(
                        source,
                        occupant,
                        input.recorded,
                    ))),
                }),
            )));
        }
    }
    Ok(())
}

fn authorized_source<'a>(
    envelope: &'a MessageEnvelope,
    intent: &'a IngressEffectIntent,
    recorded: &[IngressEffectIntent],
    pending: &[IngressEffectIntent],
) -> Result<&'a Message, MucRecoveryError> {
    let source = room_canonical::source(envelope, intent)?;
    let authorized = match intent {
        IngressEffectIntent::RouteMucGroupchat { room, .. } => {
            !waddle_xmpp::muc::is_groupchat_subject_change_message(source)
                || all_receipted(recorded, pending, |intent| {
                    matches!(intent,
                    IngressEffectIntent::RoomSubjectMutation { room: saved, .. } if saved == room)
                })
        }
        IngressEffectIntent::RouteMucSystemBroadcast {
            room,
            route_identity,
            ..
        } => {
            // Pin intents retain room authority, while every system archive is
            // correlated to this exact payload's recorded room stanza ID.
            all_receipted(recorded, pending, |intent| {
                matches!(intent,
                IngressEffectIntent::Pin { room: saved, .. } if saved == room)
            }) && all_receipted(recorded, pending, |intent| {
                matches!(intent,
                    IngressEffectIntent::SystemMessageArchive { archive, stanza_id, .. }
                    if archive == room && matches!(route_identity, EffectMessageIdentity::StanzaId(id) if id == stanza_id))
            })
        }
        _ => false,
    };
    if authorized {
        Ok(source)
    } else {
        Err(MucRecoveryError::PrerequisitePending)
    }
}

/// Missing producers do not prove completion; every matching recorded producer
/// must have a receipt before its dependent copies can leave recovery.
fn all_receipted(
    recorded: &[IngressEffectIntent],
    pending: &[IngressEffectIntent],
    matches: impl Fn(&IngressEffectIntent) -> bool,
) -> bool {
    let mut found = false;
    for intent in recorded.iter().filter(|intent| matches(intent)) {
        found = true;
        if pending.contains(intent) {
            return false;
        }
    }
    found
}

#[cfg(test)]
#[path = "recovery_muc_source_tests.rs"]
mod tests;
