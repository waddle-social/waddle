use super::super::*;
use super::owner::{unregister_remote_owner_actor_entry, RemoteOwnerActorUnregisterOutcome};

#[cfg(test)]
tokio::task_local! {
    pub(super) static RETIREMENT_ATTEMPTS: Arc<std::sync::atomic::AtomicUsize>;
    pub(super) static RETIREMENT_AUTHORITY_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
    static LOCAL_RETIREMENT_UNREGISTER_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
}

impl OrderedRelayDeliveryBridge {
    /// Finish an owner-local socket's retirement independently of the relay
    /// request lifetime. The transferred gate prevents concurrent admission;
    /// its pending generations survive timeout or task cancellation.
    pub(super) fn schedule_local_incumbent_retirement(
        &self,
        services: Arc<OrderedRelayDeliveryServices>,
        jid: jid::FullJid,
        generation: waddle_xmpp_core::OccupancySessionGeneration,
        mut guard: waddle_xmpp::registry::ConnectionBindGuard,
    ) {
        // Inventory exact actor ownership before the task can be cancelled or
        // occupancy cleanup clears its independent generation obligations.
        if let Some(incumbent) = guard.incumbent() {
            guard.retain_actor_retirement(incumbent);
        }
        #[cfg(test)]
        let unregister_gate = LOCAL_RETIREMENT_UNREGISTER_GATE.try_with(Clone::clone).ok();
        #[cfg(test)]
        let retirement_finished = unregister_gate
            .as_ref()
            .map(|(_, _, finished)| finished.clone());
        tokio::spawn(async move {
            let retirement = async {
                use waddle_xmpp::registry::{ForceDetachOrigin, SocketCleanupState};
                let incumbent = guard.incumbent();
                let resumed = incumbent
                    .as_ref()
                    .is_some_and(|incumbent| incumbent.generation == generation);
                let actor_retirements = guard.pending_actor_retirements();
                for pending in &actor_retirements {
                    if pending.state() == SocketCleanupState::Running {
                        let (ack, _ack_rx) = tokio::sync::oneshot::channel();
                        let _ = pending
                            .entry
                            .force_detach_sender()
                            .try_send(ForceDetachRequest {
                                origin: if pending.generation == generation {
                                    ForceDetachOrigin::CrossNodeResume
                                } else {
                                    ForceDetachOrigin::FreshBindReplacement
                                },
                                requester_bare_jid: jid.to_bare(),
                                ack,
                            });
                        // Route removal and the detach ACK can both precede
                        // handler shutdown. Only the lifecycle is proof.
                        pending.wait_stopped().await;
                    }
                }
                if !guard.pending_retirements().is_empty() {
                    let Some(state) = services.web_socket_state.upgrade() else {
                        return;
                    };
                    if !crate::server::routes::websocket::complete_bind_retirements(
                        &state, &jid, generation, &mut guard, resumed,
                    )
                    .await
                    {
                        return;
                    }
                }
                #[cfg(test)]
                if let Some((entered, release, _)) = unregister_gate {
                    entered.notify_one();
                    release.notified().await;
                }
                for pending in actor_retirements {
                    let owner = pending.entry.carbons_handle();
                    if !matches!(
                        unregister_remote_owner_actor_entry(&services, &jid, &owner).await,
                        RemoteOwnerActorUnregisterOutcome::Unregistered
                    ) {
                        return;
                    }
                    services
                        .connection_registry
                        .unregister_if_owner(&jid, &owner);
                    if pending.generation != generation {
                        pending.finish(SocketCleanupState::Retired);
                    }
                    guard.complete_actor_retirement(&pending);
                }
            };
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), retirement).await;
            drop(guard);
            #[cfg(test)]
            if let Some(finished) = retirement_finished {
                finished.notify_one();
            }
        });
    }

    /// A socket bound on the UserActor owner bypasses the remote registration
    /// endpoint. It must still retire a foreign incumbent before publishing
    /// its local route. Same-generation resume keeps the occupancy intact.
    pub(crate) async fn retire_remote_incumbent_before_local_bind(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        generation: waddle_xmpp_core::OccupancySessionGeneration,
    ) -> bool {
        let Some(services) = self.services.get() else {
            return false;
        };
        let Some(lock) = self.lock_for_remote_owner_registration(jid).await else {
            return false;
        };
        let guard = lock.lock().await;
        let retired = async {
            // Capture the exact incumbent while the caller's bind gate and
            // our remote registration gate prevent successor publication.
            // Check durable authority briefly: a SQL connection must not be
            // retained while force-detach waits for remote socket cleanup.
            let registration = self.remote_owner_resources.lock().await.get(jid).cloned();
            match crate::occupancy_authority::is_current(
                &services.occupancy_database,
                jid,
                generation,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    tracing::warn!(%jid, %error,
                        "local bind could not verify authority before remote retirement");
                    return false;
                }
            }
            #[cfg(test)]
            if let Ok((entered, release)) = RETIREMENT_AUTHORITY_GATE.try_with(Clone::clone) {
                entered.notify_one();
                release.notified().await;
            }
            match registration {
                None => true,
                Some(registration) if registration.occupancy_session == generation => true,
                Some(registration) => {
                    if self
                        .retire_remote_owner_registration_with_origin(
                            services,
                            jid,
                            &registration,
                            waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement,
                        )
                        .await
                    {
                        self.remove_remote_owner_registration_if_current(jid, &registration)
                            .await;
                        true
                    } else {
                        false
                    }
                }
            }
        }
        .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
            .await;
        retired
    }

    pub(super) async fn retire_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        let origin = match crate::occupancy_authority::is_current(
            &services.occupancy_database,
            jid,
            registration.occupancy_session,
        )
        .await
        {
            Ok(true) => waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
            Ok(false) => waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement,
            Err(error) => {
                tracing::warn!(%jid, %error,
                    "remote-resource retirement authority lookup failed");
                return false;
            }
        };
        self.retire_remote_owner_registration_with_origin(services, jid, registration, origin)
            .await
    }

    async fn retire_remote_owner_registration_with_origin(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        origin: waddle_xmpp::registry::ForceDetachOrigin,
    ) -> bool {
        #[cfg(test)]
        let _ = RETIREMENT_ATTEMPTS.try_with(|attempts| {
            attempts.fetch_add(1, Ordering::Relaxed);
        });
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let detach = handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                occupancy_session: Some(registration.occupancy_session),
                origin,
                requester_bare_jid: jid.to_bare(),
                trace: RelayTraceContext::default(),
            })
            .await;
        // A stale socket reference proves no live route, not that its remote
        // room membership cleanup completed. A fresh bind must retain the
        // retirement obligation until the socket provides that proof.
        if origin == waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement
            && (detach.is_err()
                || detach.as_ref().is_ok_and(|reply| {
                    reply.status != RelayRemoteResourceForceDetachStatus::Detached
                }))
        {
            return false;
        }
        self.finish_remote_owner_registration_retire(services, jid, registration, detach)
            .await
    }

    pub(in super::super) async fn finish_remote_owner_registration_retire(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        detach: Result<RelayForceDetachRemoteUserResourceReply, RelayAskError>,
    ) -> bool {
        let reply = match detach {
            Ok(reply) => reply,
            Err(error) if ask_error_proves_remote_resource_ref_stale(&error) => {
                tracing::info!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement cleaning stale old-socket mirror"
                );
                if matches!(
                    unregister_remote_owner_actor_entry(services, jid, &registration.owner).await,
                    RemoteOwnerActorUnregisterOutcome::Failed
                ) {
                    return false;
                }
                services
                    .connection_registry
                    .unregister_if_owner(jid, &registration.owner);
                return true;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement refused uncertain old-socket detach"
                );
                return false;
            }
        };
        if !matches!(
            reply.status,
            RelayRemoteResourceForceDetachStatus::Detached
                | RelayRemoteResourceForceDetachStatus::NotLive
        ) {
            tracing::warn!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource replacement refused uncertain old-socket detach"
            );
            return false;
        }
        if matches!(
            unregister_remote_owner_actor_entry(services, jid, &registration.owner).await,
            RemoteOwnerActorUnregisterOutcome::Failed
        ) {
            return false;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        true
    }

    pub(in super::super) async fn cleanup_remote_owner_resource_if_registration(
        &self,
        jid: &jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| registration.registration_id == registration_id)
            .cloned();
        let Some(registration) = registration else {
            return;
        };
        if matches!(
            unregister_remote_owner_actor_entry(&services, jid, &registration.owner).await,
            RemoteOwnerActorUnregisterOutcome::Failed
        ) {
            return;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|registration| registration.registration_id == registration_id)
        {
            registrations.remove(jid);
        }
    }
}

