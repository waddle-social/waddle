//! Reconcile routing mirrors using committed expiry or exact-resource absence.
use super::super::{
    Duration, HashMap, LocalResourcePresence, NodeId, NodeIdentity, OrderedRelayDeliveryBridge,
    OrderedRelayDeliveryServices, RemoteOwnerRegistration,
};
use super::owner::remote_owner_registration_matches;
use crate::clustering::resource_presence::{peer_resource_reachability, PeerResourceReachability};
use waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure;
use waddle_xmpp::registry::{UnregisterAndReleaseIfEmpty, UnregisterAndReleaseOutcome};
use waddle_xmpp::telemetry::attributes::{RemoteOwnerMirrorOutcome, SweepOutcome};
use waddle_xmpp::telemetry::remote_owner::{record_mirror_attempt, record_mirror_backlog};

const REMOTE_OWNER_SWEEP_LIMIT: usize = 64;
const REMOTE_OWNER_SWEEP_BUDGET: Duration = Duration::from_secs(30);
const REMOTE_OWNER_SWEEP_ENTRY_TIMEOUT: Duration = Duration::from_secs(5);

impl OrderedRelayDeliveryBridge {
    pub(in super::super) async fn mark_remote_owner_unregister_pending(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let mut registrations = self.remote_owner_resources.lock().await;
        if let Some(current) = registrations.get_mut(jid) {
            if remote_owner_registration_matches(current, registration) {
                current.unregister_pending = true;
                registrations.mark_pending(jid);
            }
        }
    }

