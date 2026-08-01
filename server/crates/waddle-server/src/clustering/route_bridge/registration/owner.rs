use super::super::delivery::receiver::{current_claim, user_entity};
use super::super::*;

impl OrderedRelayDeliveryBridge {
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
                        if !self
                            .retire_remote_owner_registration(&services, &msg.jid, &displaced)
                            .await
                        {
                            return RelayRemoteResourceRegistrationReply {
                                status: RelayRemoteResourceRegistrationStatus::Unavailable,
                            };
                        }
                        let mut registrations = self.remote_owner_resources.lock().await;
                        match registrations.get(&msg.jid) {
                            Some(current)
                                if remote_owner_registration_matches(current, &displaced) =>
                            {
                                registrations.remove(&msg.jid);
                            }
                            Some(_) => {
                                return RelayRemoteResourceRegistrationReply {
                                    status:
                                        RelayRemoteResourceRegistrationStatus::StaleRegistration,
                                };
                            }
                            None => {}
                        }
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.remove_remote_owner_registration_if_current(&msg.jid, &displaced)
                            .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else {
                if !self
                    .retire_remote_owner_registration(&services, &msg.jid, &displaced)
                    .await
                {
                    return RelayRemoteResourceRegistrationReply {
                        status: RelayRemoteResourceRegistrationStatus::Unavailable,
                    };
                }
                let mut registrations = self.remote_owner_resources.lock().await;
                match registrations.get(&msg.jid) {
                    Some(current) if remote_owner_registration_matches(current, &displaced) => {
                        registrations.remove(&msg.jid);
                    }
                    Some(_) => {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                        };
                    }
                    None => {}
                }
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
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
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
                    if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
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
                        if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
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
                tracing::warn!(
                    jid = %msg.jid,
                    %error,
                    "clustered remote-resource owner registration failed"
                );
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Unavailable,
                }
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_on_owner(
        &self,
        msg: RelayUnregisterRemoteUserResource,
    ) -> RelayRemoteResourceUnregisterReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUnregisterReply { removed: false };
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
            return RelayRemoteResourceUnregisterReply { removed: false };
        };
        let actor_removed = services
            .user_registry
            .ask(UnregisterUserResource {
                jid: msg.jid.clone(),
                owner: Some(registration.owner.clone()),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
            .is_ok();
        if !actor_removed {
            return RelayRemoteResourceUnregisterReply { removed: false };
        }
        let registry_removed = services
            .connection_registry
            .unregister_if_owner(&msg.jid, &registration.owner)
            .is_some();
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations.get(&msg.jid).is_some_and(|registration| {
            registration.registration_id == msg.registration_id
                && registration.socket_generation == msg.socket_generation
        }) {
            registrations.remove(&msg.jid);
        }
        RelayRemoteResourceUnregisterReply {
            removed: registry_removed,
        }
    }

    pub(crate) async fn update_remote_user_resource_on_owner(
        &self,
        msg: RelayUpdateRemoteUserResource,
    ) -> RelayRemoteResourceUpdateReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::Unavailable,
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
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::StaleRegistration,
            };
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::StaleRegistration,
                };
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.jid,
                    %error,
                    "clustered remote-resource state update could not resolve owner UserActor"
                );
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::Unavailable,
                };
            }
        };
        let jid = msg.jid;
        let status = match msg.update {
            RemoteResourceStateUpdate::Presence {
                available,
                priority,
                state,
            } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(_) => {
                        if apply_remote_resource_presence_to_registry(
                            &services.connection_registry,
                            &jid,
                            &registration.owner,
                            available,
                            priority,
                            state,
                        ) {
                            RelayRemoteResourceUpdateStatus::Updated
                        } else {
                            RelayRemoteResourceUpdateStatus::StaleRegistration
                        }
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::Carbons { enabled } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        entry.carbons_enabled.store(enabled, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::RosterInterested => match owner_remote_entry_if_current(
                &actor,
                &services.connection_registry,
                &jid,
                &registration.owner,
            )
            .await
            {
                Ok(entry) => {
                    entry.roster_interested.store(true, Ordering::Relaxed);
                    RelayRemoteResourceUpdateStatus::Updated
                }
                Err(status) => status,
            },
            RemoteResourceStateUpdate::BlocklistInterested => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        entry.blocklist_interested.store(true, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
        };
        RelayRemoteResourceUpdateReply { status }
    }

    pub(super) async fn retire_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let detach = handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                requester_bare_jid: jid.to_bare(),
                trace: RelayTraceContext::default(),
            })
            .await;
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
                if !unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
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
        if !unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
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
        let actor_removed = services
            .user_registry
            .ask(UnregisterUserResource {
                jid: jid.clone(),
                owner: Some(registration.owner.clone()),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
            .is_ok();
        if !actor_removed {
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
}

pub(in super::super) async fn owner_remote_entry_if_current(
    actor: &ActorRef<waddle_xmpp::registry::user_actor::UserActor>,
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
) -> Result<ConnectionEntry, RelayRemoteResourceUpdateStatus> {
    let actor_entry = match actor
        .ask(waddle_xmpp::registry::GetConnectionEntry { jid: jid.clone() })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(entry)) => entry,
        Ok(None) => return Err(RelayRemoteResourceUpdateStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceUpdateStatus::Unavailable),
    };
    if !Arc::ptr_eq(&actor_entry.carbons_enabled, owner) {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    let Some(registry_entry) = registry.entry_if_owner(jid, owner) else {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    };
    if !Arc::ptr_eq(
        &registry_entry.carbons_enabled,
        &actor_entry.carbons_enabled,
    ) {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    Ok(registry_entry)
}

pub(super) async fn remote_owner_registration_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RelayRemoteResourceRegistrationStatus> {
    let actor = match services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: jid.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return Err(RelayRemoteResourceRegistrationStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceRegistrationStatus::Unavailable),
    };
    owner_remote_entry_if_current(
        &actor,
        &services.connection_registry,
        jid,
        &registration.owner,
    )
    .await
    .map(|_| ())
    .map_err(|status| match status {
        RelayRemoteResourceUpdateStatus::Updated => {
            RelayRemoteResourceRegistrationStatus::Registered
        }
        RelayRemoteResourceUpdateStatus::StaleRegistration => {
            RelayRemoteResourceRegistrationStatus::StaleRegistration
        }
        RelayRemoteResourceUpdateStatus::Unavailable => {
            RelayRemoteResourceRegistrationStatus::Unavailable
        }
    })
}

pub(super) async fn unregister_remote_owner_actor_entry(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
) -> bool {
    match services
        .user_registry
        .ask(UnregisterUserResource {
            jid: jid.clone(),
            owner: Some(owner.clone()),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource owner actor unregister failed"
            );
            false
        }
    }
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
