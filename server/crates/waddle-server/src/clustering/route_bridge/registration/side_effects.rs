use super::super::*;

/// Whether the remote owner supplied an authoritative XEP-0280 recipient
/// snapshot. A timeout after send can be maybe-committed, but must never be
/// represented as a known empty audience in ingress shadow capture.
pub(crate) enum RemoteCarbonFanout {
    Applied(Vec<jid::FullJid>),
    MaybeCommitted,
}

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn try_fanout_remote_user_carbons(
        &self,
        source_jid: &jid::FullJid,
        owner: &jid::BareJid,
        message: &xmpp_parsers::message::Message,
        kind: CarbonKind,
        exclude: Vec<jid::FullJid>,
    ) -> Option<RemoteCarbonFanout> {
        self.try_remote_user_carbons(
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

    async fn try_remote_user_carbons(
        &self,
        source_jid: &jid::FullJid,
        effect: RemoteUserSideEffect,
    ) -> Option<RemoteCarbonFanout> {
        let registration = self
            .remote_socket_registration_if_current(source_jid)
            .await?;
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
            Ok(reply) if reply.status == RelayRemoteUserSideEffectStatus::Applied => {
                Some(RemoteCarbonFanout::Applied(reply.carbon_recipients))
            }
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
                ..
            }) => {
                self.remove_remote_socket_registration_if_current(source_jid, &registration)
                    .await;
                None
            }
            Ok(_) => None,
            Err(RelayAskError::Send {
                effect: RelaySendEffect::MaybeCommitted,
                message,
                ..
            }) => {
                tracing::warn!(
                    jid = %source_jid,
                    %message,
                    "clustered remote-user carbon relay may have committed; suppressing local fallback"
                );
                Some(RemoteCarbonFanout::MaybeCommitted)
            }
            Err(error) => {
                tracing::warn!(jid = %source_jid, %error, "clustered remote-user carbon relay ask failed");
                None
            }
        }
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
                ..
            }) => true,
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
                ..
            }) => {
                self.remove_remote_socket_registration_if_current(source_jid, &registration)
                    .await;
                false
            }
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
                ..
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

    pub(crate) async fn apply_remote_user_side_effect_on_owner(
        &self,
        msg: RelayRemoteUserSideEffect,
    ) -> RelayRemoteUserSideEffectReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
                carbon_recipients: Vec::new(),
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
                carbon_recipients: Vec::new(),
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
                    carbon_recipients: Vec::new(),
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
                    carbon_recipients: Vec::new(),
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
                carbon_recipients: Vec::new(),
            };
        }

        let (status, carbon_recipients) = match msg.effect {
            RemoteUserSideEffect::Carbons {
                owner,
                message,
                kind,
                exclude,
            } => match message.0 {
                Stanza::Message(message) => {
                    let web_socket_state = services.web_socket_state.upgrade();
                    let carbon_recipients =
                        crate::server::routes::interpret::carbons::send_carbons_to_registry(
                            &services.connection_registry,
                            crate::server::routes::interpret::carbons::CarbonRegistryDeps {
                                ingress_effect_capture: None,
                                sm_session_registry: Some(&services.sm_session_registry),
                                web_socket_state: web_socket_state.as_deref(),
                            },
                            owner,
                            Box::new(message),
                            kind.into(),
                            exclude,
                        )
                        .await;
                    (RelayRemoteUserSideEffectStatus::Applied, carbon_recipients)
                }
                _ => (
                    RelayRemoteUserSideEffectStatus::StaleRegistration,
                    Vec::new(),
                ),
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
                        carbon_recipients: Vec::new(),
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
                (RelayRemoteUserSideEffectStatus::Applied, Vec::new())
            }
            RemoteUserSideEffect::BlocklistPush {
                user_bare,
                blocked,
                jids,
            } => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return RelayRemoteUserSideEffectReply {
                        status: RelayRemoteUserSideEffectStatus::Unavailable,
                        carbon_recipients: Vec::new(),
                    };
                };
                crate::server::routes::websocket::handlers::iq::blocking::send_blocking_pushes(
                    &state, &user_bare, blocked, &jids,
                )
                .await;
                (RelayRemoteUserSideEffectStatus::Applied, Vec::new())
            }
        };
        RelayRemoteUserSideEffectReply {
            status,
            carbon_recipients,
        }
    }
}

pub(in super::super) fn apply_remote_resource_presence_to_registry(
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

pub(in super::super) fn apply_remote_resource_state(
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
