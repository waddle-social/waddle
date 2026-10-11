//! Preview retries keep the original approval even when enrichment changes.
use crate::server::routes::interpret::effects::{
    direct::ExternalDirectEffect, Effect, ExternalEffect, IngressPlan, PlannedEffect,
};
use waddle_xmpp::ingress::IngressEffectIntent;

/// Existing canonical authority cannot acquire another preview target from a
/// newly enriched plan. Keep original slots eligible for normal payload
/// reconciliation; unapproved slots never enter the intent ledger.
pub(in crate::ingress) fn freeze_preview_intents(
    plan: &mut IngressPlan,
    recorded: &[IngressEffectIntent],
) {
    plan.intents.retain(|offered| {
        let IngressEffectIntent::LinkPreviewMediaRef { mutation } = offered else {
            return true;
        };
        recorded.iter().any(|intent| {
            matches!(intent, IngressEffectIntent::LinkPreviewMediaRef { mutation: saved }
                if saved.archive == mutation.archive
                    && saved.message_id == mutation.message_id
                    && saved.current_archive_stanza_id == mutation.current_archive_stanza_id
                    && saved.upload_slot_id == mutation.upload_slot_id)
        })
    });
}

/// Reconstruct pending effects using the stored mutations, without consulting
/// current enrichment. Source archive identity stays independent of the slot
/// selected by today's enrichment, and only the original slot can be repaired.
pub(in crate::ingress) fn restore_pending_preview_effects(
    plan: &mut IngressPlan,
    recorded: &[IngressEffectIntent],
    unreceipted: &[IngressEffectIntent],
) -> bool {
    let mut restored = false;
    for intent in recorded {
        let IngressEffectIntent::LinkPreviewMediaRef { mutation } = intent else {
            continue;
        };
        if !unreceipted.contains(intent) {
            continue;
        }
        let present = plan.plan.iter().any(|planned| {
            matches!(&planned.effect,
                Effect::External(ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs { mutations }
                    | ExternalDirectEffect::ClearLinkPreviewRefs { mutations })) if mutations.contains(mutation))
        });
        if present {
            continue;
        }
        if !plan.intents.contains(intent) {
            plan.intents.push(intent.clone());
        }
        plan.plan.push(PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
                mutations: vec![mutation.clone()],
            }),
        )));
        restored = true;
    }
    restored
}
