//! The account coordinator as a connection's binding publisher.
//!
//! [`CoordinatorBindingPublisher`] maps each step onto the coordinator's
//! slot operations (`docs/planning/2138-account-coordinator.md`, "The
//! binding publisher"). The coordinator records which binding holds a
//! resource; it holds no outbound endpoint, so it has nothing to withdraw
//! from a routing index.

use std::convert::Infallible;

use jid::FullJid;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceTransitionError, SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::BindingRef;

use super::binding::{
    BindingConfirmation, BindingPublication, BindingPublisher, BindingRetirement,
};
use crate::account_coordinator::{
    AccountCoordinatorError, ConfirmOutcome, CoordinatorBindingPublisher, PublishOutcome,
    PublishedBinding, RetireEvidence, RetireOutcome,
};

/// A committed resume as the coordinator records it: the binding the
/// session was last held by, and the evidence that the resume committed.
#[derive(Debug, Clone)]
pub(crate) struct CoordinatorResume {
    pub(crate) previous: BindingRef,
    pub(crate) receipt: SuccessfulResumeReceipt,
}

fn publication(
    outcome: PublishOutcome,
) -> BindingPublication<PublishedBinding, AccountResourceTransitionError> {
    match outcome {
        PublishOutcome::Published(published) => BindingPublication::Published(published),
        PublishOutcome::ReplacementPending {
            displaced,
            revision,
        } => BindingPublication::ReplacementPending {
            displaced,
            revision,
        },
        PublishOutcome::Refused(error) => BindingPublication::Refused(error),
    }
}

fn retirement(outcome: RetireOutcome) -> BindingRetirement<AccountResourceTransitionError> {
    match outcome {
        RetireOutcome::Retired { .. } => BindingRetirement::Retired,
        RetireOutcome::Stale(_) => BindingRetirement::NotCurrent { current: None },
        RetireOutcome::Superseded(current) => BindingRetirement::NotCurrent {
            current: Some(current),
        },
        RetireOutcome::Refused(error) => BindingRetirement::Refused(error),
    }
}

impl BindingPublisher for CoordinatorBindingPublisher {
    type Published = PublishedBinding;
    type Resume = CoordinatorResume;
    type Receipt = Box<AccountResourceBindSuccessReceipt>;
    type Refusal = AccountResourceTransitionError;
    type Withdrawn = Infallible;
    type RemoteRetirement = BindingRetirement<AccountResourceTransitionError>;
    type Error = AccountCoordinatorError;

    async fn publish(
        &self,
        jid: &FullJid,
        binding: &BindingRef,
        lifetime: ResourceLifetime,
    ) -> Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error> {
        CoordinatorBindingPublisher::publish(self, jid, binding, lifetime)
            .await
            .map(publication)
    }

    async fn publish_resumed(
        &self,
        jid: &FullJid,
        binding: &BindingRef,
        resume: Self::Resume,
    ) -> Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error> {
        CoordinatorBindingPublisher::publish_resumed(
            self,
            jid,
            &resume.previous,
            binding,
            resume.receipt,
        )
        .await
        .map(publication)
    }

    async fn confirm(
        &self,
        jid: &FullJid,
        published: &Self::Published,
    ) -> Result<BindingConfirmation<Self::Receipt, Self::Refusal>, Self::Error> {
        Ok(
            match CoordinatorBindingPublisher::confirm(self, jid, published).await? {
                ConfirmOutcome::Confirmed(receipt) => BindingConfirmation::Confirmed(receipt),
                ConfirmOutcome::NotCurrent => BindingConfirmation::NotCurrent,
                ConfirmOutcome::Refused(error) => BindingConfirmation::Refused(error),
            },
        )
    }

    fn withdraw(&self, _jid: &FullJid, _published: &Self::Published) -> Option<Self::Withdrawn> {
        None
    }

    async fn retire(
        &self,
        jid: &FullJid,
        published: &Self::Published,
        evidence: RetireEvidence,
    ) -> Result<BindingRetirement<Self::Refusal>, Self::Error> {
        CoordinatorBindingPublisher::retire(self, jid, published, evidence)
            .await
            .map(retirement)
    }

    async fn retire_remote(
        &self,
        jid: &FullJid,
        published: &Self::Published,
        evidence: RetireEvidence,
    ) -> Result<Self::RemoteRetirement, Self::Error> {
        CoordinatorBindingPublisher::retire_remote(self, jid, published, evidence)
            .await
            .map(retirement)
    }
}