#[cfg(test)]
mod local_actor_retirement_tests {
    use super::*;
    use crate::clustering::route_bridge::{
        remote_registration_request_from_entry,
        tests::{origin_identity, receiver_identity, services_with_claims, test_peer_id},
    };
    use tokio::time::{timeout, Duration};
    use waddle_xmpp::registry::SocketCleanupState;

    async fn assert_cancelled_unregister_remains_retryable(resumed: bool) {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        services.connection_registry = Arc::clone(&state.deps.protocol.connection_registry);
        services.user_registry = state.deps.protocol.user_registry.clone();
        services.sm_session_registry = Arc::clone(&state.deps.protocol.sm_session_registry);
        services.web_socket_state = Arc::downgrade(&state);
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(services.clone());
        let jid: jid::FullJid = "juliet@example.test/phone".parse().expect("JID");
        let old_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let generation = if resumed {
            old_generation
        } else {
            waddle_xmpp_core::OccupancySessionGeneration::mint()
        };
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        *entry.occupancy_session.lock().expect("generation") = Some(old_generation);
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: jid.clone(),
                entry: entry.clone(),
            })
            .await
            .expect("incumbent actor registration");
        let mut guard = services.connection_registry.lock_bind(&jid).await;
        let lifecycle = guard.publish(entry.clone(), old_generation);
        lifecycle.finish(if resumed {
            SocketCleanupState::Detached
        } else {
            SocketCleanupState::Retired
        });
        if !resumed {
            guard.retain_retirement(old_generation);
        }
        crate::occupancy_authority::publish(&services.occupancy_database, &jid, generation)
            .await
            .expect("successor authority");
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(tokio::sync::Notify::new());
        LOCAL_RETIREMENT_UNREGISTER_GATE.sync_scope(
            (entered.clone(), release, finished.clone()),
            || {
                bridge.schedule_local_incumbent_retirement(
                    services.clone(),
                    jid.clone(),
                    generation,
                    guard,
                );
            },
        );
        timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("occupancy cleanup finished before unregister");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        finished.notified().await;
        let guard = services.connection_registry.lock_bind(&jid).await;
        assert!(
            guard.pending_retirements().is_empty(),
            "occupancy cleanup has already finished"
        );
        assert!(guard.incumbent().is_some_and(|current| Arc::ptr_eq(&current, &lifecycle)),
            "cancelling the unregister await must retain the exact actor owner after occupancy cleanup");
        let actor_retirements = guard.pending_actor_retirements();
        assert_eq!(actor_retirements.len(), 1);
        assert!(Arc::ptr_eq(&actor_retirements[0], &lifecycle));
        drop(guard);
        tokio::time::resume();
        let (tx, _rx) = mpsc::channel(1);
        let replacement = ConnectionEntry::new(tx);
        *replacement.occupancy_session.lock().expect("generation") = Some(generation);
        let request = remote_registration_request_from_entry(
            jid.clone(),
            NodeId::new("successor".to_owned()),
            &replacement,
        );
        assert_eq!(request.state.occupancy_session, Some(generation));
        assert!(crate::occupancy_authority::is_current(
            &services.occupancy_database,
            &jid,
            generation,
        )
        .await
        .expect("successor authority remains current after the actor timeout"));
        assert_eq!(
            bridge
                .register_remote_user_resource_on_owner(request.clone())
                .await
                .status,
            RelayRemoteResourceRegistrationStatus::Busy,
            "retry must finish the retained actor unregister"
        );
        let guard = timeout(
            Duration::from_secs(5),
            services.connection_registry.lock_bind(&jid),
        )
        .await
        .expect("retry completes");
        assert!(
            guard.pending_retirements().is_empty(),
            "same-generation actor cleanup must not become an occupancy sweep"
        );
        drop(guard);
        assert_eq!(
            bridge
                .register_remote_user_resource_on_owner(request)
                .await
                .status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        assert_eq!(
            lifecycle.state(),
            if resumed {
                SocketCleanupState::Detached
            } else {
                SocketCleanupState::Retired
            }
        );
    }

    #[tokio::test]
    async fn cancelled_local_actor_unregister_retains_retired_owner_for_retry() {
        assert_cancelled_unregister_remains_retryable(false).await;
    }

    #[tokio::test]
    async fn cancelled_local_actor_unregister_preserves_same_generation_detach() {
        assert_cancelled_unregister_remains_retryable(true).await;
    }
}
