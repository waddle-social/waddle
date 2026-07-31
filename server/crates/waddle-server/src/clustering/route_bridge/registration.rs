use super::delivery::receiver::{current_claim, user_entity};
use super::*;

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
        let mut handle = RelayHandle::new(user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let reply = match handle
            .register_remote_user_resource(RelayRegisterRemoteUserResource {
                jid: jid.clone(),
                registration_id,
                socket_generation,
                socket_node,
                state,
                trace: RelayTraceContext::default(),
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
            | RelayRemoteResourceRegistrationStatus::Unavailable => {
                RemoteResourceRegisterOutcome::Failed
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) {
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
            return;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        if let Err(error) = handle
            .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource unregister ask failed; owner-side stale \
                 entry will self-heal on closed-channel delivery"
            );
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

    pub(crate) async fn try_fanout_remote_user_carbons(
        &self,
        source_jid: &jid::FullJid,
        owner: &jid::BareJid,
        message: &xmpp_parsers::message::Message,
        kind: CarbonKind,
        exclude: Vec<jid::FullJid>,
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::Carbons {
                owner: owner.clone(),
                message: RemoteStanza(Stanza::Message(message.clone())),
                kind: kind.into(),
                exclude,
            },
        )
        .await
    }

    pub(crate) async fn try_fanout_remote_user_roster_push(
        &self,
        source_jid: &jid::FullJid,
        user_jid: &jid::BareJid,
        item: &RosterItem,
        version: &RosterVersion,
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::RosterPush {
                user_jid: user_jid.clone(),
                source_jid: source_jid.clone(),
                item: item.clone(),
                version: version.clone(),
            },
        )
        .await
    }

    pub(crate) async fn try_fanout_remote_user_blocklist_push(
        &self,
        source_jid: &jid::FullJid,
        user_bare: &jid::BareJid,
        blocked: bool,
        jids: &[jid::Jid],
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::BlocklistPush {
                user_bare: user_bare.clone(),
                blocked,
                jids: jids.to_vec(),
            },
        )
        .await
    }

    pub(super) async fn try_remote_user_side_effect(
        &self,
        source_jid: &jid::FullJid,
        effect: RemoteUserSideEffect,
    ) -> bool {
        let Some(registration) = self.remote_socket_registration_if_current(source_jid).await
        else {
            return false;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .remote_user_side_effect(RelayRemoteUserSideEffect {
                source_jid: source_jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                effect,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Applied,
            }) => true,
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            }) => {
                self.remove_remote_socket_registration_if_current(source_jid, &registration)
                    .await;
                false
            }
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
            }) => false,
            Err(RelayAskError::Send {
                effect: RelaySendEffect::MaybeCommitted,
                message,
                ..
            }) => {
                tracing::warn!(
                    jid = %source_jid,
                    %message,
                    "clustered remote-user side-effect relay may have committed; suppressing local fallback"
                );
                true
            }
            Err(error) => {
                tracing::warn!(
                    jid = %source_jid,
                    %error,
                    "clustered remote-user side-effect relay ask failed"
                );
                false
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

    pub(crate) async fn apply_remote_user_side_effect_on_owner(
        &self,
        msg: RelayRemoteUserSideEffect,
    ) -> RelayRemoteUserSideEffectReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
            };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.source_jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            };
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.source_jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::StaleRegistration,
                };
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.source_jid,
                    %error,
                    "clustered remote-user side effect could not resolve owner UserActor"
                );
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::Unavailable,
                };
            }
        };
        if let Err(status) = owner_remote_entry_if_current(
            &actor,
            &services.connection_registry,
            &msg.source_jid,
            &registration.owner,
        )
        .await
        {
            return RelayRemoteUserSideEffectReply {
                status: match status {
                    RelayRemoteResourceUpdateStatus::Updated => {
                        RelayRemoteUserSideEffectStatus::Applied
                    }
                    RelayRemoteResourceUpdateStatus::StaleRegistration => {
                        RelayRemoteUserSideEffectStatus::StaleRegistration
                    }
                    RelayRemoteResourceUpdateStatus::Unavailable => {
                        RelayRemoteUserSideEffectStatus::Unavailable
                    }
                },
            };
        }

        let status = match msg.effect {
            RemoteUserSideEffect::Carbons {
                owner,
                message,
                kind,
                exclude,
            } => match message.0 {
                Stanza::Message(message) => {
                    let web_socket_state = services.web_socket_state.upgrade();
                    crate::server::routes::interpret::carbons::send_carbons_to_registry(
                        &services.connection_registry,
                        Some(&services.sm_session_registry),
                        web_socket_state.as_deref(),
                        owner,
                        Box::new(message),
                        kind.into(),
                        exclude,
                    )
                    .await;
                    RelayRemoteUserSideEffectStatus::Applied
                }
                _ => RelayRemoteUserSideEffectStatus::StaleRegistration,
            },
            RemoteUserSideEffect::RosterPush {
                user_jid,
                source_jid,
                item,
                version,
            } => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return RelayRemoteUserSideEffectReply {
                        status: RelayRemoteUserSideEffectStatus::Unavailable,
                    };
                };
                crate::server::routes::websocket::handlers::iq::roster::push::send_roster_push_to_sibling_resources(
                    &state,
                    &user_jid,
                    &source_jid,
                    &item,
                    &version,
                )
                .await;
                RelayRemoteUserSideEffectStatus::Applied
            }
            RemoteUserSideEffect::BlocklistPush {
                user_bare,
                blocked,
                jids,
            } => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return RelayRemoteUserSideEffectReply {
                        status: RelayRemoteUserSideEffectStatus::Unavailable,
                    };
                };
                crate::server::routes::websocket::handlers::iq::blocking::send_blocking_pushes(
                    &state, &user_bare, blocked, &jids,
                )
                .await;
                RelayRemoteUserSideEffectStatus::Applied
            }
        };
        RelayRemoteUserSideEffectReply { status }
    }

    pub(super) async fn remove_remote_socket_registration_if_snapshot(
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
            requester_bare_jid: msg.requester_bare_jid,
            ack,
        };
        if entry.force_detach_sender().try_send(request).is_err() {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
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
            requester_bare_jid: jid.to_bare(),
            ack,
        });
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

    pub(super) async fn finish_remote_owner_registration_retire(
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

    pub(super) async fn cleanup_remote_owner_resource_if_registration(
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

    pub(super) fn spawn_remote_resource_forwarder(
        self: &Arc<Self>,
        jid: jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_node: NodeId,
        mut rx: mpsc::Receiver<OutboundStanza>,
        force_detach_rx: Option<mpsc::Receiver<ForceDetachRequest>>,
    ) {
        let outbound_bridge = Arc::clone(self);
        let outbound_jid = jid.clone();
        let outbound_socket_node = socket_node.clone();
        tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                forward_remote_resource_outbound(
                    &outbound_bridge,
                    &outbound_jid,
                    registration_id,
                    &outbound_socket_node,
                    outbound,
                )
                .await;
            }
        });
        if let Some(mut force_detach_rx) = force_detach_rx {
            let control_bridge = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(request) = force_detach_rx.recv().await {
                    forward_remote_resource_force_detach(
                        &control_bridge,
                        &jid,
                        registration_id,
                        &socket_node,
                        request,
                    )
                    .await;
                }
            });
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

