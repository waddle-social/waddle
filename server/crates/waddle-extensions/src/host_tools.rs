use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use xmpp_parsers::jid::{BareJid, FullJid, Jid};

use crate::types::{DisplayText, PubSubNode, ReplyTarget, RoomJid, StanzaId, ThreadId};

#[derive(Debug, Clone)]
pub struct InvocationContext {
    pub waddle_id: crate::types::WaddleId,
    pub plugin_id: crate::types::PluginId,
    pub requester: Option<BareJid>,
    pub kind: InvocationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationKind {
    MessageHook,
    Command,
    Launch,
}

#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub room: BareJid,
    pub name: Option<DisplayText>,
    pub description: Option<DisplayText>,
}

#[derive(Debug, Clone)]
pub struct ListChannelsRequest;

#[derive(Debug, Clone)]
pub struct ListChannelsResponse {
    pub channels: Vec<ChannelSummary>,
}

#[derive(Debug, Clone)]
pub struct SpaceSummary {
    pub service: BareJid,
    pub node: PubSubNode,
    pub name: Option<DisplayText>,
    pub description: Option<DisplayText>,
    pub channels: Vec<RoomJid>,
}

#[derive(Debug, Clone)]
pub struct ListSpacesRequest;

#[derive(Debug, Clone)]
pub struct ListSpacesResponse {
    pub spaces: Vec<SpaceSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucRole {
    None,
    Visitor,
    Participant,
    Moderator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucAffiliation {
    None,
    Outcast,
    Member,
    Admin,
    Owner,
}

#[derive(Debug, Clone)]
pub struct RoomMember {
    pub room: BareJid,
    pub jid: Jid,
    pub nick: Option<DisplayText>,
    pub role: MucRole,
    pub affiliation: MucAffiliation,
}

#[derive(Debug, Clone)]
pub struct ListRoomMembersRequest {
    pub room: BareJid,
}

#[derive(Debug, Clone)]
pub struct ListRoomMembersResponse {
    pub members: Vec<RoomMember>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceShow {
    Chat,
    Away,
    ExtendedAway,
    DoNotDisturb,
}

#[derive(Debug, Clone)]
pub struct PresenceState {
    pub jid: FullJid,
    pub availability: PresenceAvailability,
    pub show: Option<PresenceShow>,
    pub status: Option<DisplayText>,
    pub priority: i32,
}

#[derive(Debug, Clone)]
pub struct GetPresenceRequest {
    pub subject: BareJid,
}

#[derive(Debug, Clone)]
pub struct GetPresenceResponse {
    pub resources: Vec<PresenceState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterSubscription {
    None,
    To,
    From,
    Both,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterAsk {
    Subscribe,
}

#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub jid: BareJid,
    pub name: Option<DisplayText>,
    pub subscription: RosterSubscription,
    pub ask: Option<RosterAsk>,
    pub groups: Vec<DisplayText>,
}

#[derive(Debug, Clone)]
pub struct GetRosterRequest {
    pub owner: BareJid,
}

#[derive(Debug, Clone)]
pub struct GetRosterResponse {
    pub entries: Vec<RosterEntry>,
}

#[derive(Debug, Clone)]
pub enum MamTarget {
    Room(BareJid),
    Conversation(BareJid),
}

#[derive(Debug, Clone)]
pub struct MamQuery {
    pub target: MamTarget,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub thread_id: Option<ThreadId>,
    pub sender: Option<BareJid>,
    pub text: Option<DisplayText>,
    pub max_results: u32,
}

#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    pub stanza_id: StanzaId,
    pub from: Jid,
    pub to: Jid,
    pub sent_at: DateTime<Utc>,
    pub body: Option<DisplayText>,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
}

#[derive(Debug, Clone)]
pub struct MamQueryResponse {
    pub messages: Vec<ArchivedMessage>,
    pub complete: bool,
}

#[derive(Debug, Clone)]
pub enum MessageTarget {
    Muc(BareJid),
    Direct(BareJid),
}

#[derive(Debug, Clone)]
pub struct SendMessageRequest {
    pub target: MessageTarget,
    pub body: DisplayText,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
}

#[derive(Debug, Clone)]
pub struct SendMessageResponse {
    pub stanza_id: StanzaId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostToolErrorCode {
    Denied,
    InvalidRequest,
    NotFound,
    Unsupported,
    TemporaryFailure,
}

#[derive(Debug, Clone, Error)]
#[error("{code:?}: {message}")]
pub struct HostToolError {
    pub code: HostToolErrorCode,
    pub message: DisplayText,
}

impl HostToolError {
    pub fn denied(message: DisplayText) -> Self {
        Self {
            code: HostToolErrorCode::Denied,
            message,
        }
    }

    pub fn invalid_request(message: DisplayText) -> Self {
        Self {
            code: HostToolErrorCode::InvalidRequest,
            message,
        }
    }
}

#[async_trait]
pub trait ExtensionHostTools: Send + Sync + 'static {
    async fn list_channels(
        &self,
        context: &InvocationContext,
        request: ListChannelsRequest,
    ) -> Result<ListChannelsResponse, HostToolError>;

    async fn list_spaces(
        &self,
        context: &InvocationContext,
        request: ListSpacesRequest,
    ) -> Result<ListSpacesResponse, HostToolError>;

    async fn list_room_members(
        &self,
        context: &InvocationContext,
        request: ListRoomMembersRequest,
    ) -> Result<ListRoomMembersResponse, HostToolError>;

    async fn get_presence(
        &self,
        context: &InvocationContext,
        request: GetPresenceRequest,
    ) -> Result<GetPresenceResponse, HostToolError>;

    async fn get_roster(
        &self,
        context: &InvocationContext,
        request: GetRosterRequest,
    ) -> Result<GetRosterResponse, HostToolError>;

    async fn query_mam(
        &self,
        context: &InvocationContext,
        query: MamQuery,
    ) -> Result<MamQueryResponse, HostToolError>;

    async fn send_message(
        &self,
        context: &InvocationContext,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, HostToolError>;
}

#[derive(Debug, Default)]
pub struct DenyingExtensionHostTools;

#[async_trait]
impl ExtensionHostTools for DenyingExtensionHostTools {
    async fn list_channels(
        &self,
        _context: &InvocationContext,
        _request: ListChannelsRequest,
    ) -> Result<ListChannelsResponse, HostToolError> {
        Err(unavailable())
    }

    async fn list_spaces(
        &self,
        _context: &InvocationContext,
        _request: ListSpacesRequest,
    ) -> Result<ListSpacesResponse, HostToolError> {
        Err(unavailable())
    }

    async fn list_room_members(
        &self,
        _context: &InvocationContext,
        _request: ListRoomMembersRequest,
    ) -> Result<ListRoomMembersResponse, HostToolError> {
        Err(unavailable())
    }

    async fn get_presence(
        &self,
        _context: &InvocationContext,
        _request: GetPresenceRequest,
    ) -> Result<GetPresenceResponse, HostToolError> {
        Err(unavailable())
    }

    async fn get_roster(
        &self,
        _context: &InvocationContext,
        _request: GetRosterRequest,
    ) -> Result<GetRosterResponse, HostToolError> {
        Err(unavailable())
    }

    async fn query_mam(
        &self,
        _context: &InvocationContext,
        _query: MamQuery,
    ) -> Result<MamQueryResponse, HostToolError> {
        Err(unavailable())
    }

    async fn send_message(
        &self,
        _context: &InvocationContext,
        _request: SendMessageRequest,
    ) -> Result<SendMessageResponse, HostToolError> {
        Err(unavailable())
    }
}

fn unavailable() -> HostToolError {
    HostToolError {
        code: HostToolErrorCode::Unsupported,
        message: DisplayText::new("extension host tools are not configured")
            .expect("static host-tool error is non-empty"),
    }
}
