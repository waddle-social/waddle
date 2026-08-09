use super::super::delivery::receiver::{current_claim, user_entity};
use super::super::*;
use super::owner_update::remote_owner_registration_is_current;

pub(in super::super) use super::owner_update::owner_remote_entry_if_current;

const REMOTE_RESOURCE_BUSY_UNREGISTER_ATTEMPTS: usize = 3;

fn remote_resource_busy_unregister_backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(50 * (attempt as u64 + 1))
}

/// Map an owner-local registry ask failure onto the relay register reply.
///
/// `UserActorBusy` is the established prompt transient. `ClaimUnavailable` is
/// equally transient during cross-node resume: the old node can time out its
/// exact claim DELETE and inventory it, and the janitor converges the release
/// moments later — replying Busy lets the socket's existing bounded
/// idempotent retry re-attempt after the release lands instead of rolling
/// back a valid resumed session. (A claim genuinely held by another live node
/// retries briefly and then fails through the same bounded budget.) Every
/// other failure is terminal `Unavailable`.
fn registration_status_for_owner_register_error(
    error: &kameo::error::SendError<
        waddle_xmpp::registry::RegisterUserResourceIfOwnerOrAbsent,
        waddle_xmpp::registry::UserRegistryError,
    >,
) -> RelayRemoteResourceRegistrationStatus {
    match error {
        kameo::error::SendError::HandlerError(
            waddle_xmpp::registry::UserRegistryError::UserActorBusy(_)
            | waddle_xmpp::registry::UserRegistryError::ClaimUnavailable(_),
        ) => RelayRemoteResourceRegistrationStatus::Busy,
        _ => RelayRemoteResourceRegistrationStatus::Unavailable,
    }
}

impl OrderedRelayDeliveryBridge {
    async fn schedule_remote_owner_registration_retirement(
        self: &Arc<Self>,
        services: &Arc<OrderedRelayDeliveryServices>,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let should_spawn = {
            let mut pending = self.pending_remote_owner_retirements.lock().await;
            if pending
                .get(jid)
                .is_some_and(|current| remote_owner_registration_matches(current, registration))
            {
                false
            } else {
                pending.insert(jid.clone(), registration.clone());
                true
            }
        };
        if !should_spawn {
            return;
        }

        let bridge = Arc::clone(self);
        let services = Arc::clone(services);
        let jid = jid.clone();
        let registration = registration.clone();
        tokio::spawn(async move {
            bridge
                .complete_pending_remote_owner_registration_retirement(services, jid, registration)
                .await;
        });
    }