pub(super) fn apply_remote_resource_presence_to_registry(
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
    available: bool,
    priority: i8,
    state: Option<RemotePresenceStateSnapshot>,
) -> bool {
    if !registry.update_presence_if_owner(jid, owner, available, priority) {
        return false;
    }
    if available {
        let state = state.map(PresenceState::from).unwrap_or(PresenceState {
            show: None,
            status: None,
            priority,
            payloads: Vec::new(),
        });
        registry.update_presence_state_if_owner(
            jid,
            owner,
            state.show,
            state.status,
            state.priority,
            state.payloads,
        )
    } else {
        registry.clear_presence_state_if_owner(jid, owner)
    }
}

pub(super) async fn owner_remote_entry_if_current(
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

pub(super) async fn forward_remote_resource_outbound(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_node: &NodeId,
    outbound: OutboundStanza,
) {
    if outbound.pending_row_id.is_some() {
        tracing::warn!(
            jid = %jid,
            "clustered remote-resource forwarder received pending-delivery \
             flush frame; dropping to avoid breaking SM row ack accounting"
        );
        return;
    }
    let kind = outbound.kind;
    let frame = RemoteResourceOutboundFrame {
        jid: jid.clone(),
        registration_id,
        stanza: RemoteStanza(outbound.stanza),
        kind,
    };
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    match handle
        .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
            frame,
            trace: RelayTraceContext::default(),
        })
        .await
    {
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Delivered,
        }) => {}
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Unavailable,
        }) => {
            tracing::debug!(
                jid = %jid,
                "clustered remote-resource socket registration unavailable; cleaning owner mirror"
            );
            bridge
                .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                .await;
        }
        Ok(reply) => {
            tracing::debug!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource forwarder did not deliver frame"
            );
        }
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource forwarder relay ask failed"
            );
            if ask_error_proves_remote_resource_ref_stale(&error) {
                bridge
                    .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                    .await;
            }
        }
    }
}

pub(super) async fn forward_remote_resource_force_detach(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_node: &NodeId,
    request: ForceDetachRequest,
) {
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    let outcome = match handle
        .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
            jid: jid.clone(),
            registration_id,
            requester_bare_jid: request.requester_bare_jid,
            trace: RelayTraceContext::default(),
        })
        .await
    {
        Ok(reply) => reply.outcome,
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource force-detach relay ask failed"
            );
            ForceDetachOutcome::NotPersisted
        }
    };
    let _ = request.ack.send(outcome);
}

pub(super) fn apply_remote_resource_state(
    entry: &ConnectionEntry,
    state: &RemoteResourceStateSnapshot,
) {
    entry
        .carbons_enabled
        .store(state.carbons_enabled, Ordering::Relaxed);
    entry
        .roster_interested
        .store(state.roster_interested, Ordering::Relaxed);
    entry
        .blocklist_interested
        .store(state.blocklist_interested, Ordering::Relaxed);
    entry
        .presence_available
        .store(state.presence_available, Ordering::Relaxed);
    entry
        .presence_priority
        .store(state.presence_priority, Ordering::Relaxed);
}
