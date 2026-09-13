//! Committed responsibility and the bounded post-commit work it authorizes.
use crate::{
    ingress_substrate::EffectReceiptKind,
    ingress_uow::ReconcileVerdict,
    server::routes::interpret::effects::{
        AppliedDurableEffects, ExternalEffect, PlanEffectDependency,
    },
};
use jid::BareJid;
use waddle_xmpp::ingress::{IngressOrdinal, MessageKey};
pub use waddle_xmpp::telemetry::attributes::IngressDecisionClass;
use waddle_xmpp_core::xep0359::StanzaId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasOutcomeClass {
    Inserted,
    NoOrigin,
    Existing,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectReceiptKey {
    pub kind: EffectReceiptKind,
    pub semantic_identity_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct IngressDecision {
    pub class: IngressDecisionClass,
    pub message_key: Option<MessageKey>,
    pub ordinal: Option<IngressOrdinal>,
    pub alias: AliasOutcomeClass,
    pub verdict: Option<ReconcileVerdict>,
    pub archive_ids: Vec<(BareJid, StanzaId)>,
    pub applied_durable: std::sync::Arc<AppliedDurableEffects>,
    pub external: Vec<ExternalEffect>,
    /// Captured dependencies aligned with external effects.
    pub external_dependencies: Vec<Vec<PlanEffectDependency>>,
    /// Receipt identities fulfilled by each external effect, in the same order.
    pub external_receipts: Vec<Vec<EffectReceiptKey>>,
    /// Exact receipt union for effect indices handled by a transactional arm.
    pub arm_owned_receipts: Vec<EffectReceiptKey>,
    pub route_progress: Vec<super::recorded::RouteProgress>,
    pub receipts_pending: Vec<EffectReceiptKey>,
}

pub(super) fn bind_claim_keys(external: &mut [ExternalEffect], key: MessageKey) {
    for effect in external {
        if let crate::server::routes::interpret::effects::ExternalEffect::InviteLedger(
            crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation::Claim { message_key, .. }
        ) = effect {
            *message_key = Some(key);
        }
    }
}

pub(super) fn assemble_receipts(
    external: &[ExternalEffect],
    intents: &[waddle_xmpp::ingress::IngressEffectIntent],
    route_progress: &[super::recorded::RouteProgress],
) -> Result<(Vec<Vec<EffectReceiptKey>>, Vec<EffectReceiptKey>), crate::ingress_uow::IngressUowError>
{
    let mut external_receipts = super::durable::external_receipts(external, intents)?;
    // A progress-aware arm can own a strict subset of the frozen fanout.
    // Generic receipt mapping deliberately requires full coverage; add only
    // the exact route receipt whose aggregate this arm settles transactionally.
    for (index, effect) in external.iter().enumerate() {
        if super::execute_uow::owns(effect, route_progress) {
            for progress in route_progress
                .iter()
                .filter(|progress| progress.matches(effect))
            {
                if !external_receipts[index].contains(&progress.receipt) {
                    external_receipts[index].push(progress.receipt.clone());
                }
            }
        }
    }
    let mut arm_owned_receipts = Vec::new();
    for (index, effect) in external.iter().enumerate() {
        if super::execute_uow::owns(effect, route_progress) {
            for receipt in &external_receipts[index] {
                if !arm_owned_receipts.contains(receipt) {
                    arm_owned_receipts.push(receipt.clone());
                }
            }
        }
    }
    Ok((external_receipts, arm_owned_receipts))
}

#[cfg(test)]
mod tests {
    use super::IngressDecisionClass;
    #[test]
    fn decision_matrix_advances_only_committed_classes() {
        use IngressDecisionClass::*;
        for class in [
            Accepted,
            ExistingCommitted,
            ExistingConsistent,
            ExistingRepaired,
            ExistingDivergent,
            OwnerFirstAcceptance,
            OwnerDuplicate,
            AliasConflict,
            SemanticMalformed,
            AuthorizationDenied,
            PolicyDenied,
            CaptureOverflow,
        ] {
            assert!(class.advances(), "{class:?}");
        }
        for class in [
            PrincipalMissing,
            ClaimFenceMissing,
            RoomGenerationStale,
            FrontierStale,
            SmOrdinalConflict,
            IntentContradiction,
            Storage,
            SerializationExhaustion,
            Timeout,
            AmbiguousCommit,
            Lineage,
            EpochUnsupported,
        ] {
            assert!(!class.advances(), "{class:?}");
        }
    }
}
