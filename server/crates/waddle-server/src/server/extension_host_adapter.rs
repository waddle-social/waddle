//! Server-side implementation surface for extension host tools.
//!
//! This module implements the `waddle-extensions` host-tool trait and limits
//! every operation to existing XMPP-native state:
//! channel discovery, XEP-0503 Spaces-on-PubSub, XEP-0045 room actors,
//! RFC 6121 roster/presence, XEP-0313 MAM, and the typed message pipeline.

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use thiserror::Error;
use tracing::warn;
use waddle_extensions::types::PubSubNode as ExtensionPubSubNode;
use waddle_extensions::{
    host_tools as ext_host, DisplayText, ReplyTarget, RoomJid, StanzaId, ThreadId,
};
use waddle_xmpp::{
    mam::{ArchivedMessage as MamArchivedMessage, MamQuery},
    muc::{
        room_actor::{GetSnapshot, RoomActor},
        room_registry_actor::GetRoom,
    },
    protocol::{frame::InboundFrame, Blocklist, InboundEvent, XmppStateMachine},
    roster::{AskType, RosterItem, Subscription},
    Stanza,
};
use xmpp_parsers::message::{Body, Message, MessageType as XmppMessageType};
use xmpp_parsers::presence::Show;

use crate::{
    auth::Session,
    db::blocking::DatabaseBlockingStorage,
    db::roster::DatabaseRosterStorage,
    db::{actor::DbQueryOne, row_value, ValueExt},
    permissions::{CheckPermission, Object, ObjectType, Permission, Subject},
    server::bootstrap_membership::DEPLOYMENT_SERVER_ID,
};

use super::{
    routes::{
        interpret::{self, Deps},
        websocket::WebSocketState,
    },
    xmpp_state::list_xmpp_channels,
};

#[derive(Clone)]
pub struct ExtensionHostAdapter {
    state: Arc<WebSocketState>,
}

