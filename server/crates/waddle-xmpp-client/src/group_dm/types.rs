//! Typed Args / Result payloads of the `urn:waddle:group-dm:*`
//! commands and the mediated-invite history-access extension.

use jid::BareJid;

/// Args for `urn:waddle:group-dm:create:0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmCreateArgs {
    /// Display name; the server rejects empty/overlong names.
    pub name: String,
    /// Full membership INCLUDING the creator. The server dedups and
    /// requires at least two distinct JIDs.
    pub member_jids: Vec<BareJid>,
}

/// Completed `urn:waddle:group-dm:create:0` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmCreated {
    /// Bare JID of the freshly created group-DM room.
    pub room_jid: BareJid,
    /// XEP-0045 room visibility flags echoed by the server when
    /// present (a group DM is hidden + members-only + persistent).
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
    pub persistent: Option<bool>,
}

/// Completed `urn:waddle:group-dm:rename:0` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmRenamed {
    pub room_jid: BareJid,
    /// `None` when the rename cleared the custom name (the server
    /// echoes an empty `name` value in that case).
    pub name: Option<String>,
}

/// Completed `urn:waddle:group-dm:leave:0` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmLeft {
    pub room_jid: BareJid,
    pub left: bool,
}

/// History visibility requested for a newly invited member. Client
/// mirror of the server-side `xep_waddle_group_dm` enum (the client
/// crate must not depend on server crates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupDmHistoryAccess {
    /// New member may only see archive rows from their add boundary.
    /// This is the server default when no extension child is sent.
    FromJoin,
    /// New member may query the full group-DM archive.
    Full,
}

impl GroupDmHistoryAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FromJoin => "from-join",
            Self::Full => "full",
        }
    }
}
