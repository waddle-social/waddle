use jid::BareJid;
use waddle_xmpp_core::roster::RosterItem;

use crate::messaging::MucAffiliation;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoFeature(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoIdentity {
    pub category: String,
    pub identity_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoInfoResult {
    pub jid: String,
    pub node: Option<String>,
    pub identities: Vec<DiscoIdentity>,
    pub features: Vec<String>,
    pub forms: Vec<DiscoDataForm>,
}

impl DiscoInfoResult {
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    pub fn has_form_value(&self, form_type: &str, field_var: &str, value: &str) -> bool {
        self.forms
            .iter()
            .filter(|form| form.form_type.as_deref() == Some(form_type))
            .flat_map(|form| &form.fields)
            .any(|field| {
                field.var == field_var
                    && field.values.iter().any(|field_value| field_value == value)
            })
    }

    pub fn form_value(&self, form_type: &str, field_var: &str) -> Option<&str> {
        self.forms
            .iter()
            .filter(|form| form.form_type.as_deref() == Some(form_type))
            .flat_map(|form| &form.fields)
            .find(|field| field.var == field_var)
            .and_then(|field| field.values.first())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoDataForm {
    pub form_type: Option<String>,
    pub fields: Vec<DiscoDataField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoDataField {
    pub var: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoItem {
    pub jid: String,
    pub name: Option<String>,
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadSlot {
    pub put_url: String,
    pub get_url: String,
    pub put_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpaceNode(String);

impl SpaceNode {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            None
        } else {
            Some(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSpace {
    pub id: SpaceNode,
    pub service_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredChannelType {
    Text,
    Announcement,
    Forum,
}

impl DiscoveredChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Announcement => "announcement",
            Self::Forum => "forum",
        }
    }

    pub fn from_metadata(value: &str) -> Option<Self> {
        match value.trim() {
            "text" => Some(Self::Text),
            "announcement" => Some(Self::Announcement),
            "forum" => Some(Self::Forum),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredChannel {
    pub id: String,
    pub room_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: DiscoveredChannelType,
    pub position: i32,
    pub space_id: SpaceNode,
    /// Whether the client should join this room automatically on
    /// session ready. Stamped from the user's XEP-0402 bookmark when
    /// one exists; space bookmarks carry their own `autojoin` attr;
    /// non-bookmarked catalog rooms default to `true` (web parity —
    /// every publicly listed room is joined so its live traffic
    /// flows).
    pub autojoin: bool,
    /// `<conference name='…'>` from the user's XEP-0402 bookmark,
    /// when the room is bookmarked and the bookmark carries a name.
    pub bookmark_name: Option<String>,
    /// Whether the room's disco#info advertises the
    /// `urn:waddle:group-dm:0` feature. Group DMs are joined like any
    /// autojoin room (their live messages must flow) but belong on
    /// the DM surface, not the channel list.
    pub is_group_dm: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredTopology {
    pub spaces: Vec<DiscoveredSpace>,
    pub channels: Vec<DiscoveredChannel>,
}

/// Service JIDs hosting the MUC and Spaces components for a Waddle
/// deployment, resolved via service discovery against the server
/// domain. Falls back to the conventional `muc.<domain>` and
/// `spaces.<domain>` subdomains when the server does not advertise
/// either feature explicitly — so clients keep working on
/// minimally-configured servers and on standalone single-Waddle
/// builds where the components share the bare domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredComponentServices {
    pub muc: BareJid,
    pub spaces: BareJid,
}

/// Waddle-private `<mark-read/>` IQ-set parameters carried under
/// `urn:waddle:inbox:0`. The query/response side of inbox lives on the
/// XEP-0430 surface (`waddle_xmpp_client::inbox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxMarkRead {
    pub partner: String,
    pub thread: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterResult {
    pub ver: Option<String>,
    pub items: Vec<RosterItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserSearchQuery {
    pub nick: Option<String>,
    pub email: Option<String>,
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchForm {
    pub instructions: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchItem {
    pub jid: String,
    pub nick: Option<String>,
    pub email: Option<String>,
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSearchResult {
    pub items: Vec<UserSearchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MucAdminAffiliationItem {
    pub jid: Option<String>,
    pub nick: Option<String>,
    pub affiliation: Option<MucAffiliation>,
    pub reason: Option<String>,
}