#[derive(Clone)]
pub struct ExtensionInvocation {
    pub session: Session,
    pub actor_jid: FullJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostChannel {
    pub room: BareJid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpace {
    pub node: String,
    pub service: BareJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMucMember {
    pub occupant_jid: Jid,
    pub nick: String,
    pub role: HostMucRole,
    pub affiliation: HostMucAffiliation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMucAffiliation {
    Owner,
    Admin,
    Member,
    Outcast,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMucRole {
    Moderator,
    Participant,
    Visitor,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPresence {
    pub jid: FullJid,
    pub show: HostPresenceShow,
    pub status: Option<String>,
    pub priority: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPresenceShow {
    Available,
    Chat,
    Away,
    Dnd,
    Xa,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRosterItem {
    pub jid: BareJid,
    pub name: Option<String>,
    pub subscription: HostRosterSubscription,
    pub ask: Option<HostRosterAsk>,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRosterSubscription {
    None,
    To,
    From,
    Both,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRosterAsk {
    Subscribe,
}

#[derive(Debug, Clone)]
pub enum HostMessageTarget {
    Room(BareJid),
    Direct(Jid),
}

#[derive(Debug, Clone)]
pub struct HostSendMessage {
    pub target: HostMessageTarget,
    pub stanza_id: waddle_extensions::StanzaId,
    pub body: String,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
}

#[derive(Debug, Error)]
pub enum ExtensionHostAdapterError {
    #[error("not authorized")]
    NotAuthorized,
    #[error("room not found: {0}")]
    RoomNotFound(BareJid),
    #[error("room actor failed: {0}")]
    RoomActor(String),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("protocol failed: {0}")]
    Protocol(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl ExtensionHostAdapter {
    pub fn new(state: Arc<WebSocketState>) -> Self {
        Self { state }
    }

    pub async fn list_channels(
        &self,
        invocation: &ExtensionInvocation,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HostChannel>, ExtensionHostAdapterError> {
        let rows = list_xmpp_channels(
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

    pub async fn send_message(
        &self,
        invocation: &ExtensionInvocation,
        request: HostSendMessage,
    ) -> Result<StanzaId, ExtensionHostAdapterError> {
        match request.target {
            HostMessageTarget::Room(room) => {
                self.authorize_room(invocation, &room, Permission::SendMessage)
                    .await?;
                let response = interpret::ExtensionRoomMessage {
                    room: RoomJid::new(room.to_string())
                        .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?,
                    body: DisplayText::new(request.body)
                        .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?,
                    stanza_id: Some(request.stanza_id),
                    thread_id: request.thread_id,
                    reply_to: request.reply_to,
                };
                let deps = self.interpret_deps(Some(&invocation.session));
                let result =
                    interpret::dispatch_extension_bot_groupchat_response(&deps, room, response)
                        .await
                        .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?;
                if result.outcome.close {
                    return Err(ExtensionHostAdapterError::Protocol(
                        "bot groupchat dispatch requested transport close".to_string(),
                    ));
                }
                Ok(result.stanza_id)
            }
            HostMessageTarget::Direct(target) => {
                self.authorize_direct_send(invocation, &target).await?;
                self.dispatch_direct(
                    invocation,
                    target,
                    request.stanza_id.clone(),
                    request.body,
                    request.thread_id,
                    request.reply_to,
                )
                .await?;
                Ok(request.stanza_id)
            }
        }
    }

    async fn dispatch_direct(
        &self,
        invocation: &ExtensionInvocation,
        target: Jid,
        stanza_id: waddle_extensions::StanzaId,
        body: String,
        thread_id: Option<ThreadId>,
        reply_to: Option<ReplyTarget>,
    ) -> Result<(), ExtensionHostAdapterError> {
        let mut message = Message::new(Some(target));
        message.id = Some(stanza_id.as_str().to_string());
        message.type_ = XmppMessageType::Chat;
        message.bodies.insert(String::new(), Body(body));
        if let Some(thread_id) = thread_id.as_ref() {
            waddle_xmpp::xep0201::set_thread_id(&mut message, thread_id.as_str());
        }
        if let Some(reply_to) = reply_to.as_ref() {
            let mut reply = waddle_xmpp::xep::ReplyReference::new(reply_to.id.as_str());
            if let Some(to) = reply_to
                .to
                .as_ref()
                .and_then(|to| to.as_str().parse::<Jid>().ok())
            {
                reply = reply.with_to(to);
            }
            waddle_xmpp::xep::set_reply_payload(&mut message, &reply);
        }

        let mut sm = XmppStateMachine::new(
            self.state.deps.auth_state.xmpp_domain.clone(),
            (*self.state.deps.protocol.dispatcher).clone(),
        );
        sm.transition_to_ready(invocation.actor_jid.clone(), false);
        let blocklist = self
            .state
            .deps
            .protocol
            .blocking_storage
            .list_blocked_jids(&invocation.actor_jid.to_bare())
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        sm.set_blocklist(Blocklist::new(blocklist));
        let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
            Stanza::Message(message),
        ))));
        let deps = self.interpret_deps(Some(&invocation.session));
        let outcome = interpret::interpret(events, &deps).await;
        if outcome.close {
            return Err(ExtensionHostAdapterError::Protocol(
                "direct message dispatch requested transport close".to_string(),
            ));
        }
        Ok(())
    }

    fn spaces_jid(&self) -> Result<BareJid, ExtensionHostAdapterError> {
        self.state
            .deps
            .service_domains
            .spaces
            .parse()
            .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))
    }

    fn interpret_deps<'a>(&'a self, session: Option<&'a Session>) -> Deps<'a> {
        Deps {
            connection_registry: &self.state.deps.protocol.connection_registry,
            sm_session_registry: Some(&self.state.deps.protocol.sm_session_registry),
            mam_storage: Some(&self.state.deps.protocol.mam_storage),
            inbox_storage: Some(&self.state.deps.protocol.inbox_storage),
            extension_manager: Some(&self.state.deps.protocol.extension_manager),
            room_registry: Some(&self.state.deps.protocol.room_registry),
            web_socket_state: Some(&self.state),
            authenticated_session: session,
            local_domain: self.state.deps.auth_state.xmpp_domain.as_str(),
            blocking_storage: Some(&self.state.deps.protocol.blocking_storage),
            message_dispatcher: Some(&self.state.deps.protocol.dispatcher),
        }
    }

    async fn roster_storage(&self) -> Result<DatabaseRosterStorage, ExtensionHostAdapterError> {
        Ok(DatabaseRosterStorage::new(
            self.state.deps.app_state.db_pool.global().clone(),
        ))
    }

    async fn roster_item(
        &self,
        owner: &BareJid,
        contact: &BareJid,
    ) -> Result<Option<RosterItem>, ExtensionHostAdapterError> {
        let storage = self.roster_storage().await?;
        let row = storage
            .get_roster_item(owner, contact)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        row.map(|row| {
            Ok(RosterItem {
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
            })
        })
        .transpose()
    }

    async fn room_actor(
        &self,
        room: &BareJid,
    ) -> Result<ActorRef<RoomActor>, ExtensionHostAdapterError> {
        self.state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
            .map_err(|error| ExtensionHostAdapterError::RoomActor(format!("{error:?}")))?
            .ok_or_else(|| ExtensionHostAdapterError::RoomNotFound(room.clone()))
    }

    async fn room_snapshot(
        &self,
        room: &BareJid,
    ) -> Result<waddle_xmpp::muc::room_actor::RoomSnapshot, ExtensionHostAdapterError> {
        self.room_actor(room)
            .await?
            .ask(GetSnapshot)
            .await
            .map_err(|error| ExtensionHostAdapterError::RoomActor(format!("{error:?}")))
    }

    async fn authorize_archive(
        &self,
        invocation: &ExtensionInvocation,
        archive: &BareJid,
    ) -> Result<(), ExtensionHostAdapterError> {
        if archive == &invocation.actor_jid.to_bare() {
            return Ok(());
        }
        if archive.domain().as_str() == self.state.deps.service_domains.muc {
            return self
                .authorize_room(invocation, archive, Permission::View)
                .await;
        }
        Err(ExtensionHostAdapterError::NotAuthorized)
    }

    async fn authorize_room(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
        permission: Permission,
    ) -> Result<(), ExtensionHostAdapterError> {
        self.ensure_not_outcast(invocation, room).await?;
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        if self
            .allowed(
                invocation,
                Object::new(ObjectType::Channel, channel_id),
                permission,
            )
            .await?
        {
            Ok(())
        } else {
            Err(ExtensionHostAdapterError::NotAuthorized)
        }
    }

    async fn ensure_not_outcast(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
    ) -> Result<(), ExtensionHostAdapterError> {
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        let subject = Subject::user(&invocation.session.user_id);
        if self
            .permission_allowed(
                subject,
                Object::new(ObjectType::Channel, channel_id),
                Permission::Custom("outcast".into()),
            )
            .await?
        {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        Ok(())
    }

    async fn permission_allowed(
        &self,
        subject: Subject,
        object: Object,
        permission: Permission,
    ) -> Result<bool, ExtensionHostAdapterError> {
        self.state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject,
                permission,
                object,
            })
            .await
            .map(|response| response.allowed)
            .map_err(|error| ExtensionHostAdapterError::Protocol(format!("{error:?}")))
    }

    async fn authorize_direct_send(
        &self,
        invocation: &ExtensionInvocation,
        target: &Jid,
    ) -> Result<(), ExtensionHostAdapterError> {
        let target_bare = target.to_bare();
        if target_bare.domain().as_str() != self.state.deps.auth_state.xmpp_domain {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        let roster = self
            .roster_item(&invocation.actor_jid.to_bare(), &target_bare)
            .await?;
        if roster
            .as_ref()
            .is_some_and(|item| matches!(item.subscription, Subscription::To | Subscription::Both))
        {
            Ok(())
        } else {
            Err(ExtensionHostAdapterError::NotAuthorized)
        }
    }

    async fn allowed(
        &self,
        invocation: &ExtensionInvocation,
        object: Object,
        permission: Permission,
    ) -> Result<bool, ExtensionHostAdapterError> {
        let subject = Subject::user(&invocation.session.user_id);
        if self
            .permission_allowed(subject.clone(), object, permission)
            .await?
        {
            return Ok(true);
        }

        self.permission_allowed(
            subject,
            Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            Permission::Owner,
        )
        .await
    }
}

#[async_trait]
impl ext_host::ExtensionHostTools for ExtensionHostAdapter {
    async fn list_channels(
        &self,
        context: &ext_host::InvocationContext,
        _request: ext_host::ListChannelsRequest,
    ) -> Result<ext_host::ListChannelsResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let channels = self
            .list_channels(&invocation, 500, 0)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|channel| ext_host::ChannelSummary {
                room: channel.room,
                name: DisplayText::new(channel.name).ok(),
                description: None,
            })
            .collect();
        Ok(ext_host::ListChannelsResponse { channels })
    }

    async fn list_spaces(
        &self,
        context: &ext_host::InvocationContext,
        _request: ext_host::ListSpacesRequest,
    ) -> Result<ext_host::ListSpacesResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let spaces = self
            .list_spaces(&invocation)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .filter_map(|space| {
                Some(ext_host::SpaceSummary {
                    service: space.service,
                    node: ExtensionPubSubNode::new(space.node).ok()?,
                    name: None,
                    description: None,
                    channels: Vec::new(),
                })
            })
            .collect();
        Ok(ext_host::ListSpacesResponse { spaces })
    }

    async fn list_room_members(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListRoomMembersRequest,
    ) -> Result<ext_host::ListRoomMembersResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let occupants = self
            .list_muc_members(&invocation, &request.room)
            .await
            .map_err(host_tool_error)?;
        let members = occupants
            .into_iter()
            .map(|member| ext_host::RoomMember {
                room: request.room.clone(),
                jid: member.occupant_jid,
                nick: DisplayText::new(member.nick).ok(),
                role: ext_muc_role(member.role),
                affiliation: ext_muc_affiliation(member.affiliation),
            })
            .collect();
        Ok(ext_host::ListRoomMembersResponse { members })
    }

    async fn get_presence(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetPresenceRequest,
    ) -> Result<ext_host::GetPresenceResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let resources = self
            .presence(&invocation, &request.subject)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|presence| ext_host::PresenceState {
                jid: presence.jid,
                availability: ext_host::PresenceAvailability::Available,
                show: ext_presence_show(presence.show),
                status: presence
                    .status
                    .and_then(|status| DisplayText::new(status).ok()),
                priority: i32::from(presence.priority),
            })
            .collect();
        Ok(ext_host::GetPresenceResponse { resources })
    }

    async fn get_roster(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetRosterRequest,
    ) -> Result<ext_host::GetRosterResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let entries = self
            .roster(&invocation, &request.owner)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|entry| ext_host::RosterEntry {
                jid: entry.jid,
                name: entry.name.and_then(|name| DisplayText::new(name).ok()),
                subscription: ext_roster_subscription(entry.subscription),
                ask: entry.ask.map(|_| ext_host::RosterAsk::Subscribe),
                groups: entry
                    .groups
                    .into_iter()
                    .filter_map(|group| DisplayText::new(group).ok())
                    .collect(),
            })
            .collect();
        Ok(ext_host::GetRosterResponse { entries })
    }

