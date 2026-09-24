//! Restore the frozen carbon audience without re-running XEP-0280 selection.
use super::{decision::EffectReceiptKey, RecoveryInput};
use crate::{
    ingress_uow::IngressUowError,
    server::routes::interpret::effects::{
        delivery::ExternalDeliveryEffect, Effect, ExternalEffect, IngressPlan,
        PlanEffectDependency, PlannedEffect,
    },
};
use jid::BareJid;
use waddle_xmpp::{ingress::IngressEffectIntent, protocol::CarbonKind};

pub(super) fn restore(
    plan: &mut IngressPlan,
    input: &RecoveryInput<'_>,
) -> Result<Vec<EffectReceiptKey>, IngressUowError> {
    let mut discarded = Vec::new();
    for intent in input.unreceipted {
        let (owner, kind) = match intent {
            IngressEffectIntent::Carbons {
                excluded_source,
                kind,
                ..
            } => (excluded_source.to_bare(), *kind),
            IngressEffectIntent::RelayCarbons { owner, kind, .. } => (owner.clone(), *kind),
            _ => continue,
        };
        if kind == CarbonKind::Received && input.blocked_recipients.contains(&owner) {
            discarded.push(super::super::durable::receipt_key(intent)?);
            continue;
        }
        let (message, dependencies) = carbon_message(input, &owner);
        match intent {
            IngressEffectIntent::Carbons {
                carbon_recipients,
                excluded_source,
                ..
            } => {
                for recipient in carbon_recipients {
                    plan.plan.push(PlannedEffect {
                        dependencies: dependencies.clone(),
                        ..PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                            ExternalDeliveryEffect::Carbons {
                                owner: owner.clone(),
                                recipient: recipient.clone(),
                                // Local intents already freeze exactly one destination;
                                // this source preserves their original receipt identity.
                                exclude: vec![excluded_source.clone()],
                                message: Box::new(message.clone()),
                                kind,
                            },
                        )))
                    });
                }
            }
            IngressEffectIntent::RelayCarbons { exclude, .. } => {
                plan.plan.push(PlannedEffect {
                    dependencies,
                    ..PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                        ExternalDeliveryEffect::RelayCarbons {
                            origin: None,
                            owner,
                            exclude: exclude.clone(),
                            message: Box::new(message),
                            kind,
                        },
                    )))
                });
            }
            _ => unreachable!("selected carbon intent"),
        }
    }
    Ok(discarded)
}

fn carbon_message(
    input: &RecoveryInput<'_>,
    owner: &BareJid,
) -> (xmpp_parsers::message::Message, Vec<PlanEffectDependency>) {
    // XEP-0280 forwards the original message. In particular a sent carbon's
    // inner `to` must remain the correspondent, never the carbon owner.
    let mut message = input.envelope.message().clone();
    let mut dependencies = Vec::new();
    for intent in input.recorded {
        if let IngressEffectIntent::ArchiveAuthoritative {
            archive, stanza_id, ..
        } = intent
        {
            if archive == owner {
                waddle_xmpp_core::xep0359::add_stanza_id(&mut message, stanza_id);
                dependencies.push(PlanEffectDependency::AfterArchive {
                    archive: archive.clone(),
                    minted: stanza_id.clone(),
                });
            }
        }
    }
    (message, dependencies)
}
