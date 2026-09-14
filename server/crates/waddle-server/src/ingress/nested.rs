//! Host ingress owns its admission permit through commit, execution and settlement.
use std::{sync::Arc, time::Duration};

use tokio::sync::{oneshot, OwnedRwLockReadGuard};
use tokio::task::JoinHandle;
use waddle_xmpp::{ingress::TransportGeneration, Stanza};
use xmpp_parsers::stanza_error::StanzaError;

use super::{
    execute, Deps, ImmediateSink, IngressAuthority, IngressDecisionClass, IngressSubmission,
};
use crate::{
    auth::Session,
    server::routes::websocket::{
        interpret_loop::build_interpret_deps, ResolvedPrincipal, WebSocketState,
    },
};

const SETTLEMENT_BUDGET: Duration = Duration::from_secs(5);
const SETTLEMENT_RETRY_DELAY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityUnavailable {
    #[error("ingress authority is draining")]
    Stopped,
    #[error("ingress authority has a queued writer")]
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestedRefusal {
    InvalidHostSubmission,
    Decision(IngressDecisionClass),
    /// The worker ended without reporting whether commit succeeded.
    DecisionUnavailable,
}

pub enum NestedOutcome {
    Refused(NestedRefusal),
    Committed {
        decision_class: IngressDecisionClass,
        archive_ids: Vec<(jid::BareJid, waddle_xmpp_core::xep0359::StanzaId)>,
        /// A response waiter: aborting this handle cannot cancel authority-owned work.
        settlement: JoinHandle<SettlementOutcome>,
    },
}

#[derive(Debug)]
pub struct SettlementOutcome {
    pub rejection: Option<StanzaError>,
    pub terminal: Result<bool, execute::ExecutionPersistenceFailure>,
}

/// Owned backing for the host's borrowed interpreter dependencies.
pub struct NestedContinuation {
    state: Arc<WebSocketState>,
    session: Option<Session>,
    host_sender: jid::FullJid,
    #[cfg(test)]
    before_execute: Option<Arc<TestGate>>,
    #[cfg(test)]
    before_settlement: Option<Arc<TestGate>>,
}

impl NestedContinuation {
    pub fn new(
        state: Arc<WebSocketState>,
        session: Option<Session>,
        host_sender: jid::FullJid,
    ) -> Self {
        Self {
            state,
            session,
            host_sender,
            #[cfg(test)]
            before_execute: None,
            #[cfg(test)]
            before_settlement: TEST_BEFORE_SETTLEMENT.try_with(Arc::clone).ok(),
        }
    }

    fn deps(&self) -> Deps<'_> {
        let mut deps = build_interpret_deps(
            &self.state,
            self.session
                .as_ref()
                .map(ResolvedPrincipal::from_authenticated_session),
        );
        deps.host_sender = Some(
            crate::server::routes::interpret::HostOwnedResources::Sender(self.host_sender.clone()),
        );
        deps
    }
}

pub struct NestedIngressOperation {
    authority: Arc<IngressAuthority>,
    permit: OwnedRwLockReadGuard<bool>,
}

impl IngressAuthority {
    /// Admit without waiting: recursive readers must never queue behind drain.
    pub fn try_begin_nested(
        self: &Arc<Self>,
    ) -> Result<NestedIngressOperation, AuthorityUnavailable> {
        if self.cancellation.is_cancelled() {
            return Err(AuthorityUnavailable::Stopped);
        }
        let permit = Arc::clone(&self.admission)
            .try_read_owned()
            .map_err(|_| AuthorityUnavailable::Busy)?;
        if self.cancellation.is_cancelled() || !*permit {
            return Err(AuthorityUnavailable::Stopped);
        }
        Ok(NestedIngressOperation {
            authority: Arc::clone(self),
            permit,
        })
    }
}

