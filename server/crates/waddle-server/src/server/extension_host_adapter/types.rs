use jid::{BareJid, FullJid, Jid};
use thiserror::Error;
use waddle_extensions::{ReplyTarget, ThreadId};

use crate::auth::Session;

#[derive(Clone)]
pub struct ExtensionInvocation {
    pub session: Option<Session>,
    pub actor_jid: FullJid,
    pub plugin_id: waddle_extensions::PluginId,
    pub source_room: Option<BareJid>,
    pub kind: waddle_extensions::host_tools::InvocationKind,
    pub provider_room_grants: Vec<BareJid>,
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
    pub markup: Vec<waddle_extensions::MessageMarkupSpan>,
    pub extensions: Option<waddle_extensions::ExtensionEnvelope>,
}

#[derive(Debug, Error)]
pub enum ExtensionHostAdapterError {
    #[error("not authorized")]
    NotAuthorized,
    #[error("room not found: {0}")]
    RoomNotFound(BareJid),
    #[error("room actor failed: {0}")]
    RoomActor(String),
    #[error("room ownership cannot currently be verified: {0}")]
    RoomOwnershipUncertain(BareJid),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("protocol failed: {0}")]
    Protocol(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}
