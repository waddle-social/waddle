//! Recipient-local rediscovery of durable unoffered claims and unclaimed rows.
//!
//! The scheduler is bounded and round-robin. It never cancels a claim/offer
//! future to impose a timeout, and never creates a sleeping retry pump per tick.
//!
//! A committed offer reservation is not proof of transport delivery. Claims
//! already marked offered remain excluded even without an SM sequence: the
//! crash window between offer reservation and SM custody requires stronger
//! recovery evidence and is intentionally not resolved by this scanner.
use super::*;
use std::collections::BTreeMap;
use tokio::task::JoinHandle;

use crate::server::routes::websocket::WebSocketState;

const RECOVERY_CONCURRENCY: usize = 4;
const RECOVERY_PAGE_SIZE: usize = 64;

struct RecoveryTarget {
    resource: FullJid,
    owner: Arc<std::sync::atomic::AtomicBool>,
    session: Option<SmSessionId>,
    priority: i8,
}

impl RecoveryTarget {
    fn is_current(&self, registry: &ConnectionRegistry) -> bool {
        registry
            .entry_if_owner(&self.resource, &self.owner)
            .is_some_and(|entry| {
                entry.is_locally_hosted()
                    && entry.is_presence_available()
                    && entry.presence_priority() >= 0
            })
    }
}

struct RecoveryWork {
    // Retained retry custody pins the dependencies until reconciliation can
    // complete. An ordinary weak-state janitor exit cannot discard this work.
    state: Arc<WebSocketState>,
    target: RecoveryTarget,
    recipient: BareJid,
    after: Option<PendingRowId>,
    outcome: FlushOutcome,
}

impl RecoveryWork {
    async fn run(mut self, claimed_before_ms: i64) -> Self {
        let protocol = &self.state.deps.protocol;
        let storage = &protocol.pending_delivery_storage;
        if !self.outcome.has_retry_work() {
            if !self.target.is_current(&protocol.connection_registry) {
                return self;
            }
            match storage
                .list_unoffered_claims(
                    &self.recipient,
                    self.after.as_ref(),
                    claimed_before_ms,
                    RECOVERY_PAGE_SIZE,
                )
                .await
            {
                Ok(claims) => {
                    self.after = if claims.len() == RECOVERY_PAGE_SIZE {
                        claims.last().map(|claim| claim.row_id.clone())
                    } else {
                        None
                    };
                    for claim in claims {
                        if let Err(error) = storage
                            .release_unpushed_row_if_session(
                                &claim.row_id,
                                &claim.session,
                                &claim.token,
                            )
                            .await
                        {
                            // The durable unoffered marker survives the error;
                            // the row cursor advances so one bad prefix cannot
                            // starve later claims, and wraps to retry this row.
                            warn!(row_id = %claim.row_id, %error, "unoffered claim recovery release failed");
                        }
                    }
                }
                Err(error) => {
                    warn!(recipient = %self.recipient, %error, "unoffered claim recovery scan failed");
                    return self;
                }
            }
            // Independent of release events: a prior node might already have
            // released everything after this connection's empty first flush.
            match storage.list_unclaimed_after(&self.recipient, None, 1).await {
                Ok(rows) if rows.is_empty() => return self,
                Ok(_) => {}
                Err(error) => {
                    warn!(recipient = %self.recipient, %error, "unclaimed pending recovery probe failed");
                    return self;
                }
            }
        }
        let resolver = MamArchiveResolver {
            mam_storage: protocol.mam_storage.clone(),
        };
        self.outcome = super::flush::flush_recovery_pass(
            storage,
            &protocol.connection_registry,
            &self.recipient,
            &self.target.resource,
            FlushContext {
                server_domain: self.state.deps.auth_state.xmpp_domain.as_str(),
                sm_session: self.target.session.as_ref(),
                blocking_storage: Some(&protocol.blocking_storage),
                owner: Some(&self.target.owner),
                archive_resolver: &resolver,
                dispatch_gate: Some(protocol.ingress.as_ref()),
            },
            self.outcome,
        )
        .await;
        self
    }
}

/// Owned by the existing claim janitor for its lifetime. At most four jobs run
/// concurrently, with one job per recipient. Row cursor state is bounded by
/// currently eligible local recipients; unresolved offer custody is retained
/// even when its original resource disappears.
#[derive(Default)]
pub(crate) struct PendingRecovery {
    recipient_cursor: Option<BareJid>,
    row_cursors: BTreeMap<BareJid, PendingRowId>,
    running: BTreeMap<BareJid, JoinHandle<RecoveryWork>>,
    retry: BTreeMap<BareJid, RecoveryWork>,
}