impl NestedIngressOperation {
    /// Starts authority-owned work before the first commit await.
    pub async fn commit_and_continue(
        self,
        submission: IngressSubmission,
        continuation: NestedContinuation,
    ) -> NestedOutcome {
        if !matches!(
            submission.identity,
            super::IngressStreamIdentity::Extension { .. }
        ) || !matches!(submission.principal, super::IngressPrincipal::Extension(_))
            || !matches!(submission.connection_generation, TransportGeneration::Host)
        {
            return NestedOutcome::Refused(NestedRefusal::InvalidHostSubmission);
        }
        let (decision_tx, decision_rx) = oneshot::channel();
        let (settlement_tx, settlement_rx) = oneshot::channel();
        tokio::spawn(async move {
            let Self { authority, permit } = self;
            let decision = authority.commit_admitted(&submission).await;
            let _ = decision_tx.send((decision.class, decision.archive_ids.clone()));
            if decision.class.advances() {
                #[cfg(test)]
                if let Some(gate) = &continuation.before_execute {
                    gate.wait().await;
                }
                let mut report = execute::execute_effects(
                    &authority.uow,
                    &authority.database,
                    &decision,
                    &ImmediateSink,
                    &continuation.deps(),
                    SETTLEMENT_BUDGET,
                )
                .await;
                let rejection = report
                    .frame_obligations
                    .iter()
                    .flat_map(|obligation| &obligation.frames)
                    .find_map(stanza_error)
                    .or_else(|| report.refusal.map(
                        |crate::server::routes::interpret::effects::SettledRefusal::OfflineQuotaExceeded| {
                            crate::server::routes::interpret::offline_delivery::offline_quota_error()
                        },
                    ));
                #[cfg(test)]
                if let Some(gate) = &continuation.before_settlement {
                    gate.wait().await;
                }
                // Host frames are consumed here. Receipt writes are idempotent;
                // retry them without dispatching the committed effects again.
                let terminal = settle(&authority, &mut report).await;
                let _ = settlement_tx.send(SettlementOutcome {
                    rejection,
                    terminal,
                });
            }
            drop(permit);
        });
        match decision_rx.await {
            Ok((decision_class, archive_ids)) if decision_class.advances() => {
                NestedOutcome::Committed {
                    decision_class,
                    archive_ids,
                    settlement: tokio::spawn(async move {
                        match settlement_rx.await {
                            Ok(outcome) => outcome,
                            Err(_) => SettlementOutcome {
                                rejection: None,
                                terminal: Err(
                                    crate::ingress_uow::IngressUowError::AuthorityStopped.into(),
                                ),
                            },
                        }
                    }),
                }
            }
            Ok((class, _)) => NestedOutcome::Refused(NestedRefusal::Decision(class)),
            Err(_) => NestedOutcome::Refused(NestedRefusal::DecisionUnavailable),
        }
    }
}

async fn settle(
    authority: &IngressAuthority,
    report: &mut super::ExecutionReport,
) -> Result<bool, execute::ExecutionPersistenceFailure> {
    let deadline = tokio::time::Instant::now() + SETTLEMENT_BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let result = report
            .complete_frame_obligations(&authority.uow, &authority.database, remaining)
            .await;
        if result.is_ok() || tokio::time::Instant::now() + SETTLEMENT_RETRY_DELAY >= deadline {
            return result;
        }
        tokio::time::sleep(SETTLEMENT_RETRY_DELAY).await;
    }
}

fn stanza_error(stanza: &Stanza) -> Option<StanzaError> {
    match stanza {
        Stanza::Message(message) => message
            .payloads
            .iter()
            .find_map(|payload| StanzaError::try_from(payload.clone()).ok()),
        Stanza::Presence(presence) => presence
            .payloads
            .iter()
            .find_map(|payload| StanzaError::try_from(payload.clone()).ok()),
        Stanza::Iq(iq) => match iq.as_ref() {
            xmpp_parsers::iq::Iq::Error { error, .. } => Some(error.clone()),
            _ => None,
        },
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_BEFORE_SETTLEMENT: Arc<TestGate>;
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestGate {
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[cfg(test)]
impl TestGate {
    async fn wait(&self) {
        self.reached.notify_one();
        self.release.notified().await;
    }
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