    async fn clear_pending_remote_owner_registration_retirement_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let mut pending = self.pending_remote_owner_retirements.lock().await;
        if pending
            .get(jid)
            .is_some_and(|current| remote_owner_registration_matches(current, registration))
        {
            pending.remove(jid);
        }
    }

    async fn complete_pending_remote_owner_registration_retirement(
        self: Arc<Self>,
        services: Arc<OrderedRelayDeliveryServices>,
        jid: jid::FullJid,
        registration: RemoteOwnerRegistration,
    ) {
        let Some(lock) = self.lock_for_remote_owner_registration(&jid).await else {
            self.clear_pending_remote_owner_registration_retirement_if_current(&jid, &registration)
                .await;
            return;
        };
        let guard = lock.lock().await;

        let current = self.remote_owner_resources.lock().await.get(&jid).cloned();
        if current
            .as_ref()
            .is_some_and(|current| remote_owner_registration_matches(current, &registration))
        {
            #[cfg(test)]
            self.wait_for_remote_owner_retirement_test_gate().await;

            if self
                .retire_remote_owner_registration(&services, &jid, &registration)
                .await
            {
                self.remove_remote_owner_registration_if_current(&jid, &registration)
                    .await;
            }
        }

        self.clear_pending_remote_owner_registration_retirement_if_current(&jid, &registration)
            .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
    }

    pub(super) async fn remove_remote_owner_registration_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|current| remote_owner_registration_matches(current, registration))
        {
            registrations.remove(jid);
        }
    }

    pub(crate) async fn register_remote_user_resource_on_owner(
        self: &Arc<Self>,
        msg: RelayRegisterRemoteUserResource,
    ) -> RelayRemoteResourceRegistrationReply {
        let jid = msg.jid.clone();
        // A pending displaced-mirror retirement holds the per-JID lock for
        // the full force-detach + unregister sequence. A successor's retry
        // must observe that WITHOUT waiting on the lock — blocking here would
        // burn the relay reply/retry budget the prompt-Busy design exists to
        // protect — so answer Busy immediately while retirement is pending.
        if self
            .pending_remote_owner_retirements
            .lock()
            .await
            .contains_key(&jid)
        {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Busy,
            };
        }
        let Some(lock) = self.lock_for_remote_owner_registration(&jid).await else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        };
        let guard = lock.lock().await;
        let reply = self
            .register_remote_user_resource_on_owner_locked(msg)
            .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
        reply
    }

    pub(super) async fn register_remote_user_resource_on_owner_locked(
        self: &Arc<Self>,
        msg: RelayRegisterRemoteUserResource,
    ) -> RelayRemoteResourceRegistrationReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        };
        let target_entity = user_entity(&msg.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        };
        let me = services.node_identity.current();
        if !snapshot.owner_lease_fresh || snapshot.owner != me {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        }

        if let Some(displaced) = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .cloned()
        {
            if displaced.registration_id == msg.registration_id
                && displaced.socket_node == msg.socket_node
                && displaced.socket_generation == msg.socket_generation
            {
                match remote_owner_registration_is_current(&services, &msg.jid, &displaced).await {
                    Ok(()) => {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Registered,
                        };
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.remove_remote_owner_registration_if_current(&msg.jid, &displaced)
                            .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else if displaced.socket_node == msg.socket_node
                && displaced.socket_generation >= msg.socket_generation
            {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            } else if displaced.socket_node != msg.socket_node {
                match remote_owner_registration_is_current(&services, &msg.jid, &displaced).await {
                    Ok(()) => {
                        self.schedule_remote_owner_registration_retirement(
                            &services, &msg.jid, &displaced,
                        )
                        .await;
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Busy,
                        };
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.remove_remote_owner_registration_if_current(&msg.jid, &displaced)
                            .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else {
                self.schedule_remote_owner_registration_retirement(&services, &msg.jid, &displaced)
                    .await;
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Busy,
                };
            }
            if self
                .remote_owner_resources
                .lock()
                .await
                .contains_key(&msg.jid)
            {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            }
        }

        let (tx, rx) = mpsc::channel(REMOTE_RESOURCE_OUTBOUND_CHANNEL_SIZE);
        let entry = ConnectionEntry::new(tx);
        apply_remote_resource_state(&entry, &msg.state);
        let owner = entry.carbons_handle();
        let force_detach_rx = entry.take_force_detach_rx();
        match services
            .user_registry
            .ask(RegisterUserResourceIfOwnerOrAbsent {
                jid: msg.jid.clone(),
                entry: entry.clone(),
                owner: owner.clone(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(REMOTE_OWNER_REGISTER_USER_REGISTRY_REPLY_TIMEOUT)
            .await
        {
            Ok(true) => {
                let registration = RemoteOwnerRegistration {
                    registration_id: msg.registration_id,
                    socket_node: msg.socket_node.clone(),
                    socket_generation: msg.socket_generation,
                    owner: owner.clone(),
                };
                if !services
                    .connection_registry
                    .register_entry_if_owner_or_absent(msg.jid.clone(), entry.clone(), &owner)
                {
                    if matches!(
                        unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await,
                        RemoteOwnerActorUnregisterOutcome::Failed
                    ) {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Unavailable,
                        };
                    }
                    return RelayRemoteResourceRegistrationReply {
                        status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                    };
                }
                match remote_owner_registration_is_current(&services, &msg.jid, &registration).await
                {
                    Ok(()) => {}
                    Err(status) => {
                        if matches!(
                            unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await,
                            RemoteOwnerActorUnregisterOutcome::Failed
                        ) {
                            return RelayRemoteResourceRegistrationReply {
                                status: RelayRemoteResourceRegistrationStatus::Unavailable,
                            };
                        }
                        services
                            .connection_registry
                            .unregister_if_owner(&msg.jid, &owner);
                        return RelayRemoteResourceRegistrationReply { status };
                    }
                }
                apply_remote_resource_presence_to_registry(
                    &services.connection_registry,
                    &msg.jid,
                    &owner,
                    msg.state.presence_available,
                    msg.state.presence_priority,
                    msg.state.presence_state.clone(),
                );
                self.remote_owner_resources
                    .lock()
                    .await
                    .insert(msg.jid.clone(), registration);
                self.spawn_remote_resource_forwarder(
                    msg.jid,
                    msg.registration_id,
                    msg.socket_node,
                    rx,
                    force_detach_rx,
                );
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Registered,
                }
            }
            Ok(false) => RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
            },
            Err(error) => {
                let status = registration_status_for_owner_register_error(&error);
                if status == RelayRemoteResourceRegistrationStatus::Unavailable {
                    tracing::warn!(
                        jid = %msg.jid,
                        %error,
                        "clustered remote-resource owner registration failed"
                    );
                }
                RelayRemoteResourceRegistrationReply { status }
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_on_owner(
        &self,
        msg: RelayUnregisterRemoteUserResource,
    ) -> RelayRemoteResourceUnregisterReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Failed,
            };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::NotRegistered,
            };
        };
        let actor_outcome =
            unregister_remote_owner_actor_entry(&services, &msg.jid, &registration.owner).await;
        match actor_outcome {
            RemoteOwnerActorUnregisterOutcome::Failed => {
                return RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Failed,
                };
            }
            RemoteOwnerActorUnregisterOutcome::RecordedRetry => {
                // The busy actor still holds the old resource and can route
                // peer stanzas to its ConnectionEntry. Removing the routing
                // entry and forwarder registration NOW would bounce those
                // stanzas at the socket as unavailable — outside the
                // detached XEP-0198 replay snapshot. Keep both until the
                // recorded owner-gated obligation completes; its janitor
                // convergence performs the full owner-side cleanup
                // (connection registry, forwarder, and owner tracking).
                return RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::RecordedRetry,
                };
            }
            RemoteOwnerActorUnregisterOutcome::Unregistered => {}
        }
        let _registry_removed = services
            .connection_registry
            .unregister_if_owner(&msg.jid, &registration.owner);
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations.get(&msg.jid).is_some_and(|registration| {
            registration.registration_id == msg.registration_id
                && registration.socket_generation == msg.socket_generation
        }) {
            registrations.remove(&msg.jid);
        }
        RelayRemoteResourceUnregisterReply {
            status: RelayRemoteResourceUnregisterStatus::Unregistered,
        }
    }

    pub(super) async fn lock_for_remote_owner_registration(
        &self,
        jid: &jid::FullJid,
    ) -> Option<Arc<Mutex<()>>> {
        let mut locks = self.remote_owner_registration_locks.lock().await;
        if !locks.contains_key(jid) && locks.len() >= MAX_REMOTE_OWNER_REGISTRATION_LOCKS {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        if !locks.contains_key(jid) && locks.len() >= MAX_REMOTE_OWNER_REGISTRATION_LOCKS {
            tracing::warn!(
                limit = MAX_REMOTE_OWNER_REGISTRATION_LOCKS,
                "clustered remote-resource registration lock map is full"
            );
            return None;
        }
        Some(
            locks
                .entry(jid.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
        )
    }

    pub(super) async fn remove_remote_owner_registration_lock_if_unused(
        &self,
        jid: &jid::FullJid,
        lock: &Arc<Mutex<()>>,
    ) {
        let mut locks = self.remote_owner_registration_locks.lock().await;
        if locks
            .get(jid)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(jid);
        }
    }

    #[cfg(test)]
    pub(super) fn install_remote_owner_retirement_test_gate(
        &self,
    ) -> Arc<super::super::RemoteOwnerRetirementTestGate> {
        if let Some(gate) = self.remote_owner_retirement_test_gate.get() {
            return Arc::clone(gate);
        }
        let gate = Arc::new(super::super::RemoteOwnerRetirementTestGate::default());
        let _ = self
            .remote_owner_retirement_test_gate
            .set(Arc::clone(&gate));
        Arc::clone(
            self.remote_owner_retirement_test_gate
                .get()
                .expect("test gate must be installed"),
        )
    }

    #[cfg(test)]
    async fn wait_for_remote_owner_retirement_test_gate(&self) {
        if let Some(gate) = self.remote_owner_retirement_test_gate.get() {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
}

pub(super) async fn unregister_remote_owner_actor_entry(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
) -> RemoteOwnerActorUnregisterOutcome {
    for attempt in 0..REMOTE_RESOURCE_BUSY_UNREGISTER_ATTEMPTS {
        match services
            .user_registry
            .ask(
                waddle_xmpp::registry::user_registry::UnregisterAndReleaseIfEmptyWithoutPendingRecord {
                    jid: jid.clone(),
                    owner: Some(owner.clone()),
                },
            )
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(
                waddle_xmpp::registry::UnregisterAndReleaseOutcome::Released
                | waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetainedLiveResources
                | waddle_xmpp::registry::UnregisterAndReleaseOutcome::AlreadyAbsent,
            ) => return RemoteOwnerActorUnregisterOutcome::Unregistered,
            Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(
                waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure::UserActorBusy,
            )) if attempt + 1 < REMOTE_RESOURCE_BUSY_UNREGISTER_ATTEMPTS => {
                tokio::time::sleep(remote_resource_busy_unregister_backoff(attempt)).await;
            }
            Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(reason)) => {
                let recorded = services
                    .user_registry
                    .ask(waddle_xmpp::registry::RecordPendingUserUnregister {
                        jid: jid.clone(),
                        owner: Some(owner.clone()),
                    })
                    .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                    .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                    .await;
                match recorded {
                    Ok(()) => {
                        tracing::warn!(
                            jid = %jid,
                            ?reason,
                            "clustered remote-resource owner actor unregister remained retryable; recorded janitor retry"
                        );
                        return RemoteOwnerActorUnregisterOutcome::RecordedRetry;
                    }
                    Err(record_error) => {
                        tracing::warn!(
                            jid = %jid,
                            ?reason,
                            ?record_error,
                            "clustered remote-resource owner actor unregister retry could not be recorded"
                        );
                        return RemoteOwnerActorUnregisterOutcome::Failed;
                    }
                }
            }
            Err(error) => {
                let recorded = services
                    .user_registry
                    .ask(waddle_xmpp::registry::RecordPendingUserUnregister {
                        jid: jid.clone(),
                        owner: Some(owner.clone()),
                    })
                    .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                    .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                    .await;
                match recorded {
                    Ok(()) => {
                        tracing::warn!(
                            jid = %jid,
                            ?error,
                            "clustered remote-resource owner actor unregister ask outcome was ambiguous; recorded janitor retry"
                        );
                        return RemoteOwnerActorUnregisterOutcome::RecordedRetry;
                    }
                    Err(record_error) => {
                        tracing::warn!(
                            jid = %jid,
                            ?error,
                            ?record_error,
                            "clustered remote-resource owner actor unregister retry could not be recorded"
                        );
                        return RemoteOwnerActorUnregisterOutcome::Failed;
                    }
                }
            }
        }
    }

    unreachable!("remote-resource owner unregister retry loop always returns")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteOwnerActorUnregisterOutcome {
    Unregistered,
    RecordedRetry,
    Failed,
}

