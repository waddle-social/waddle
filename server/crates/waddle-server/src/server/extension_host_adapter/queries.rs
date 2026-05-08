use jid::BareJid;
use tracing::warn;
use waddle_xmpp::roster::{RosterItem, Subscription};

use crate::{
    db::blocking::DatabaseBlockingStorage,
    permissions::{Object, ObjectType, Permission},
};

use super::{
    conversions::*, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation,
    HostChannel, HostMucMember, HostPresence, HostPresenceShow, HostRosterItem, HostSpace,
};

impl ExtensionHostAdapter {
    pub async fn list_channels(
        &self,
        invocation: &ExtensionInvocation,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HostChannel>, ExtensionHostAdapterError> {
        let rows = super::super::xmpp_state::list_xmpp_channels(
            self.state.deps.app_state.db_pool.global_actor().clone(),
            limit,
            offset,
        )
        .await
        .map_err(ExtensionHostAdapterError::Storage)?;

        let mut out = Vec::new();
        for row in rows {
            let room = waddle_xmpp::managed_room_jid(&row.id, &self.state.deps.service_domains.muc)
                .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?;
            if self.ensure_not_outcast(invocation, &room).await.is_err() {
                continue;
            }
            if !self
                .allowed(
                    invocation,
                    Object::new(ObjectType::Channel, &row.id),
                    Permission::View,
                )
                .await?
            {
                continue;
            }
            out.push(HostChannel {
                room,
                name: row.name,
            });
        }
        Ok(out)
    }

    pub async fn list_spaces(
        &self,
        invocation: &ExtensionInvocation,
    ) -> Result<Vec<HostSpace>, ExtensionHostAdapterError> {
        let spaces_jid = self.spaces_jid()?;
        let nodes = self
            .state
            .deps
            .protocol
            .pubsub_storage
            .list_nodes(&spaces_jid)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;

        let mut out = Vec::new();
        for node in nodes {
            if !self
                .allowed(
                    invocation,
                    Object::new(ObjectType::Space, &node),
                    Permission::View,
                )
                .await?
            {
                continue;
            }
            if self
                .state
                .deps
                .protocol
                .pubsub_storage
                .get_node(&spaces_jid, &node)
                .await
                .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?
                .is_none()
            {
                continue;
            }
            out.push(HostSpace {
                node,
                service: spaces_jid.clone(),
            });
        }
        Ok(out)
    }

    pub async fn list_muc_members(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
    ) -> Result<Vec<HostMucMember>, ExtensionHostAdapterError> {
        self.authorize_room(invocation, room, Permission::View)
            .await?;
        let snapshot = self.room_snapshot(room).await?;
        let mut members = Vec::new();
        for occupant in snapshot.room.occupants.values() {
            members.push(HostMucMember {
                occupant_jid: occupant_jid(room, &occupant.nick)?,
                nick: occupant.nick.clone(),
                role: host_muc_role(occupant.role),
                affiliation: host_muc_affiliation(occupant.affiliation),
            });
        }
        Ok(members)
    }

    pub async fn presence(
        &self,
        invocation: &ExtensionInvocation,
        target: &BareJid,
    ) -> Result<Vec<HostPresence>, ExtensionHostAdapterError> {
        if invocation.actor_jid.to_bare() != *target {
            let roster = self
                .roster_item(&invocation.actor_jid.to_bare(), target)
                .await?;
            let subscribed = roster.as_ref().is_some_and(|item| {
                matches!(item.subscription, Subscription::To | Subscription::Both)
            });
            if !subscribed {
                return Err(ExtensionHostAdapterError::NotAuthorized);
            }
            let blocking =
                DatabaseBlockingStorage::new(self.state.deps.app_state.db_pool.global().clone());
            let requester_blocks_target = blocking
                .is_blocked(&invocation.actor_jid.to_bare(), target)
                .await
                .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
            if requester_blocks_target {
                return Err(ExtensionHostAdapterError::NotAuthorized);
            }
            let blocked = blocking
                .is_blocked(target, &invocation.actor_jid.to_bare())
                .await
                .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
            if blocked {
                return Err(ExtensionHostAdapterError::NotAuthorized);
            }
        }

        let registry = &self.state.deps.protocol.connection_registry;
        let mut out = Vec::new();
        for (jid, priority) in registry.get_available_resources_for_user(target) {
            let state = registry.get_presence_state(&jid);
            out.push(HostPresence {
                jid,
                show: state
                    .as_ref()
                    .and_then(|state| state.show.as_deref())
                    .map(host_presence_show_str)
                    .unwrap_or(HostPresenceShow::Available),
                status: state.and_then(|state| state.status),
                priority,
            });
        }
        match self
            .state
            .deps
            .protocol
            .sm_session_registry
            .available_detached_presence_states_for_user(target)
            .await
        {
            Ok(states) => {
                out.extend(states.into_iter().map(|(jid, show, status, priority)| {
                    HostPresence {
                        jid,
                        show: show
                            .map(host_presence_show)
                            .unwrap_or(HostPresenceShow::Available),
                        status,
                        priority,
                    }
                }));
            }
            Err(error) => {
                warn!(%error, bare_jid = %target, "failed to list detached presence states");
            }
        }
        Ok(out)
    }

    pub async fn roster(
        &self,
        invocation: &ExtensionInvocation,
        owner: &BareJid,
    ) -> Result<Vec<HostRosterItem>, ExtensionHostAdapterError> {
        if invocation.actor_jid.to_bare() != *owner {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        let storage = self.roster_storage().await?;
        storage
            .get_roster(owner)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?
            .into_iter()
            .map(|row| {
                let item = RosterItem {
                    jid: row.contact_jid.parse().map_err(|error: jid::Error| {
                        ExtensionHostAdapterError::Protocol(error.to_string())
                    })?,
                    name: row.name,
                    subscription: row.subscription.parse().map_err(
                        |error: waddle_xmpp::CoreError| {
                            ExtensionHostAdapterError::Protocol(error.to_string())
                        },
                    )?,
                    ask: row.ask.map(|ask| ask.parse()).transpose().map_err(
                        |error: waddle_xmpp::CoreError| {
                            ExtensionHostAdapterError::Protocol(error.to_string())
                        },
                    )?,
                    approved: row.approved,
                    groups: row.groups,
                };
                Ok(host_roster_item(item))
            })
            .collect()
    }
}
