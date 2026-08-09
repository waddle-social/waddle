use super::super::delivery::receiver::{current_claim, user_entity};
use super::super::*;

fn remote_resource_busy_register_backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(50 * (attempt as u64 + 1))
}

fn remote_resource_register_status_is_retryable(
    status: RelayRemoteResourceRegistrationStatus,
) -> bool {
    matches!(
        status,
        RelayRemoteResourceRegistrationStatus::Busy
            | RelayRemoteResourceRegistrationStatus::StaleRegistration
    )
}

/// A relay reply timeout is ambiguous: the owner may still finish the
/// idempotent registration and publish `Registered` or `Busy`. Retrying the
/// same registration id is therefore safe and gives a deliberately-small
/// caller reply budget a chance to observe the typed result.
fn remote_resource_register_error_is_retryable(error: &RelayAskError) -> bool {
    matches!(
        error,
        RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            ..
        }
    )
}

/// The receiver may spend its bounded pending-unregister drain plus child
/// registration budget before replying. Raise only this idempotent register
/// ask to that minimum; other ordered-relay operations retain the deployment
/// configured reply timeout.
/// Consecutive owner-lookup misses (`RelayAskError::NotFound`) after which a
/// pending remote unregister obligation is treated as expired. Each miss
/// already spans `relay.rs`'s internal lookup backoff budget, and the streak
/// resets on any reply or non-lookup error, so a transient partition keeps
/// the obligation while a permanently gone owner incarnation stops accruing
/// map entries and relay traffic.
const REMOTE_UNREGISTER_OWNER_LOOKUP_MISS_TERMINAL_STREAK: u32 = 5;

fn remote_resource_register_reply_timeout(configured: std::time::Duration) -> std::time::Duration {
    configured.max(REMOTE_OWNER_REGISTER_REPLY_TIMEOUT)
}

/// Busy / reply-timeout retries stay bounded to TWO owner-handler windows.
/// One window alone made the retry advertisement hollow for the exact case
/// it exists for: a first attempt that consumed its full reply timeout left
/// zero budget, so the idempotent retry that would have observed a committed
/// `Registered` never ran and a valid cross-node resume rolled back. Two
/// windows guarantee one full re-ask after a worst-case first attempt while
/// keeping the caller bounded rather than an open-ended poller.
fn remote_resource_register_retry_budget(configured: std::time::Duration) -> std::time::Duration {
    remote_resource_register_reply_timeout(configured).saturating_mul(2)
}