pub(super) fn remote_owner_registration_matches(
    left: &RemoteOwnerRegistration,
    right: &RemoteOwnerRegistration,
) -> bool {
    left.registration_id == right.registration_id
        && left.socket_node == right.socket_node
        && left.socket_generation == right.socket_generation
        && Arc::ptr_eq(&left.owner, &right.owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::route_bridge::tests::{
        origin_identity, receiver_identity, services_with_claims, test_peer_id,
    };
    use tokio::time::{timeout, Duration};
    use tokio_util::sync::CancellationToken;

    async fn seed_current_remote_owner_registration(
        bridge: &Arc<OrderedRelayDeliveryBridge>,
        services: &Arc<OrderedRelayDeliveryServices>,
        socket_node: NodeId,
        socket_generation: RemoteResourceSocketGeneration,
    ) -> (jid::FullJid, RemoteOwnerRegistration) {
        let target = "juliet@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        services
            .connection_registry
            .register_entry(target.clone(), entry.clone());
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target.clone(),
                entry,
            })
            .await
            .expect("register current owner mirror");

        let registration = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            socket_node,
            socket_generation,
            owner,
        };
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(target.clone(), registration.clone());
        (target, registration)
    }

    #[test]
    fn owner_register_error_mapping_treats_claim_unavailable_as_retryable() {
        let busy = kameo::error::SendError::HandlerError(
            waddle_xmpp::registry::UserRegistryError::UserActorBusy(
                "mapper@example.test".parse::<jid::BareJid>().expect("jid"),
            ),
        );
        assert_eq!(
            registration_status_for_owner_register_error(&busy),
            RelayRemoteResourceRegistrationStatus::Busy
        );
        let claim_unavailable = kameo::error::SendError::HandlerError(
            waddle_xmpp::registry::UserRegistryError::ClaimUnavailable(
                "mapper@example.test".parse::<jid::BareJid>().expect("jid"),
            ),
        );
        assert_eq!(
            registration_status_for_owner_register_error(&claim_unavailable),
            RelayRemoteResourceRegistrationStatus::Busy,
            "a pending exact release must surface as a retryable Busy, not terminal"
        );
        let other = kameo::error::SendError::HandlerError(
            waddle_xmpp::registry::UserRegistryError::ClaimHeldByAnotherNode(
                "mapper@example.test".parse::<jid::BareJid>().expect("jid"),
            ),
        );
        assert_eq!(
            registration_status_for_owner_register_error(&other),
            RelayRemoteResourceRegistrationStatus::Unavailable
        );
    }

    async fn assert_successor_register_returns_busy_without_waiting_for_retirement(
        successor: RelayRegisterRemoteUserResource,
        displaced_socket_node: NodeId,
        displaced_generation: RemoteResourceSocketGeneration,
    ) {
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
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let gate = bridge.install_remote_owner_retirement_test_gate();
        let (target, displaced) = seed_current_remote_owner_registration(
            &bridge,
            &services,
            displaced_socket_node,
            displaced_generation,
        )
        .await;

        let reply = timeout(
            Duration::from_secs(1),
            bridge.register_remote_user_resource_on_owner(successor),
        )
        .await
        .expect("register handler must not wait for owner-managed retirement");
        assert_eq!(reply.status, RelayRemoteResourceRegistrationStatus::Busy);

        timeout(Duration::from_secs(1), gate.entered.notified())
            .await
            .expect("background retirement task should start");
        assert!(
            bridge
                .pending_remote_owner_retirements
                .lock()
                .await
                .get(&target)
                .is_some_and(|pending| remote_owner_registration_matches(pending, &displaced)),
            "the displaced registration should be marked pending while retirement runs"
        );
    }

    #[tokio::test]
    async fn successor_from_other_socket_returns_busy_before_retirement_awaits() {
        let (fresh_tx, _fresh_rx) = mpsc::channel(1);
        let fresh_entry = ConnectionEntry::new(fresh_tx);
        let target = "juliet@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        assert_successor_register_returns_busy_without_waiting_for_retirement(
            RelayRegisterRemoteUserResource {
                jid: target,
                registration_id: RemoteResourceRegistrationId::fresh(),
                socket_generation: RemoteResourceSocketGeneration::next(None),
                socket_node: NodeId::new("replacement-socket-node".to_string()),
                state: RemoteResourceStateSnapshot::from_entry(&fresh_entry, None),
                trace: RelayTraceContext::default(),
            },
            NodeId::new("missing-old-socket-node".to_string()),
            RemoteResourceSocketGeneration::next(None),
        )
        .await;
    }

    #[tokio::test]
    async fn successor_from_same_socket_newer_generation_returns_busy_before_retirement_awaits() {
        let old_generation = RemoteResourceSocketGeneration::next(None);
        let (fresh_tx, _fresh_rx) = mpsc::channel(1);
        let fresh_entry = ConnectionEntry::new(fresh_tx);
        let target = "juliet@example.test/phone"
            .parse::<jid::FullJid>()
            .expect("valid full jid");
        assert_successor_register_returns_busy_without_waiting_for_retirement(
            RelayRegisterRemoteUserResource {
                jid: target,
                registration_id: RemoteResourceRegistrationId::fresh(),
                socket_generation: RemoteResourceSocketGeneration::next(Some(old_generation)),
                socket_node: NodeId::new("same-socket-node".to_string()),
                state: RemoteResourceStateSnapshot::from_entry(&fresh_entry, None),
                trace: RelayTraceContext::default(),
            },
            NodeId::new("same-socket-node".to_string()),
            old_generation,
        )
        .await;
    }
}