    /// Retire a bounded, fair page of mirrors. Raw heartbeat age, draining,
    /// transport failures, and failed lease reads never prove a socket gone.
    /// Each finite round excludes new arrivals. Attempts rotate even on timeout
    /// so one blocked account cannot starve the rest. Cancellation preserves the
    /// exact registration for retry without skipping unattempted candidates.
    pub(crate) async fn sweep_remote_owner_resources(&self) -> SweepOutcome {
        let Some(services) = self.services.get() else {
            return SweepOutcome::Deferred;
        };
        let Ok(_sweep_guard) = self.remote_owner_sweep_lock.try_lock() else {
            return SweepOutcome::Deferred;
        };
        let candidates = {
            let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                return SweepOutcome::Deferred;
            };
            registrations.sweep_page(REMOTE_OWNER_SWEEP_LIMIT)
        };
        let deadline = tokio::time::Instant::now() + REMOTE_OWNER_SWEEP_BUDGET;
        // One committed-node read per socket per page, including failed reads.
        // Never retain this cache across ticks: a healthy socket may expire
        // between pages, and a missing row may be replaced with a new epoch.
        let mut leases: HashMap<NodeId, Result<Option<NodeIdentity>, RemoteOwnerMirrorOutcome>> =
            HashMap::new();
        let mut outcome = SweepOutcome::Completed;
        for (jid, registration) in candidates {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                if outcome != SweepOutcome::Failed {
                    outcome = SweepOutcome::Deferred;
                }
                break;
            }
            {
                let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                    if outcome != SweepOutcome::Failed {
                        outcome = SweepOutcome::Deferred;
                    }
                    break;
                };
                registrations.mark_sweep_attempt(&jid);
            }
            let dependency_deadline = now + REMOTE_OWNER_SWEEP_ENTRY_TIMEOUT;
            let entry_deadline = deadline.min(dependency_deadline);
            let timeout_outcome = if deadline < dependency_deadline {
                RemoteOwnerMirrorOutcome::Deferred
            } else {
                RemoteOwnerMirrorOutcome::Failed
            };
            let attempt = if registration.unregister_pending {
                self.retire_remote_owner_mirror(
                    services,
                    &jid,
                    &registration,
                    entry_deadline,
                    timeout_outcome,
                )
                .await
            } else {
                let lease = if let Some(cached) = leases.get(&registration.socket_node) {
                    cached.clone()
                } else {
                    let lease = match tokio::time::timeout_at(
                        entry_deadline,
                        services
                            .node_lease
                            .unexpired_node_identity(&registration.socket_node),
                    )
                    .await
                    {
                        Ok(Ok(identity)) => Ok(identity),
                        Ok(Err(error)) => {
                            tracing::warn!(socket_node = %registration.socket_node, ?error, "owner mirror lease read failed");
                            Err(RemoteOwnerMirrorOutcome::Failed)
                        }
                        Err(_) => {
                            if timeout_outcome == RemoteOwnerMirrorOutcome::Failed {
                                tracing::warn!(socket_node = %registration.socket_node, "owner mirror lease read timed out");
                            }
                            Err(timeout_outcome)
                        }
                    };
                    leases.insert(registration.socket_node.clone(), lease.clone());
                    lease
                };
                match lease {
                    Ok(Some(identity))
                        if identity == registration.socket_identity
                            && waddle_xmpp::ownership::NodeLeaseIdentity::from_node_identity(
                                &identity,
                            ) == *registration.connection_ref.node_lease() =>
                    {
                        self.reconcile_healthy_remote_owner_mirror(
                            services,
                            &jid,
                            &registration,
                            entry_deadline,
                            timeout_outcome,
                        )
                        .await
                    }
                    Ok(_) => {
                        self.retire_remote_owner_mirror(
                            services,
                            &jid,
                            &registration,
                            entry_deadline,
                            timeout_outcome,
                        )
                        .await
                    }
                    Err(outcome) => outcome,
                }
            };
            record_mirror_attempt(attempt);
            match attempt {
                RemoteOwnerMirrorOutcome::Failed => outcome = SweepOutcome::Failed,
                RemoteOwnerMirrorOutcome::Deferred if outcome != SweepOutcome::Failed => {
                    outcome = SweepOutcome::Deferred
                }
                _ => {}
            }
        }
        if let Ok(registrations) = self.remote_owner_resources.try_lock() {
            let (inventory, pending, age) = registrations.sweep_backlog();
            record_mirror_backlog(inventory, pending, age);
        }
        outcome
    }

    async fn reconcile_healthy_remote_owner_mirror(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        deadline: tokio::time::Instant,
        timeout_outcome: RemoteOwnerMirrorOutcome,
    ) -> RemoteOwnerMirrorOutcome {
        let (Some(membership), Some(presence)) = (
            services.cluster_membership.as_ref(),
            services.resource_presence.as_ref(),
        ) else {
            return RemoteOwnerMirrorOutcome::Deferred;
        };
        match tokio::time::timeout_at(deadline, async {
            // Mirrors never prove liveness. Ask about hosted sockets and
            // resumable sessions here and on every unexpired cluster peer.
            if self.resource_presence_local(jid).await == LocalResourcePresence::Present {
                return RemoteOwnerMirrorOutcome::Live;
            }
            match peer_resource_reachability(
                membership.as_ref(),
                presence.as_ref(),
                jid,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await
            {
                PeerResourceReachability::AbsentOnEveryPeer => {
                    self.retire_remote_owner_mirror(
                        services,
                        jid,
                        registration,
                        deadline,
                        timeout_outcome,
                    )
                    .await
                }
                PeerResourceReachability::PresentOnAPeer => RemoteOwnerMirrorOutcome::Live,
                PeerResourceReachability::Unproven => RemoteOwnerMirrorOutcome::Deferred,
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => timeout_outcome,
        }
    }

    async fn retire_remote_owner_mirror(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        deadline: tokio::time::Instant,
        timeout_outcome: RemoteOwnerMirrorOutcome,
    ) -> RemoteOwnerMirrorOutcome {
        let lock =
            match tokio::time::timeout_at(deadline, self.lock_for_remote_owner_registration(jid))
                .await
            {
                Ok(Some(lock)) => lock,
                Ok(None) => return RemoteOwnerMirrorOutcome::Deferred,
                Err(_) => return timeout_outcome,
            };
        let result = tokio::time::timeout_at(deadline, async {
            let Ok(_registration_guard) = lock.try_lock() else {
                return RemoteOwnerMirrorOutcome::Deferred;
            };
            // A successor may register while the absence probe is in flight.
            // Keep its registration/owner fences, but do not require current
            // publication authority to remove an obsolete local routing copy.
            // The registry releases only its captured claim owner and epoch.
            {
                let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                    return RemoteOwnerMirrorOutcome::Deferred;
                };
                if !registrations
                    .get(jid)
                    .is_some_and(|current| remote_owner_registration_matches(current, registration))
                {
                    return RemoteOwnerMirrorOutcome::Superseded;
                }
                // Record the owed cleanup before awaiting its actor: a cancelled
                // ask may commit later, and its pending age must survive retries.
                if let Some(current) = registrations.get_mut(jid) {
                    current.unregister_pending = true;
                }
                registrations.mark_pending(jid);
            }
            // The janitor itself is the retry loop. One typed registry ask
            // preserves Busy as expected deferral instead of timing out a
            // sequence of synchronous busy retries. The registry records owed
            // unregister work, and the mirror inventory also survives cancellation.
            match services
                .user_registry
                .ask(UnregisterAndReleaseIfEmpty {
                    jid: jid.clone(),
                    owner: Some(registration.owner),
                })
                // The outer deadline bounds enqueue plus reply. A 2s parent
                // reply timeout would race the child's own 2s Busy result.
                .await
            {
                Ok(
                    UnregisterAndReleaseOutcome::Released
                    | UnregisterAndReleaseOutcome::RetainedLiveResources
                    | UnregisterAndReleaseOutcome::AlreadyAbsent,
                ) => {
                    services
                        .connection_registry
                        .unregister_if_owner(jid, &registration.owner);
                    let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                        // The actor operation completed; retry only inventory
                        // cleanup when its lock is available on a later page.
                        return RemoteOwnerMirrorOutcome::Deferred;
                    };
                    if registrations.get(jid).is_some_and(|current| {
                        remote_owner_registration_matches(current, registration)
                    }) {
                        registrations.remove(jid);
                    }
                    RemoteOwnerMirrorOutcome::Retired
                }
                Ok(UnregisterAndReleaseOutcome::RetryableFailure(
                    UnregisterAndReleaseRetryableFailure::UserActorBusy,
                )) => RemoteOwnerMirrorOutcome::Deferred,
                Ok(UnregisterAndReleaseOutcome::RetryableFailure(
                    UnregisterAndReleaseRetryableFailure::UserActorStateLost,
                )) => RemoteOwnerMirrorOutcome::Failed,
                Err(error) => {
                    tracing::warn!(%jid, ?error, "owner mirror retirement ask failed");
                    RemoteOwnerMirrorOutcome::Failed
                }
            }
        })
        .await;
        // Best-effort bookkeeping shares the entry budget. The bounded map
        // also prunes unused locks before admitting a new key at capacity.
        let _ = tokio::time::timeout_at(
            deadline,
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock),
        )
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(_) => {
                if timeout_outcome == RemoteOwnerMirrorOutcome::Failed {
                    tracing::warn!(%jid, "owner mirror retirement timed out");
                }
                timeout_outcome
            }
        }
    }
}