async fn retry_remote_resource_register<F, Fut>(
    jid: &jid::FullJid,
    user_owner: &NodeId,
    retry_budget: std::time::Duration,
    mut register: F,
) -> Result<RelayRemoteResourceRegistrationReply, RelayAskError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<RelayRemoteResourceRegistrationReply, RelayAskError>>,
{
    let deadline = tokio::time::Instant::now() + retry_budget;
    for attempt in 0usize.. {
        match register().await {
            Ok(current) if remote_resource_register_status_is_retryable(current.status) => {
                let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
                else {
                    return Ok(current);
                };
                let backoff = remote_resource_busy_register_backoff(attempt).min(remaining);
                if backoff.is_zero() {
                    return Ok(current);
                }
                tracing::debug!(
                    jid = %jid,
                    owner_node = %user_owner.as_str(),
                    status = ?current.status,
                    "clustered remote-resource register returned a transient status; retrying idempotent registration"
                );
                tokio::time::sleep(backoff).await;
            }
            Ok(current) => return Ok(current),
            Err(error) if remote_resource_register_error_is_retryable(&error) => {
                let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
                else {
                    return Err(error);
                };
                let backoff = remote_resource_busy_register_backoff(attempt).min(remaining);
                if backoff.is_zero() {
                    return Err(error);
                }
                tracing::debug!(
                    jid = %jid,
                    owner_node = %user_owner.as_str(),
                    %error,
                    "clustered remote-resource register reply timed out; retrying idempotent registration"
                );
                tokio::time::sleep(backoff).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("remote-resource register retry loop always returns or times out")
}

#[cfg(test)]
pub(crate) async fn retry_remote_resource_register_test<F, Fut>(
    jid: &jid::FullJid,
    user_owner: &NodeId,
    retry_budget: std::time::Duration,
    register: F,
) -> Result<RelayRemoteResourceRegistrationReply, RelayAskError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<RelayRemoteResourceRegistrationReply, RelayAskError>>,
{
    retry_remote_resource_register(jid, user_owner, retry_budget, register).await
}

fn should_ack_remote_force_detach_before_cleanup(
    origin: waddle_xmpp::registry::ForceDetachOrigin,
) -> bool {
    matches!(
        origin,
        waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
    )
}

fn remote_resource_unregister_retry_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(200 * u64::from((attempt + 1).min(10)))
}

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn try_register_remote_user_resource(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        entry: ConnectionEntry,
        owner: Arc<AtomicBool>,
    ) -> RemoteResourceRegisterOutcome {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceRegisterOutcome::Failed;
        };
        let target_entity = user_entity(&jid.to_bare());
        let Some(target_snapshot) = current_claim(&services, &target_entity).await else {
            return RemoteResourceRegisterOutcome::NotRemote;
        };
        if !target_snapshot.owner_lease_fresh {
            return RemoteResourceRegisterOutcome::NotRemote;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return RemoteResourceRegisterOutcome::NotRemote;
        }

        let registration_id = RemoteResourceRegistrationId::fresh();
        let socket_generation = {
            let mut generations = self.remote_socket_generations.lock().await;
            let next = RemoteResourceSocketGeneration::next(generations.get(jid).copied());
            generations.insert(jid.clone(), next);
            next
        };
        let socket_node = NodeId::new(me.node_id.clone());
        let user_owner = NodeId::new(target_snapshot.owner.node_id.clone());
        let state = RemoteResourceStateSnapshot::from_entry(
            &entry,
            services.connection_registry.get_presence_state(jid),
        );
        let relay_stop_token = self.stop_token.clone();
        let relay_mailbox_timeout = self.mailbox_timeout;
        let relay_reply_timeout = remote_resource_register_reply_timeout(self.reply_timeout);
        let retry_budget = remote_resource_register_retry_budget(self.reply_timeout);
        let request = RelayRegisterRemoteUserResource {
            jid: jid.clone(),
            registration_id,
            socket_generation,
            socket_node,
            state,
            trace: RelayTraceContext::default(),
        };
        let reply = match retry_remote_resource_register(jid, &user_owner, retry_budget, || {
            let mut handle = RelayHandle::new(user_owner.clone(), relay_stop_token.clone())
                .with_ask_timeouts(relay_mailbox_timeout, relay_reply_timeout);
            let request = request.clone();
            async move { handle.register_remote_user_resource(request).await }
        })
        .await
        {
            Ok(reply) => reply,
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    owner_node = %user_owner.as_str(),
                    %error,
                    "clustered remote-resource register ask failed"
                );
                return RemoteResourceRegisterOutcome::Failed;
            }
        };
        match reply.status {
            RelayRemoteResourceRegistrationStatus::Registered => {
                if services
                    .connection_registry
                    .entry_if_owner(jid, &owner)
                    .is_none()
                {
                    let mut handle = RelayHandle::new(user_owner.clone(), self.stop_token.clone())
                        .with_ask_timeouts(relay_mailbox_timeout, relay_reply_timeout);
                    let _ = handle
                        .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                            jid: jid.clone(),
                            registration_id,
                            socket_generation,
                            trace: RelayTraceContext::default(),
                        })
                        .await;
                    return RemoteResourceRegisterOutcome::Failed;
                }
                self.remote_socket_resources.lock().await.insert(
                    jid.clone(),
                    RemoteSocketRegistration {
                        registration_id,
                        socket_generation,
                        owner,
                        user_owner,
                    },
                );
                RemoteResourceRegisterOutcome::Registered
            }
            RelayRemoteResourceRegistrationStatus::NotOwner => {
                RemoteResourceRegisterOutcome::NotRemote
            }
            RelayRemoteResourceRegistrationStatus::StaleRegistration
            | RelayRemoteResourceRegistrationStatus::Busy
            | RelayRemoteResourceRegistrationStatus::Unavailable => {
                RemoteResourceRegisterOutcome::Failed
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_if_owner(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) -> RemoteResourceUnregisterOutcome {
        let registration = {
            let mut registrations = self.remote_socket_resources.lock().await;
            match registrations.get(jid) {
                Some(registration) if Arc::ptr_eq(&registration.owner, owner) => {
                    registrations.remove(jid)
                }
                _ => None,
            }
        };
        let Some(registration) = registration else {
            return RemoteResourceUnregisterOutcome::NotRegistered;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(reply) => match reply.status {
                RelayRemoteResourceUnregisterStatus::Unregistered => {
                    RemoteResourceUnregisterOutcome::Unregistered
                }
                RelayRemoteResourceUnregisterStatus::RecordedRetry => {
                    RemoteResourceUnregisterOutcome::RecordedRetry
                }
                RelayRemoteResourceUnregisterStatus::NotRegistered => {
                    RemoteResourceUnregisterOutcome::NotRegistered
                }
                RelayRemoteResourceUnregisterStatus::Failed => {
                    self.record_pending_remote_socket_unregister(jid, &registration)
                        .await;
                    RemoteResourceUnregisterOutcome::Failed
                }
            },
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    %error,
                    "clustered remote-resource unregister ask failed; no remote cleanup proof"
                );
                self.record_pending_remote_socket_unregister(jid, &registration)
                    .await;
                RemoteResourceUnregisterOutcome::Failed
            }
        }
    }

    async fn record_pending_remote_socket_unregister(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let pending = PendingRemoteSocketUnregister {
            key: PendingRemoteSocketUnregisterKey {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
            },
            user_owner: registration.user_owner.clone(),
        };
        let should_spawn = {
            let mut pending_unregistrations =
                self.pending_remote_socket_unregistrations.lock().await;
            pending_unregistrations
                .insert(pending.key.clone(), pending.clone())
                .is_none()
        };
        if !should_spawn {
            return;
        }
        tracing::warn!(
            jid = %pending.key.jid,
            owner_node = %pending.user_owner.as_str(),
            "clustered remote-resource unregister lacked cleanup proof; queued sender-side retry"
        );
        let bridge = Arc::clone(self);
        tokio::spawn(async move {
            bridge.retry_pending_remote_socket_unregister(pending).await;
        });
    }

    /// Durable death proof for a pending unregister's owner reference: the
    /// user's claim row is authoritative. No claim, a stale lease, or a
    /// different owner incarnation all mean the referenced owner can no
    /// longer hold this user's actor/mirror (claims are released on actor
    /// teardown and node ids never survive a restart), so the obligation is
    /// moot. A fresh claim held by the SAME incarnation means the owner is
    /// alive behind a partition — the obligation must be retained. A store
    /// error proves nothing and retains the obligation.
    async fn pending_remote_unregister_owner_is_provably_gone(
        &self,
        pending: &PendingRemoteSocketUnregister,
    ) -> bool {
        let Some(services) = self.services.get().cloned() else {
            return false;
        };
        let entity = user_entity(&pending.key.jid.to_bare());
        match services.claim_store.current_claim(&entity).await {
            Ok(None) => true,
            Ok(Some(snapshot)) => {
                !snapshot.owner_lease_fresh || snapshot.owner.node_id != pending.user_owner.as_str()
            }
            Err(error) => {
                tracing::warn!(
                    jid = %pending.key.jid,
                    owner_node = %pending.user_owner.as_str(),
                    %error,
                    "clustered remote-resource unregister: claim liveness check failed; \
                     retaining the obligation"
                );
                false
            }
        }
    }

    async fn clear_pending_remote_socket_unregister_if_current(
        &self,
        pending: &PendingRemoteSocketUnregister,
    ) {
        let mut pending_unregistrations = self.pending_remote_socket_unregistrations.lock().await;
        if pending_unregistrations
            .get(&pending.key)
            .is_some_and(|current| current.user_owner == pending.user_owner)
        {
            pending_unregistrations.remove(&pending.key);
        }
    }

    async fn retry_pending_remote_socket_unregister(
        self: Arc<Self>,
        pending: PendingRemoteSocketUnregister,
    ) {
        let mut attempt = 0u32;
        let mut consecutive_owner_lookup_misses = 0u32;
        loop {
            let mut handle = RelayHandle::new(pending.user_owner.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
            let result = handle
                .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                    jid: pending.key.jid.clone(),
                    registration_id: pending.key.registration_id,
                    socket_generation: pending.key.socket_generation,
                    trace: RelayTraceContext::default(),
                })
                .await;
            match result {
                Ok(reply)
                    if matches!(
                        reply.status,
                        RelayRemoteResourceUnregisterStatus::Unregistered
                            | RelayRemoteResourceUnregisterStatus::RecordedRetry
                            | RelayRemoteResourceUnregisterStatus::NotRegistered
                    ) =>
                {
                    self.clear_pending_remote_socket_unregister_if_current(&pending)
                        .await;
                    return;
                }
                Ok(reply) => {
                    consecutive_owner_lookup_misses = 0;
                    tracing::warn!(
                        jid = %pending.key.jid,
                        owner_node = %pending.user_owner.as_str(),
                        status = ?reply.status,
                        "clustered remote-resource unregister retry still lacks cleanup proof"
                    );
                }
                Err(error) => {
                    if ask_error_definitively_proves_remote_resource_ref_stale(&error) {
                        self.clear_pending_remote_socket_unregister_if_current(&pending)
                            .await;
                        return;
                    }
                    // A single lookup miss is ambiguous (transient partition
                    // or Kademlia hiccup), so it must keep the obligation. A
                    // long consecutive streak escalates to the durable claim
                    // store for an actual liveness verdict instead of guessing
                    // from resolution failures alone: a partition can outlast
                    // any fixed miss count while the owner stays alive.
                    if ask_error_is_owner_lookup_miss(&error) {
                        consecutive_owner_lookup_misses =
                            consecutive_owner_lookup_misses.saturating_add(1);
                        if consecutive_owner_lookup_misses
                            >= REMOTE_UNREGISTER_OWNER_LOOKUP_MISS_TERMINAL_STREAK
                        {
                            consecutive_owner_lookup_misses = 0;
                            if self
                                .pending_remote_unregister_owner_is_provably_gone(&pending)
                                .await
                            {
                                tracing::info!(
                                    jid = %pending.key.jid,
                                    owner_node = %pending.user_owner.as_str(),
                                    "clustered remote-resource unregister: durable claim state \
                                     proves the owner incarnation no longer holds this user; \
                                     clearing the expired obligation"
                                );
                                self.clear_pending_remote_socket_unregister_if_current(&pending)
                                    .await;
                                return;
                            }
                        }
                    } else {
                        consecutive_owner_lookup_misses = 0;
                    }
                    tracing::warn!(
                        jid = %pending.key.jid,
                        owner_node = %pending.user_owner.as_str(),
                        %error,
                        "clustered remote-resource unregister retry ask failed"
                    );
                }
            }
            attempt = attempt.saturating_add(1);
            tokio::select! {
                _ = self.stop_token.cancelled() => {
                    self.clear_pending_remote_socket_unregister_if_current(&pending).await;
                    return;
                }
                _ = tokio::time::sleep(remote_resource_unregister_retry_backoff(attempt)) => {}
            }
        }
    }

    pub(crate) async fn update_remote_user_resource_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
        update: RemoteResourceStateUpdate,
    ) {
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(jid)
                .filter(|registration| Arc::ptr_eq(&registration.owner, owner))
                .cloned()
        };
        let Some(registration) = registration else {
            return;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .update_remote_user_resource(RelayUpdateRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                update,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::Updated,
            }) => {}
            Ok(RelayRemoteResourceUpdateReply { status }) => {
                tracing::warn!(
                    jid = %jid,
                    status = ?status,
                    "clustered remote-resource state update failed closed; detaching socket"
                );
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    %error,
                    "clustered remote-resource state update ask failed; detaching socket"
                );
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
            }
        }
    }

    pub(super) async fn remote_socket_registration_if_current(
        &self,
        jid: &jid::FullJid,
    ) -> Option<RemoteSocketRegistration> {
        let registration = self
            .remote_socket_resources
            .lock()
            .await
            .get(jid)
            .cloned()?;
        let services = self.services.get()?;
        services
            .connection_registry
            .entry_if_owner(jid, &registration.owner)
            .map(|_| registration)
    }

    pub(super) async fn remove_remote_socket_registration_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let mut registrations = self.remote_socket_resources.lock().await;
        if registrations.get(jid).is_some_and(|current| {
            current.registration_id == registration.registration_id
                && current.socket_generation == registration.socket_generation
                && current.user_owner == registration.user_owner
                && Arc::ptr_eq(&current.owner, &registration.owner)
        }) {
            registrations.remove(jid);
        }
    }

    pub(in super::super) async fn remove_remote_socket_registration_if_snapshot(
        &self,
        remote_origin: &RemoteResourceOriginSnapshot,
        owner: &Arc<AtomicBool>,
    ) {
        let mut registrations = self.remote_socket_resources.lock().await;
        if registrations
            .get(&remote_origin.jid)
            .is_some_and(|registration| {
                registration.registration_id == remote_origin.registration_id
                    && registration.socket_generation == remote_origin.socket_generation
                    && Arc::ptr_eq(&registration.owner, owner)
            })
        {
            registrations.remove(&remote_origin.jid);
        }
    }

    pub(crate) async fn force_detach_remote_user_resource_on_socket(
        &self,
        msg: RelayForceDetachRemoteUserResource,
    ) -> RelayForceDetachRemoteUserResourceReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            };
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.jid)
                .filter(|registration| registration.registration_id == msg.registration_id)
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            };
        };
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(&msg.jid, &registration.owner)
        else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            };
        };
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let request = ForceDetachRequest {
            origin: msg.origin,
            requester_bare_jid: msg.requester_bare_jid,
            ack,
        };
        if entry.force_detach_sender().try_send(request).is_err() {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            };
        }
        if should_ack_remote_force_detach_before_cleanup(msg.origin) {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Detached,
            };
        }
        let (outcome, status) =
            match tokio::time::timeout(ORDERED_DELIVERY_REPLY_TIMEOUT, ack_rx).await {
                Ok(Ok(ForceDetachOutcome::Detached)) => (
                    ForceDetachOutcome::Detached,
                    RelayRemoteResourceForceDetachStatus::Detached,
                ),
                Ok(Ok(ForceDetachOutcome::NotPersisted)) => (
                    ForceDetachOutcome::NotPersisted,
                    RelayRemoteResourceForceDetachStatus::Detached,
                ),
                Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => (
                    ForceDetachOutcome::IdentityMismatch,
                    RelayRemoteResourceForceDetachStatus::Refused,
                ),
                Ok(Err(_)) | Err(_) => (
                    ForceDetachOutcome::NotPersisted,
                    RelayRemoteResourceForceDetachStatus::Unknown,
                ),
            };
        RelayForceDetachRemoteUserResourceReply { outcome, status }
    }

    pub(super) async fn detach_stale_remote_socket_resource(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        {
            let mut registrations = self.remote_socket_resources.lock().await;
            if registrations.get(jid).is_some_and(|current| {
                current.registration_id == registration.registration_id
                    && current.socket_generation == registration.socket_generation
            }) {
                registrations.remove(jid);
            }
        }
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(jid, &registration.owner)
        else {
            return;
        };
        let (ack, _ack_rx) = tokio::sync::oneshot::channel();
        let _ = entry.force_detach_sender().try_send(ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
            requester_bare_jid: jid.to_bare(),
            ack,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::route_bridge::tests::{
        origin_identity, receiver_identity, services_with_claims, test_peer_id,
    };
    use crate::config::REMOTE_OWNER_REGISTER_POST_REGISTRATION_REPLY_TIMEOUT;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn reply_timeout_retries_the_idempotent_remote_registration() {
        let timeout = RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply timeout".to_string(),
        };
        assert!(remote_resource_register_error_is_retryable(&timeout));

        let transport = RelayAskError::Send {
            failure: RelaySendFailure::Transport,
            effect: RelaySendEffect::MaybeCommitted,
            message: "connection closed".to_string(),
        };
        assert!(!remote_resource_register_error_is_retryable(&transport));
    }

    #[tokio::test]
    async fn busy_retirement_retries_until_the_idempotent_registration_succeeds() {
        let attempts = AtomicUsize::new(0);
        let jid = "alice@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        let owner = NodeId::new("owner-node".to_string());

        let reply = retry_remote_resource_register(
            &jid,
            &owner,
            std::time::Duration::from_millis(200),
            || async {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                Ok(RelayRemoteResourceRegistrationReply {
                    status: if attempt < 2 {
                        RelayRemoteResourceRegistrationStatus::Busy
                    } else {
                        RelayRemoteResourceRegistrationStatus::Registered
                    },
                })
            },
        )
        .await
        .expect("bounded Busy retry should observe the eventual registration");

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stale_registration_retry_is_bounded() {
        let attempts = AtomicUsize::new(0);
        let jid = "alice@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        let owner = NodeId::new("owner-node".to_string());

        let reply = retry_remote_resource_register(
            &jid,
            &owner,
            std::time::Duration::from_millis(200),
            || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                })
            },
        )
        .await
        .expect("bounded stale-registration retry returns the final typed status");

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::StaleRegistration
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            4,
            "the bounded retry budget should retry until its 200ms window expires"
        );
    }

    #[test]
    fn remote_registration_reply_timeout_covers_the_owner_handler_bound() {
        assert_eq!(
            remote_resource_register_reply_timeout(std::time::Duration::from_secs(1)),
            REMOTE_OWNER_REGISTER_REPLY_TIMEOUT
        );
        assert_eq!(
            REMOTE_OWNER_REGISTER_REPLY_TIMEOUT,
            crate::config::ORDERED_RELAY_MAILBOX_TIMEOUT
                .saturating_add(REMOTE_OWNER_REGISTER_USER_REGISTRY_REPLY_TIMEOUT)
                .saturating_add(REMOTE_OWNER_REGISTER_POST_REGISTRATION_REPLY_TIMEOUT),
            "the floor must include the nested registry admission mailbox window \
             in addition to the admission-ask reply and post-registration budgets"
        );
        assert_eq!(
            remote_resource_register_reply_timeout(std::time::Duration::from_secs(20)),
            std::time::Duration::from_secs(20),
            "a configured reply timeout above the floor is respected"
        );
    }

    #[test]
    fn stale_remote_unregister_errors_are_cleanup_proof() {
        let not_found = RelayAskError::NotFound {
            node_id: NodeId::new("expired-owner".to_string()),
        };
        assert!(
            ask_error_proves_remote_resource_ref_stale(&not_found),
            "replacement-supersedes callers may treat a lookup miss as uncommitted"
        );
        assert!(
            !ask_error_definitively_proves_remote_resource_ref_stale(&not_found),
            "a lookup miss can be a transient partition; the unregister retry loop \
             must not clear its durable obligation on one occurrence"
        );
        assert!(ask_error_is_owner_lookup_miss(&not_found));

        let stale_ref = RelayAskError::Send {
            failure: RelaySendFailure::StaleRef,
            effect: RelaySendEffect::NoEffect,
            message: "actor stopped".to_string(),
        };
        assert!(ask_error_proves_remote_resource_ref_stale(&stale_ref));
        assert!(ask_error_definitively_proves_remote_resource_ref_stale(
            &stale_ref
        ));

        let reply_timeout = RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply timeout".to_string(),
        };
        assert!(!ask_error_proves_remote_resource_ref_stale(&reply_timeout));
        assert!(!ask_error_definitively_proves_remote_resource_ref_stale(
            &reply_timeout
        ));
    }

    /// A permanently unresolvable owner clears the obligation only once the
    /// consecutive lookup-miss streak escalates to the durable claim store
    /// and that store PROVES the owner incarnation no longer holds the user
    /// (here: no claim row exists at all). Without wired services no proof
    /// is possible and the obligation is retained by design.
    #[tokio::test(start_paused = true)]
    async fn proof_stale_unregister_retry_clears_the_pending_obligation() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let pending = PendingRemoteSocketUnregister {
            key: PendingRemoteSocketUnregisterKey {
                jid: "expired@example.test/phone"
                    .parse::<jid::FullJid>()
                    .expect("valid full jid"),
                registration_id: RemoteResourceRegistrationId::fresh(),
                socket_generation: RemoteResourceSocketGeneration::next(None),
            },
            user_owner: NodeId::new("expired-owner".to_string()),
        };
        bridge
            .pending_remote_socket_unregistrations
            .lock()
            .await
            .insert(pending.key.clone(), pending.clone());

        let retry = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let pending = pending.clone();
            async move {
                bridge.retry_pending_remote_socket_unregister(pending).await;
            }
        });

        tokio::time::timeout(std::time::Duration::from_secs(120), retry)
            .await
            .expect("streak-terminal retry should stop once the owner reference is gone")
            .expect("retry task joins");

        assert!(
            bridge
                .pending_remote_socket_unregistrations
                .lock()
                .await
                .is_empty(),
            "proof-stale retries should clear the pending unregister"
        );
    }

    #[test]
    fn only_stale_retirement_is_acked_before_cleanup() {
        assert!(should_ack_remote_force_detach_before_cleanup(
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        ));
        assert!(!should_ack_remote_force_detach_before_cleanup(
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement
        ));
        assert!(!should_ack_remote_force_detach_before_cleanup(
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
        ));
    }

    #[tokio::test]
    async fn stale_remote_force_detach_is_acked_without_waiting_for_connection_ack() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let jid = "stale@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        services
            .connection_registry
            .register_entry(jid.clone(), entry.clone());
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("force-detach receiver available");
        let registration_id = RemoteResourceRegistrationId::fresh();
        bridge.remote_socket_resources.lock().await.insert(
            jid.clone(),
            RemoteSocketRegistration {
                registration_id,
                socket_generation: RemoteResourceSocketGeneration::next(None),
                owner: Arc::clone(&owner),
                user_owner: NodeId::new("owner-node".to_string()),
            },
        );

        let reply = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            bridge.force_detach_remote_user_resource_on_socket(
                RelayForceDetachRemoteUserResource {
                    jid: jid.clone(),
                    registration_id,
                    origin: waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
                    requester_bare_jid: jid.to_bare(),
                    trace: RelayTraceContext::default(),
                },
            ),
        )
        .await
        .expect("stale retirement should be acknowledged promptly");

        assert_eq!(reply.outcome, ForceDetachOutcome::NotPersisted);
        assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Detached);

        let request = force_detach_rx
            .recv()
            .await
            .expect("forwarded force-detach");
        assert_eq!(
            request.origin,
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        );
    }

    #[tokio::test]
    async fn owner_managed_remote_force_detach_still_waits_for_connection_ack() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let jid = "owner-managed@example.test/laptop"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        services
            .connection_registry
            .register_entry(jid.clone(), entry.clone());
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("force-detach receiver available");
        let registration_id = RemoteResourceRegistrationId::fresh();
        bridge.remote_socket_resources.lock().await.insert(
            jid.clone(),
            RemoteSocketRegistration {
                registration_id,
                socket_generation: RemoteResourceSocketGeneration::next(None),
                owner,
                user_owner: NodeId::new("owner-node".to_string()),
            },
        );

        let reply_task = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let jid = jid.clone();
            async move {
                bridge
                    .force_detach_remote_user_resource_on_socket(
                        RelayForceDetachRemoteUserResource {
                            jid,
                            registration_id,
                            origin:
                                waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
                            requester_bare_jid: "owner-managed@example.test"
                                .parse()
                                .expect("valid bare jid"),
                            trace: RelayTraceContext::default(),
                        },
                    )
                    .await
            }
        });

        let request = force_detach_rx
            .recv()
            .await
            .expect("forwarded force-detach");
        assert_eq!(
            request.origin,
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement
        );
        let _ = request.ack.send(ForceDetachOutcome::Detached);

        let reply = reply_task.await.expect("reply task joins");
        assert_eq!(reply.outcome, ForceDetachOutcome::Detached);
        assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Detached);
    }
}