    async fn query_mam(
        &self,
        context: &ext_host::InvocationContext,
        query: ext_host::MamQuery,
    ) -> Result<ext_host::MamQueryResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let requester = invocation.actor_jid.to_bare();
        let (archive, with) = match query.target {
            ext_host::MamTarget::Room(jid) => (jid, query.sender),
            ext_host::MamTarget::Conversation(peer) => (requester, Some(peer)),
        };
        self.authorize_archive(&invocation, &archive)
            .await
            .map_err(host_tool_error)?;
        let xmpp_query = MamQuery {
            start: query.start,
            end: query.end,
            with: with.map(|jid| jid.to_string()),
            thread_id: query
                .thread_id
                .and_then(|thread_id| waddle_xmpp::mam::ThreadId::new(thread_id.as_str())),
            fulltext: query
                .text
                .and_then(|text| waddle_xmpp::mam::RichText::new(text.into_string())),
            max: Some(query.max_results),
            filter_before_id: None,
            filter_after_id: None,
            ids: Vec::new(),
            before_id: Some(String::new()),
            after_id: None,
        };
        let result = self
            .state
            .deps
            .protocol
            .mam_storage
            .query_messages(archive.to_string().as_str(), &xmpp_query)
            .await
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let messages = result
            .messages
            .into_iter()
            .filter_map(ext_archived_message)
            .collect();
        Ok(ext_host::MamQueryResponse {
            messages,
            complete: result.complete,
        })
    }

    async fn send_message(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::SendMessageRequest,
    ) -> Result<ext_host::SendMessageResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let target = match request.target {
            ext_host::MessageTarget::Muc(room) => HostMessageTarget::Room(room),
            ext_host::MessageTarget::Direct(jid) => HostMessageTarget::Direct(Jid::from(jid)),
        };
        let stanza_id = waddle_extensions::StanzaId::new(uuid::Uuid::new_v4().to_string())
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Protocol(error.to_string()))
            })?;
        let stanza_id = self
            .send_message(
                &invocation,
                HostSendMessage {
                    target,
                    stanza_id: stanza_id.clone(),
                    body: request.body.into_string(),
                    thread_id: request.thread_id,
                    reply_to: request.reply_to,
                },
            )
            .await
            .map_err(host_tool_error)?;
        Ok(ext_host::SendMessageResponse { stanza_id })
    }
}

