use super::super::*;
use super::owner::{unregister_remote_owner_actor_entry, RemoteOwnerActorUnregisterOutcome};

#[cfg(test)]
tokio::task_local! {
    pub(super) static RETIREMENT_ATTEMPTS: Arc<std::sync::atomic::AtomicUsize>;
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
        tokio::spawn(async move {
            let retirement =
                async {
                    use waddle_xmpp::registry::{ForceDetachOrigin, SocketCleanupState};
                    let incumbent = guard.incumbent();
                    let resumed = incumbent
                        .as_ref()
                        .is_some_and(|incumbent| incumbent.generation == generation);
                    if let Some(incumbent) = &incumbent {
                        if incumbent.state() == SocketCleanupState::Running {
                            let (ack, _ack_rx) = tokio::sync::oneshot::channel();
                            let _ = incumbent.entry.force_detach_sender().try_send(
                                ForceDetachRequest {
                                    origin: if resumed {
                                        ForceDetachOrigin::CrossNodeResume
                                    } else {
                                        ForceDetachOrigin::FreshBindReplacement
                                    },
                                    requester_bare_jid: jid.to_bare(),
                                    ack,
                                },
                            );
                            // Route removal and the detach ACK can both precede
                            // handler shutdown. Only the lifecycle is proof.
                            incumbent.wait_stopped().await;
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
                    if let Some(incumbent) = &incumbent {
                        let owner = incumbent.entry.carbons_handle();
                        if !matches!(
                            unregister_remote_owner_actor_entry(&services, &jid, &owner).await,
                            RemoteOwnerActorUnregisterOutcome::Unregistered
                        ) {
                            return;
                        }
                        services
                            .connection_registry
                            .unregister_if_owner(&jid, &owner);
                        if !resumed {
                            incumbent.finish(SocketCleanupState::Retired);
                        }
                    }
                };
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), retirement).await;
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
            // The caller may have waited for local socket cleanup since it
            // published this generation. A later remote bind can already own
            // the JID: hold current authority before inspecting or retiring it.
            let Ok(Some(_authority)) = crate::occupancy_authority::acquire_current(
                &services.occupancy_database,
                jid,
                generation,
            )
            .await
            else {
                return false;
            };
            let registration = self.remote_owner_resources.lock().await.get(jid).cloned();
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
            Err(_) => return false,
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
