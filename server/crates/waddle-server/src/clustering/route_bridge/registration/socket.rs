use super::super::delivery::receiver::{current_claim, user_entity};
use super::super::*;

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
}
