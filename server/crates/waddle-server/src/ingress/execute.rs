//! Bounded post-commit execution. Failures never revise ingress authority.

#[cfg(feature = "clustering")]
use crate::clustering::relay::RelayAskError;
#[cfg(feature = "clustering")]
use crate::clustering::relay::RelayHandle;
#[cfg(feature = "clustering")]
use crate::clustering::relay::RelayReplyReceiptToken;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RelayFrameCompletion;
#[cfg(feature = "clustering")]
use crate::clustering::NodeId;
use crate::ingress_substrate::EffectReceiptKind;
use crate::ingress_substrate::TerminalizeOutcome;
use crate::ingress_uow::DispatchReadiness;
use crate::server::routes::interpret::effects::PlanEffectDependency;
use crate::server::routes::interpret::effects::SettledRefusal;
use crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerError;
use std::time::Duration;

use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::telemetry::attributes::IngressEffectExecutionPhase;
use waddle_xmpp::Stanza;

use crate::db::Database;
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
    IngressUowTransaction,
};
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::direct::ExternalDirectEffect;
use crate::server::routes::interpret::effects::{
    Effect, EffectOutcome, ExternalEffect, ImmediateSink, PlannedEffect, SettledCompletion,
    SettledOutcome,
};
use crate::server::routes::interpret::{Deps, FullJidDeliveryOutcome, SmIngressAppendContext};

use super::decision::{EffectReceiptKey, IngressDecision};
use crate::ingress::append_authority::AppendAuthority;

#[path = "execute_dependencies.rs"]
mod dependencies;

#[path = "execute_carbon_progress.rs"]
mod carbon_progress;

#[path = "execute_observers.rs"]
mod observers;

#[path = "execute_muc_fanout.rs"]
mod muc_fanout;

#[path = "execute_dispatch_rounds.rs"]
mod dispatch_rounds;

#[cfg(test)]
#[path = "execute_dependency_tests.rs"]
mod dependency_tests;

#[cfg(test)]
#[path = "execute_test_hooks.rs"]
pub(crate) mod test_hooks;

#[cfg(test)]
#[path = "execute_settlement_tests.rs"]
mod settlement_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::{decision::AliasOutcomeClass, decision::IngressDecisionClass};
    use crate::ingress_substrate::EffectReceiptKind;
    use xmpp_parsers::message::Message;

    fn frame() -> ExternalEffect {
        ExternalEffect::Frame(Box::new(Stanza::Message(Message::new(None))))
    }

    #[test]
    fn external_outcome_preserves_delivery_uncertainty() {
        let cases = [
            (EffectOutcome::Completed, ExternalOutcome::Done),
            (EffectOutcome::Unavailable, ExternalOutcome::Failed),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered),
                ExternalOutcome::Done,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::QueuedDetached),
                ExternalOutcome::Done,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Unavailable),
                ExternalOutcome::Failed,
            ),
            (
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Dropped),
                ExternalOutcome::Uncertain,
            ),
            // A storage failure may have committed its row: recovery must not
            // count the attempt as proof that nothing landed (#1782).
            (
                EffectOutcome::InviteLedger(Err(
                    crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerError::Storage,
                )),
                ExternalOutcome::Uncertain,
            ),
            (
                EffectOutcome::MucUserDelivery(Err(
                    crate::server::routes::websocket::handlers::message::muc_invite::MucUserDeliveryError::Storage(
                        waddle_xmpp::pending_delivery::storage::PendingStorageError::Other("storage".to_owned()),
                    ),
                )),
                ExternalOutcome::Uncertain,
            ),
            // Deterministic refusals prove nothing was written.
            (
                EffectOutcome::MucUserDelivery(Err(
                    crate::server::routes::websocket::handlers::message::muc_invite::MucUserDeliveryError::QuotaExceeded,
                )),
                ExternalOutcome::Failed,
            ),
            (
                EffectOutcome::MucUserDelivery(Err(
                    crate::server::routes::websocket::handlers::message::muc_invite::MucUserDeliveryError::Unavailable,
                )),
                ExternalOutcome::Failed,
            ),
        ];
        for (result, expected) in cases {
            assert_eq!(
                classify_outcome(&frame(), result, &mut Vec::new()),
                expected
            );
        }
        #[cfg(feature = "clustering")]
        assert_eq!(
            classify_outcome(
                &frame(),
                EffectOutcome::Delivery(FullJidDeliveryOutcome::MaybeCommitted),
                &mut Vec::new()
            ),
            ExternalOutcome::Uncertain
        );
    }

    #[test]
    fn external_receipt_requires_every_mapped_effect_to_finish() {
        let key = EffectReceiptKey {
            kind: EffectReceiptKind::from_storage(1),
            semantic_identity_hash: [1; 32],
        };
        let decision = IngressDecision {
            class: IngressDecisionClass::Accepted,
            message_key: None,
            ordinal: None,
            alias: AliasOutcomeClass::NoOrigin,
            verdict: None,
            archive_ids: vec![],
            applied_durable: Default::default(),
            external_dependencies: vec![vec![], vec![]],
            external: vec![frame(), frame()],
            external_receipts: vec![vec![key.clone()], vec![key.clone()]],
            arm_owned_receipts: Vec::new(),
            route_progress: Vec::new(),
            receipts_pending: vec![key.clone()],
        };
        let mut outcomes = vec![(frame(), ExternalOutcome::Done)];
        assert!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 0).is_empty()
        );
        outcomes.push((frame(), ExternalOutcome::Failed));
        assert!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 1).is_empty()
        );
        outcomes[1].1 = ExternalOutcome::Done;
        assert_eq!(
            completed_receipts(&decision, &outcomes, &decision.external_receipts, 1),
            vec![key]
        );
    }

    #[test]
    fn frames_are_collected_without_changing_authority() {
        let mut frames = vec![];
        let stanza = Stanza::Message(Message::new(None));
        assert_eq!(
            classify_outcome(&frame(), EffectOutcome::Frames(vec![stanza]), &mut frames),
            ExternalOutcome::Done
        );
        assert_eq!(frames.len(), 1);
    }
    #[test]
    fn detached_batch_completion_preserves_incomplete_outcomes() {
        let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: None,
            call_setup: None,
            bare: "peer@example.com".parse().expect("bare"),
            resources: vec![
                "peer@example.com/one".parse().expect("first"),
                "peer@example.com/two".parse().expect("second"),
            ],
            stanza: Box::new(Stanza::Message(Message::new(None))),
        });
        assert_eq!(
            classify_outcome(
                &effect,
                EffectOutcome::Delivery(FullJidDeliveryOutcome::QueuedDetached),
                &mut Vec::new()
            ),
            ExternalOutcome::Done
        );
        assert_eq!(
            classify_outcome(
                &effect,
                EffectOutcome::Delivery(FullJidDeliveryOutcome::Dropped),
                &mut Vec::new()
            ),
            ExternalOutcome::Uncertain
        );
    }

    #[test]
    fn void_push_completion_does_not_prove_delivery() {
        let owner = "peer@example.com".parse().expect("owner");
        let effect = ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            owner,
            projection: crate::server::routes::interpret::effects::ProjectionRef(0),
            receipt: None,
        });
        assert_eq!(
            classify_outcome(&effect, EffectOutcome::Completed, &mut Vec::new()),
            ExternalOutcome::Uncertain
        );
    }
}