impl ExtensionHostAdapter {
    async fn invocation_for_context(
        &self,
        context: &ext_host::InvocationContext,
    ) -> Result<ExtensionInvocation, ext_host::HostToolError> {
        let Some(requester) = context.requester.as_ref() else {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        };
        self.invocation_for_requester(requester).await
    }

    async fn invocation_for_requester(
        &self,
        requester: &BareJid,
    ) -> Result<ExtensionInvocation, ext_host::HostToolError> {
        let Some(localpart) = requester.node() else {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        };
        if requester.domain().as_str() != self.state.deps.auth_state.xmpp_domain {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        }
        let row = self
            .state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQueryOne {
                sql: "SELECT id, username, xmpp_localpart FROM users WHERE xmpp_localpart = ? LIMIT 1"
                    .to_string(),
                params: vec![localpart.as_str().into()],
            })
            .await
            .map_err(|error| host_tool_error(ExtensionHostAdapterError::Storage(format!("{error:?}"))))?
            .ok_or_else(|| host_tool_error(ExtensionHostAdapterError::NotAuthorized))?;
        let user_id = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let username = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let xmpp_localpart = row_value(&row, 2)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let actor_jid = requester
            .clone()
            .with_resource_str("extension-host")
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Protocol(error.to_string()))
            })?;
        Ok(ExtensionInvocation {
            session: Session::new(&user_id, &username, &xmpp_localpart),
            actor_jid,
        })
    }
}

