//! The coordinators of this node, one per bare JID, created on demand.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use tokio::sync::Mutex;
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceOperation,
};
use waddle_xmpp::ownership::BindingRef;
use waddle_xmpp::telemetry::account_coordinator::record_mailbox_rejected;
use waddle_xmpp::telemetry::attributes::MailboxRejection;

use super::actor::AccountCoordinatorActor;
use super::authority::AccountBindingAuthority;
use super::claims::{AccountClaimSource, ConnectionLiveness};
use super::config::AccountCoordinatorConfig;
use super::error::AccountCoordinatorError;
use super::messages::{
    AccountView, Adopt, AdoptionReport, BindSuccess, BindingReply, Execute, MarkPresented,
    OverdueRetirements, ReadView, ReclaimIfIdle, Repair, RepairEvidence, RepairOutcome, SlotStatus,
    Status, Unregister, UnregisterOutcome, UnregisterRequest,
};
use crate::account_bindings::AccountBindingStore;

/// The account coordinators of this node.
///
/// A coordinator is created for an account the first time the account is
/// addressed, under the claim identity the claim source returns then, and
/// stays until [`Self::reclaim_idle`] finds its account without a binding
/// and idle. Requests wait a bounded time for mailbox room and for the
/// reply; a request the mailbox refuses is a typed error and is counted.
pub struct AccountCoordinators {
    store: Arc<dyn AccountBindingStore>,
    claims: Arc<dyn AccountClaimSource>,
    liveness: Arc<dyn ConnectionLiveness>,
    config: AccountCoordinatorConfig,
    actors: Mutex<HashMap<BareJid, ActorRef<AccountCoordinatorActor>>>,
}

/// Send one message to an account's coordinator. A coordinator that
/// stopped before taking the message is replaced once and the message is
/// sent again; the message was not handled, so nothing is repeated.
macro_rules! ask {
    ($directory:expr, $account:expr, $message:expr) => {{
        let directory = $directory;
        let account = $account;
        let mut message = $message;
        let mut respawned = false;
        loop {
            let actor = directory.coordinator(account).await?;
            let result = actor
                .ask(message)
                .mailbox_timeout(directory.config.mailbox_timeout)
                .reply_timeout(directory.config.reply_timeout)
                .await;
            match result {
                Ok(reply) => break Ok(reply),
                Err(SendError::HandlerError(error)) => break Err(error),
                Err(SendError::ActorNotRunning(returned)) if !respawned => {
                    directory.forget(account, &actor).await;
                    message = returned;
                    respawned = true;
                }
                Err(SendError::MailboxFull(_)) => break Err(refused(MailboxRejection::Full)),
                Err(SendError::Timeout(_)) => break Err(refused(MailboxRejection::Deadline)),
                Err(SendError::ActorNotRunning(_) | SendError::ActorStopped) => {
                    directory.forget(account, &actor).await;
                    break Err(refused(MailboxRejection::Closed));
                }
            }
        }
    }};
}

fn refused(reason: MailboxRejection) -> AccountCoordinatorError {
    record_mailbox_rejected(reason);
    AccountCoordinatorError::Mailbox(reason)
}

impl AccountCoordinators {
    pub fn new(
        store: Arc<dyn AccountBindingStore>,
        claims: Arc<dyn AccountClaimSource>,
        liveness: Arc<dyn ConnectionLiveness>,
        config: AccountCoordinatorConfig,
    ) -> Self {
        Self {
            store,
            claims,
            liveness,
            config,
            actors: Mutex::new(HashMap::new()),
        }
    }

    /// The running coordinator of `account`, created under the account's
    /// current claim when there is none.
    async fn coordinator(
        &self,
        account: &BareJid,
    ) -> Result<ActorRef<AccountCoordinatorActor>, AccountCoordinatorError> {
        let mut actors = self.actors.lock().await;
        if let Some(actor) = actors.get(account).filter(|actor| actor.is_alive()) {
            return Ok(actor.clone());
        }
        let claim_identity = self.claims.current_claim(account).await?;
        let actor = AccountCoordinatorActor::spawn_with_mailbox(
            AccountCoordinatorActor::new(
                claim_identity,
                Arc::clone(&self.store),
                Arc::clone(&self.liveness),
                self.config,
            ),
            mailbox::bounded(self.config.mailbox_capacity),
        );
        actors.insert(account.clone(), actor.clone());
        Ok(actor)
    }