#[cfg(test)]
#[path = "execute_delivery_tests.rs"]
mod delivery_tests;

#[cfg(test)]
#[path = "execute_carbons_retry_tests.rs"]
mod carbons_retry_tests;

#[cfg(all(test, feature = "clustering"))]
#[path = "execute_carbon_fanout_tests.rs"]
mod carbon_fanout_tests;

#[cfg(test)]
#[path = "execute_observer_tests.rs"]
mod observer_tests;

#[cfg(test)]
#[path = "execute_inbox_offline_tests.rs"]
mod inbox_offline_tests;

#[cfg(test)]
#[path = "execute_direct_receipt_tests.rs"]
mod direct_receipt_tests;

#[cfg(test)]
#[path = "execute_local_carbons_tests.rs"]
mod local_carbons_tests;

#[cfg(test)]
#[path = "execute_frame_completion_tests.rs"]
mod frame_completion_tests;

#[cfg(test)]
#[path = "execute_invite_noop_tests.rs"]
mod invite_noop_tests;

#[cfg(test)]
#[path = "execute_detached_receipt_tests.rs"]
mod detached_receipt_tests;

#[cfg(test)]
#[path = "execute_dm_call_tests.rs"]
mod dm_call_tests;

#[cfg(test)]
#[path = "execute_activity_tests.rs"]
mod activity_tests;

#[cfg(all(test, feature = "clustering"))]
#[path = "execute_groupchat_receipt_tests.rs"]
mod groupchat_receipt_tests;

#[cfg(test)]
#[path = "execute_detached_fault_tests.rs"]
mod detached_fault_tests;

#[cfg(test)]
#[path = "execute_relay_detached_tests.rs"]
pub(crate) mod relay_detached_tests;

#[cfg(test)]
#[path = "execute_custody_tests.rs"]
mod custody_tests;

mod implementation;
use implementation::classify_outcome;
use implementation::completed_receipts;
pub use implementation::execute_effects;
#[cfg(test)]
use implementation::is_invite_delivery_dependent;
use implementation::meter_unresolved;
use implementation::proven_receipts;
pub(super) use implementation::supplies_dependency;
pub use implementation::terminalize_if_complete;
pub(super) use implementation::terminalize_if_complete_in_transaction;
pub(crate) use implementation::terminalize_if_complete_outcome;
pub use implementation::ExecutionPersistenceFailure;
pub use implementation::ExecutionReport;
pub use implementation::ExternalOutcome;
pub use implementation::FrameObligation;
#[cfg(feature = "clustering")]
pub use implementation::RelayFrameReceiptCompletion;