impl PendingRecovery {
    pub(crate) async fn tick(&mut self, state: &Arc<WebSocketState>, claimed_before_ms: i64) {
        let completed: Vec<_> = self
            .running
            .iter()
            .filter(|(_, task)| task.is_finished())
            .map(|(jid, _)| jid.clone())
            .collect();
        for recipient in completed {
            let Some(task) = self.running.remove(&recipient) else {
                continue;
            };
            match task.await {
                Ok(work) => {
                    if let Some(after) = &work.after {
                        self.row_cursors.insert(recipient.clone(), after.clone());
                    } else {
                        self.row_cursors.remove(&recipient);
                    }
                    if work.outcome.has_retry_work() {
                        self.retry.insert(recipient, work);
                    }
                }
                Err(error) => warn!(%recipient, %error, "pending recovery worker failed"),
            }
        }
        let registry = &state.deps.protocol.connection_registry;
        let mut targets: BTreeMap<BareJid, RecoveryTarget> = BTreeMap::new();
        for resource in registry.list_connections() {
            let Some(entry) = registry.get_entry(&resource) else {
                continue;
            };
            if !entry.is_locally_hosted()
                || !entry.is_presence_available()
                || entry.presence_priority() < 0
            {
                continue;
            }
            let target = RecoveryTarget {
                resource: resource.clone(),
                owner: entry.carbons_handle(),
                session: entry.sm_stream_id(),
                priority: entry.presence_priority(),
            };
            let replace = targets.get(&resource.to_bare()).is_none_or(|previous| {
                target.priority > previous.priority
                    || (target.priority == previous.priority && target.resource < previous.resource)
            });
            if replace {
                targets.insert(resource.to_bare(), target);
            }
        }
        self.row_cursors
            .retain(|recipient, _| targets.contains_key(recipient));
        let mut recipients: Vec<_> = targets.keys().chain(self.retry.keys()).cloned().collect();
        recipients.sort();
        recipients.dedup();
        let split = self.recipient_cursor.as_ref().map_or(0, |cursor| {
            recipients.partition_point(|recipient| recipient <= cursor)
        });
        recipients.rotate_left(split);
        for recipient in recipients {
            if self.running.len() >= RECOVERY_CONCURRENCY {
                break;
            }
            self.recipient_cursor = Some(recipient.clone());
            if self.running.contains_key(&recipient) {
                continue;
            }
            let work = match self.retry.remove(&recipient) {
                Some(work) => work,
                None => {
                    let Some(target) = targets.remove(&recipient) else {
                        continue;
                    };
                    RecoveryWork {
                        state: state.clone(),
                        target,
                        recipient: recipient.clone(),
                        after: self.row_cursors.get(&recipient).cloned(),
                        outcome: FlushOutcome::default(),
                    }
                }
            };
            // Dropping a JoinHandle detaches rather than cancels the producer.
            // There is no timeout around its claim/reservation/offer sequence.
            self.running
                .insert(recipient, tokio::spawn(work.run(claimed_before_ms)));
        }
    }

    #[cfg(test)]
    pub(crate) async fn finish_current_pass(&mut self) {
        for task in self.running.values_mut() {
            // Tests await a finite pass, then tick again to collect its result.
            // Production never awaits all recipients behind a slow receiver.
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::registry::OutboundStanza;
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    use xmpp_parsers::message::Message;

    #[tokio::test]
    async fn full_receivers_do_not_starve_later_local_recipients() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage(
            Arc::new(InMemorySmSessionRegistry::new()), storage.clone(),
        ).await;
        let registry = &state.deps.protocol.connection_registry;
        let mut receivers = Vec::new();
        for index in 0..6 {
            let resource: FullJid = format!("recovery{index}@example.com/phone")
                .parse()
                .expect("fixture JID");
            let recipient = resource.to_bare();
            let message = Message::new(Some(recipient.clone().into()));
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            if index < RECOVERY_CONCURRENCY {
                sender
                    .try_send(OutboundStanza::new(waddle_xmpp::Stanza::Message(
                        message.clone(),
                    )))
                    .expect("fill slow receiver");
            }
            registry.register(resource.clone(), sender);
            registry.update_presence(&resource, true, 0);
            storage
                .insert(PendingRow {
                    id: PendingRowId::fresh(),
                    recipient,
                    original_receipt_at: chrono::Utc::now(),
                    payload: PendingPayload::Transient(Box::new(message)),
                    flushed_in_session: None,
                    outbound_sequence: None,
                })
                .await
                .expect("durable row after initial empty presence");
            // Rediscovery must not rely on resetting the first-presence CAS.
            assert!(registry
                .get_entry(&resource)
                .expect("entry")
                .claim_offline_flush());
            receivers.push(receiver);
        }
        let mut recovery = PendingRecovery::default();
        recovery.tick(&state, i64::MAX).await;
        assert_eq!(recovery.running.len(), RECOVERY_CONCURRENCY);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recovery.finish_current_pass(),
        )
        .await
        .expect("full receivers must not block recovery slots");
        recovery.tick(&state, i64::MAX).await;
        assert!(recovery.running.len() <= RECOVERY_CONCURRENCY);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recovery.finish_current_pass(),
        )
        .await
        .expect("next round remains bounded");
        for receiver in receivers.iter_mut().skip(RECOVERY_CONCURRENCY) {
            receiver
                .try_recv()
                .expect("round-robin reaches healthy recipient without new presence");
            assert!(receiver.try_recv().is_err(), "no duplicate offer");
        }
        for index in 0..RECOVERY_CONCURRENCY {
            let recipient: BareJid = format!("recovery{index}@example.com")
                .parse()
                .expect("fixture bare JID");
            let rows = storage
                .list(&recipient)
                .await
                .expect("slow receiver durable rows");
            assert_eq!(rows.len(), 1);
            assert!(
                rows[0].flushed_in_session.is_none(),
                "full capacity never commits an offer"
            );
        }
    }
}