fn host_tool_error(error: ExtensionHostAdapterError) -> ext_host::HostToolError {
    let code = match error {
        ExtensionHostAdapterError::NotAuthorized => ext_host::HostToolErrorCode::Denied,
        ExtensionHostAdapterError::RoomNotFound(_) => ext_host::HostToolErrorCode::NotFound,
        ExtensionHostAdapterError::Unsupported(_) => ext_host::HostToolErrorCode::Unsupported,
        ExtensionHostAdapterError::RoomActor(_)
        | ExtensionHostAdapterError::Storage(_)
        | ExtensionHostAdapterError::Protocol(_) => ext_host::HostToolErrorCode::TemporaryFailure,
    };
    ext_host::HostToolError {
        code,
        message: DisplayText::new(error.to_string()).unwrap_or_else(|_| {
            DisplayText::new("extension host tool failed").expect("static text")
        }),
    }
}

fn ext_muc_affiliation(affiliation: HostMucAffiliation) -> ext_host::MucAffiliation {
    match affiliation {
        HostMucAffiliation::Owner => ext_host::MucAffiliation::Owner,
        HostMucAffiliation::Admin => ext_host::MucAffiliation::Admin,
        HostMucAffiliation::Member => ext_host::MucAffiliation::Member,
        HostMucAffiliation::Outcast => ext_host::MucAffiliation::Outcast,
        HostMucAffiliation::None => ext_host::MucAffiliation::None,
    }
}

fn ext_muc_role(role: HostMucRole) -> ext_host::MucRole {
    match role {
        HostMucRole::Moderator => ext_host::MucRole::Moderator,
        HostMucRole::Participant => ext_host::MucRole::Participant,
        HostMucRole::Visitor => ext_host::MucRole::Visitor,
        HostMucRole::None => ext_host::MucRole::None,
    }
}

fn ext_presence_show(show: HostPresenceShow) -> Option<ext_host::PresenceShow> {
    match show {
        HostPresenceShow::Available => None,
        HostPresenceShow::Chat => Some(ext_host::PresenceShow::Chat),
        HostPresenceShow::Away => Some(ext_host::PresenceShow::Away),
        HostPresenceShow::Dnd => Some(ext_host::PresenceShow::DoNotDisturb),
        HostPresenceShow::Xa => Some(ext_host::PresenceShow::ExtendedAway),
    }
}

fn ext_roster_subscription(subscription: HostRosterSubscription) -> ext_host::RosterSubscription {
    match subscription {
        HostRosterSubscription::None => ext_host::RosterSubscription::None,
        HostRosterSubscription::To => ext_host::RosterSubscription::To,
        HostRosterSubscription::From => ext_host::RosterSubscription::From,
        HostRosterSubscription::Both => ext_host::RosterSubscription::Both,
        HostRosterSubscription::Remove => ext_host::RosterSubscription::Remove,
    }
}

