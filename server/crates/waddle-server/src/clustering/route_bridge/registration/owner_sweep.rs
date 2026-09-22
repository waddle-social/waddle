//! Owner mirrors outlive transport failures, but not committed node expiry.
use super::super::*;
use super::owner::{
    remote_owner_registration_matches, unregister_remote_owner_actor_entry,
    RemoteOwnerActorUnregisterOutcome,
};

const REMOTE_OWNER_SWEEP_LIMIT: usize = 64;
const REMOTE_OWNER_SWEEP_BUDGET: Duration = Duration::from_secs(30);
const REMOTE_OWNER_SWEEP_ENTRY_TIMEOUT: Duration = Duration::from_secs(5);

impl OrderedRelayDeliveryBridge {
    pub(super) async fn mark_remote_owner_unregister_pending(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        if let Some(current) = self.remote_owner_resources.lock().await.get_mut(jid) {
            if remote_owner_registration_matches(current, registration) {
                current.unregister_pending = true;
            }
        }
    }

    /// Retire a bounded, fair page of mirrors. Raw heartbeat age, draining,
    /// transport failures, and failed lease reads never prove a socket gone.
    /// Each finite round excludes new arrivals. Attempts rotate even on timeout
    /// so one blocked account cannot starve the rest. Cancellation preserves the
    /// exact registration for retry without skipping unattempted candidates.
    pub(crate) async fn sweep_remote_owner_resources(&self) -> bool {
        let Some(services) = self.services.get() else {
            return true;
        };
        let Ok(_sweep_guard) = self.remote_owner_sweep_lock.try_lock() else {
            return false;
        };
        let candidates = {
            let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                return false;
            };
            registrations.sweep_page(REMOTE_OWNER_SWEEP_LIMIT)
        };
        let deadline = tokio::time::Instant::now() + REMOTE_OWNER_SWEEP_BUDGET;
        let mut complete = true;
        for (jid, registration) in candidates {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            {
                let Ok(mut registrations) = self.remote_owner_resources.try_lock() else {
                    return false;
                };
                registrations.mark_sweep_attempt(&jid);
            }
            let timeout = REMOTE_OWNER_SWEEP_ENTRY_TIMEOUT.min(deadline - now);
            if !matches!(
                tokio::time::timeout(
                    timeout,
                    self.reconcile_remote_owner_registration(services, &jid, &registration)
                )
                .await,
                Ok(true)
            ) {
                complete = false;
                tracing::warn!(%jid, "owner mirror reconciliation remains pending");
            }
        }
        complete
    }

    async fn reconcile_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        if !registration.unregister_pending {
            match services
                .node_lease
                .unexpired_node_identity(&registration.socket_node)
                .await
            {
                Ok(Some(identity)) if identity == registration.socket_identity => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        // Recheck the full incarnation after the lease read. Registry cleanup
        // additionally compares the owner handle, preserving same-JID successors.
        let current = self.remote_owner_resources.lock().await.get(jid).cloned();
        if !current
            .as_ref()
            .is_some_and(|current| remote_owner_registration_matches(current, registration))
        {
            return true;
        }
        match unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
            RemoteOwnerActorUnregisterOutcome::Unregistered => {
                services
                    .connection_registry
                    .unregister_if_owner(jid, &registration.owner);
                self.remove_remote_owner_registration_if_current(jid, registration)
                    .await;
                true
            }
            RemoteOwnerActorUnregisterOutcome::RecordedRetry => {
                self.mark_remote_owner_unregister_pending(jid, registration)
                    .await;
                false
            }
            RemoteOwnerActorUnregisterOutcome::Failed => false,
        }
    }
}
