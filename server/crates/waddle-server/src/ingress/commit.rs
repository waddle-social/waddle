//! Phase B: one immutable plan, retried as complete fresh transactions.
use super::decision::{AliasOutcomeClass, IngressDecision, IngressDecisionClass};
use super::identity::IngressStreamIdentity;
use super::submission::IngressSubmission;
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::DeliveryProgressRepository;
use crate::ingress_uow::EffectReceiptRepository;
use crate::ingress_uow::{
    run_with_retry, CanonicalMessageRepository, DbRetryClass, EffectIntentRepository,
    IngressUnitOfWork, IngressUowError, IngressUowTransaction, ReconcileVerdict,
};
use crate::server::routes::interpret::effects::PlanFailure;
use crate::server::routes::interpret::effects::PlanSuppressionPolicy;
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::time::Instant;
use waddle_xmpp::ingress::{AliasOutcome, AliasResolution, IngressEffectIntent, MessageKey};

pub(crate) mod commit_hooks;
#[cfg(test)]
pub(crate) mod commit_race_gate;
#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alias_denial_replay_tests.rs"]
mod alias_denial_replay_tests;

mod implementation;
pub use implementation::classify_failure;
pub use implementation::commit_submission;
pub(crate) use implementation::commit_transaction;
pub(super) use implementation::live_recipient_delegated;
#[cfg(test)]
use implementation::missing_planned_archive_authority;
pub(super) use implementation::reconcile_invitation_delivery_receipts;
pub use implementation::IngressCommitFailure;