    /// Drop the directory's entry for a coordinator that stopped, unless a
    /// newer coordinator already took its place.
    async fn forget(&self, account: &BareJid, stopped: &ActorRef<AccountCoordinatorActor>) {
        let mut actors = self.actors.lock().await;
        if actors
            .get(account)
            .is_some_and(|actor| actor.id() == stopped.id())
        {
            actors.remove(account);
        }
    }

    /// One coherent read of the account's resources. An error is a failed
    /// read, never an account without resources.
    pub async fn view(&self, account: &BareJid) -> Result<AccountView, AccountCoordinatorError> {
        ask!(self, account, ReadView)
    }

    /// Complete a retirement whose endpoint evidence is overdue.
    pub async fn repair(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        evidence: RepairEvidence,
    ) -> Result<RepairOutcome, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            Repair {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
                evidence,
            }
        )
    }

    /// The account's retirements whose endpoint evidence is past its
    /// deadline. Each stays non-routable until it is repaired.
    pub async fn overdue_retirements(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, BindingRef)>, AccountCoordinatorError> {
        ask!(self, account, OverdueRetirements)
    }

    /// Take over an account whose claim moved to this node: replace any
    /// coordinator this node still runs for it, load its rows under the
    /// current claim and adopt the bindings on live connections.
    pub async fn adopt_account(
        &self,
        account: &BareJid,
    ) -> Result<AdoptionReport, AccountCoordinatorError> {
        if let Some(previous) = self.actors.lock().await.remove(account) {
            previous.kill();
        }
        ask!(self, account, Adopt)
    }

    /// Stop every coordinator whose account holds no binding and has been
    /// idle for the configured time. Returns how many stopped.
    pub async fn reclaim_idle(&self) -> usize {
        let candidates: Vec<_> = self
            .actors
            .lock()
            .await
            .iter()
            .map(|(account, actor)| (account.clone(), actor.clone()))
            .collect();
        let mut reclaimed = 0;
        for (account, actor) in candidates {
            let stopped = match actor
                .ask(ReclaimIfIdle)
                .mailbox_timeout(self.config.mailbox_timeout)
                .reply_timeout(self.config.reply_timeout)
                .await
            {
                Ok(stopped) => stopped,
                Err(SendError::ActorNotRunning(_) | SendError::ActorStopped) => true,
                Err(_) => false,
            };
            if stopped {
                self.forget(&account, &actor).await;
                reclaimed += 1;
            }
        }
        reclaimed
    }

    /// How many coordinators this node currently runs.
    pub async fn loaded_accounts(&self) -> usize {
        self.actors.lock().await.len()
    }
}

#[async_trait]
impl AccountBindingAuthority for AccountCoordinators {
    async fn slot_status(&self, full_jid: &FullJid) -> Result<SlotStatus, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            Status {
                full_jid: full_jid.clone(),
            }
        )
    }

    async fn execute(
        &self,
        full_jid: &FullJid,
        operation: AccountResourceOperation,
    ) -> Result<BindingReply, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            Execute {
                full_jid: full_jid.clone(),
                operation,
            }
        )
    }

    async fn bind_success(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<Option<AccountResourceBindSuccessReceipt>, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            BindSuccess {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
            }
        )
    }

    async fn mark_presented(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<bool, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            MarkPresented {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
                presentation,
            }
        )
    }

    async fn unregister(
        &self,
        full_jid: &FullJid,
        request: UnregisterRequest,
    ) -> Result<UnregisterOutcome, AccountCoordinatorError> {
        let account = full_jid.to_bare();
        ask!(
            self,
            &account,
            Unregister {
                full_jid: full_jid.clone(),
                request,
            }
        )
    }
}