fn ext_archived_message(message: MamArchivedMessage) -> Option<ext_host::ArchivedMessage> {
    Some(ext_host::ArchivedMessage {
        stanza_id: waddle_extensions::StanzaId::new(message.id).ok()?,
        from: message.from.parse().ok()?,
        to: message.to.parse().ok()?,
        sent_at: message.timestamp,
        body: DisplayText::new(message.body).ok(),
        thread_id: message
            .thread_id
            .and_then(|thread| waddle_extensions::ThreadId::new(thread.as_str()).ok()),
        reply_to: message.reply_to_id.and_then(|id| {
            Some(waddle_extensions::ReplyTarget {
                id: waddle_extensions::StanzaId::new(id).ok()?,
                to: message
                    .reply_to_jid
                    .and_then(|jid| waddle_extensions::FullJidValue::new(jid).ok()),
            })
        }),
    })
}

fn host_muc_affiliation(affiliation: waddle_xmpp::Affiliation) -> HostMucAffiliation {
    match affiliation {
        waddle_xmpp::Affiliation::Owner => HostMucAffiliation::Owner,
        waddle_xmpp::Affiliation::Admin => HostMucAffiliation::Admin,
        waddle_xmpp::Affiliation::Member => HostMucAffiliation::Member,
        waddle_xmpp::Affiliation::Outcast => HostMucAffiliation::Outcast,
        waddle_xmpp::Affiliation::None => HostMucAffiliation::None,
    }
}

fn host_muc_role(role: waddle_xmpp::Role) -> HostMucRole {
    match role {
        waddle_xmpp::Role::Moderator => HostMucRole::Moderator,
        waddle_xmpp::Role::Participant => HostMucRole::Participant,
        waddle_xmpp::Role::Visitor => HostMucRole::Visitor,
        waddle_xmpp::Role::None => HostMucRole::None,
    }
}

fn host_presence_show(show: Show) -> HostPresenceShow {
    match show {
        Show::Chat => HostPresenceShow::Chat,
        Show::Away => HostPresenceShow::Away,
        Show::Dnd => HostPresenceShow::Dnd,
        Show::Xa => HostPresenceShow::Xa,
    }
}

fn host_presence_show_str(show: &str) -> HostPresenceShow {
    match show {
        "chat" => HostPresenceShow::Chat,
        "away" => HostPresenceShow::Away,
        "dnd" => HostPresenceShow::Dnd,
        "xa" => HostPresenceShow::Xa,
        _ => HostPresenceShow::Available,
    }
}

fn host_roster_item(item: RosterItem) -> HostRosterItem {
    HostRosterItem {
        jid: item.jid,
        name: item.name,
        subscription: match item.subscription {
            Subscription::None => HostRosterSubscription::None,
            Subscription::To => HostRosterSubscription::To,
            Subscription::From => HostRosterSubscription::From,
            Subscription::Both => HostRosterSubscription::Both,
            Subscription::Remove => HostRosterSubscription::Remove,
        },
        ask: item.ask.map(|ask| match ask {
            AskType::Subscribe => HostRosterAsk::Subscribe,
        }),
        groups: item.groups,
    }
}

fn occupant_jid(room: &BareJid, nick: &str) -> Result<Jid, ExtensionHostAdapterError> {
    format!("{room}/{nick}")
        .parse()
        .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_xmpp_presence_show_to_host_enum() {
        assert_eq!(host_presence_show(Show::Chat), HostPresenceShow::Chat);
        assert_eq!(host_presence_show(Show::Away), HostPresenceShow::Away);
        assert_eq!(host_presence_show(Show::Dnd), HostPresenceShow::Dnd);
        assert_eq!(host_presence_show(Show::Xa), HostPresenceShow::Xa);
    }

    #[test]
    fn maps_roster_subscription_without_stringly_state() {
        let item = RosterItem {
            jid: "bob@example.com".parse().expect("jid"),
            name: Some("Bob".to_string()),
            subscription: Subscription::Both,
            ask: Some(AskType::Subscribe),
            approved: false,
            groups: vec!["Friends".to_string()],
        };

        let mapped = host_roster_item(item);
        assert_eq!(mapped.jid.to_string(), "bob@example.com");
        assert_eq!(mapped.subscription, HostRosterSubscription::Both);
        assert_eq!(mapped.ask, Some(HostRosterAsk::Subscribe));
        assert_eq!(mapped.groups, vec!["Friends"]);
    }
}
