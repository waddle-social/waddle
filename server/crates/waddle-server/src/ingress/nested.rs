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

/// The admitted host invocation's actual internal frame consumer. Room echoes
/// are consumed by this host, while errors are surfaced by the settlement.
#[derive(Debug)]
pub(crate) struct HostFrameConsumer {
    sender: jid::FullJid,
}

impl HostFrameConsumer {
    pub(crate) fn consume(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
    ) -> waddle_xmpp::registry::BroadcastOutcome {
        if target != &self.sender
            || !matches!(stanza, Stanza::Message(message)
                if message.to.as_ref().and_then(|jid| jid.try_as_full().ok()) == Some(target))
        {
            return waddle_xmpp::registry::BroadcastOutcome::NotConnected;
        }
        // This is the existing host sink: there is no socket echo. The caller
        // records its keyed acceptance before returning completion to ingress.
        waddle_xmpp::registry::BroadcastOutcome::Delivered
    }
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
        deps.host_copy_consumer = Some(Arc::new(HostFrameConsumer {
            sender: self.host_sender.clone(),
        }));
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
        self.commit_tracked(submission, continuation, &mut Vec::new())
            .await
    }

    /// [`Self::commit_and_continue`], pushing the authority-owned work
    /// (commit, delivery and settlement) onto `work` as it starts, so a
    /// caller cancelled before the decision can still wait for it.
    pub async fn commit_tracked(
        self,
        submission: IngressSubmission,
        continuation: NestedContinuation,
        work: &mut Vec<JoinHandle<()>>,
    ) -> NestedOutcome {
        if !matches!(
            submission.identity,
            super::IngressStreamIdentity::Extension { .. }
                | super::IngressStreamIdentity::RoomResult { .. }
        ) || !matches!(
            submission.principal,
            super::IngressPrincipal::Extension(_) | super::IngressPrincipal::RoomResult(_)
        ) || !matches!(submission.connection_generation, TransportGeneration::Host)
            || submission.sender != continuation.host_sender
        {
            return NestedOutcome::Refused(NestedRefusal::InvalidHostSubmission);
        }
        let (decision_tx, decision_rx) = oneshot::channel();
        let (settlement_tx, settlement_rx) = oneshot::channel();
        work.push(tokio::spawn(async move {
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
                let terminal = match consume_host_frames(&authority, &continuation, &decision, &report).await {
                    Ok(()) => settle(&authority, &mut report).await,
                    Err(error) => Err(error.into()),
                };
                let _ = settlement_tx.send(SettlementOutcome {
                    rejection,
                    terminal,
                });
            }
            drop(permit);
        }));
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

/// Key the actual host consumption before the generic frame receipt. A crash
/// between these transactions retains acceptance in the existing send ledger.
async fn consume_host_frames(
    authority: &IngressAuthority,
    continuation: &NestedContinuation,
    decision: &super::IngressDecision,
    report: &super::ExecutionReport,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    use crate::ingress_uow::{CanonicalMessageRepository, IngressUowError};
    use crate::server::routes::interpret::{FullJidDeliveryOutcome, SmIngressAppendContext};
    if !report
        .frame_obligations
        .iter()
        .flat_map(|obligation| &obligation.frames)
        .any(|stanza| {
            matches!(stanza, Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat)
        })
    {
        return Ok(());
    }
    let Some(key) = decision.message_key else {
        return Ok(());
    };
    let mut tx = authority.uow.begin().await?;
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    tx.commit().await?;
    let Some(manifest) = envelope.room_manifest() else {
        return Ok(());
    };
    let consumer = Arc::new(HostFrameConsumer {
        sender: continuation.host_sender.clone(),
    });
    for obligation in &report.frame_obligations {
        for stanza in &obligation.frames {
            let Stanza::Message(message) = stanza else {
                continue;
            };
            if message.type_ != xmpp_parsers::message::MessageType::Groupchat {
                continue;
            }
            // Initial storage-hinted frames were already released through
            // the guarded transient sink; policy creates no keyed host retry.
            if obligation.receipt_keys.is_empty()
                && waddle_xmpp::xep::xep0334::should_skip_storage(message)
            {
                continue;
            }
            let target = message
                .to
                .as_ref()
                .and_then(|jid| jid.try_as_full().ok())
                .ok_or(IngressUowError::EffectIntentConflict)?;
            let receipt = obligation
                .receipt_keys
                .iter()
                .find(|receipt| manifest.contains(receipt, Some(target)))
                .ok_or(IngressUowError::EffectIntentConflict)?;
            super::room_manifest_replay::validate_child(&envelope, receipt, target, stanza)?;
            let context = SmIngressAppendContext {
                message_key: key,
                receipt: receipt.clone(),
                received_at: decision
                    .route_progress
                    .iter()
                    .find(|progress| &progress.receipt == receipt)
                    .and_then(|progress| progress.received_at),
                archive_positions: super::archive_dispatch::positions(&authority.uow, key, receipt)
                    .await?,
                dispatch_stream: None,
                authority: super::append_authority::AppendAuthority::Verified,
            };
            let owned_consumer = consumer.clone();
            let owned_target = target.clone();
            let owned_stanza = stanza.clone();
            let outcome = super::live_delivery::accept_live_delivery_guarded(
                &authority.uow,
                Some(&authority.force_stop),
                &context,
                target,
                stanza,
                Some(&continuation.state.deps.protocol.room_registry),
                move || owned_consumer.consume(&owned_target, &owned_stanza),
            )
            .await;
            if outcome != FullJidDeliveryOutcome::Delivered {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }
    }
    Ok(())
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
mod tests;
