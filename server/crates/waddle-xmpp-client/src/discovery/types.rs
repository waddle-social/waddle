//! Service discovery, upload-slot, roster, and user-search result models.

use jid::BareJid;
use waddle_xmpp_core::roster::RosterItem;

use crate::messaging::MucAffiliation;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
/// XEP-0030 feature namespace advertised by a discovered entity.
pub struct DiscoFeature(
    /// Namespace token advertised by the discovered entity.
    pub String,
);

#[derive(Debug, Clone, PartialEq)]
/// XEP-0030 category, subtype, and display name advertised by an entity.
pub struct DiscoIdentity {
    /// XEP-0030 identity category.
    pub category: String,
    /// XEP-0030 identity subtype advertised by the entity.
    pub identity_type: String,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
/// Parsed identities, feature namespaces, node, and extended forms from disco#info.
pub struct DiscoInfoResult {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Discovery node named by the returned `<query/>`, when present.
    pub node: Option<String>,
    /// Service identities advertised in the discovery response.
    pub identities: Vec<DiscoIdentity>,
    /// Protocol feature namespaces advertised by the entity.
    pub features: Vec<String>,
    /// Extended service-discovery data forms included in the response.
    pub forms: Vec<DiscoDataForm>,
}

impl DiscoInfoResult {
    /// Whether discovery advertised the exact supplied feature namespace.
    #[must_use]
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Whether the matching discovery form field contains the supplied value.
    #[must_use]
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

    /// Borrow the first value of the requested field from the matching discovery data form.
    #[must_use]
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
/// Extended discovery data form and its FORM_TYPE discriminator.
pub struct DiscoDataForm {
    /// FORM_TYPE discriminator identifying the data form's schema.
    pub form_type: Option<String>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<DiscoDataField>,
}

#[derive(Debug, Clone, PartialEq)]
/// Named field and values retained from an extended discovery form.
pub struct DiscoDataField {
    /// XEP-0004 field name whose value is being accessed or validated.
    pub var: String,
    /// Values associated with this field.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
/// Address, node, and optional display name returned by disco#items.
pub struct DiscoItem {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
    /// Discovery node identifying this item at its JID, when present.
    pub node: Option<String>,
}

/// An extension bot that has posted in a room, from the room's
/// [`NODE_WADDLE_ROOM_BOTS`](super::NODE_WADDLE_ROOM_BOTS) disco#items node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomBot {
    /// XMPP address identifying the referenced entity.
    pub jid: jid::BareJid,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
/// Upload and download URLs with the headers required to upload a file.
pub struct UploadSlot {
    /// HTTP URL to which the file should be uploaded.
    pub put_url: String,
    /// HTTP URL from which the uploaded file can be downloaded.
    pub get_url: String,
    /// HTTP headers required when uploading to the slot.
    pub put_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Non-blank PubSub node identifier for a Waddle space.
pub struct SpaceNode(String);

impl SpaceNode {
    /// Create `SpaceNode` from non-blank text, returning `None` for empty or whitespace-only input.
    #[must_use]
    pub fn try_new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            None
        } else {
            Some(Self(value))
        }
    }

    /// Borrow the stored text without allocating.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Display metadata and stable identity of a discovered Waddle space.
pub struct DiscoveredSpace {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: SpaceNode,
    /// Bare JID of the service handling this request.
    pub service_jid: BareJid,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Longer human-readable description.
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Supported channel presentation modes advertised through service discovery.
#[non_exhaustive]
pub enum DiscoveredChannelType {
    /// A text message or plain text UI block.
    Text,
    /// A channel whose messages are restricted to authorized announcers.
    Announcement,
    /// A channel organized into titled discussion threads.
    Forum,
}

impl DiscoveredChannelType {
    /// Borrow the stored text or canonical protocol token without allocating.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Announcement => "announcement",
            Self::Forum => "forum",
        }
    }

    /// Recognize a supported channel type from trimmed discovery metadata.
    #[must_use]
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
/// Room address, metadata, and autojoin policy for a discovered channel.
pub struct DiscoveredChannel {
    /// Identifier used to correlate this value with its protocol or domain record.
    pub id: String,
    /// Bare JID of the room in scope.
    pub room_jid: BareJid,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// Presentation and behavior classification of the channel.
    pub channel_type: DiscoveredChannelType,
    /// Display ordering position reported for the channel.
    pub position: i32,
    /// Identifier of the space associated with this value.
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
/// Spaces and channels assembled from service discovery and bookmarks.
pub struct DiscoveredTopology {
    /// Spaces returned by the discovery or listing operation.
    pub spaces: Vec<DiscoveredSpace>,
    /// Channels returned by this discovery or listing operation.
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
    /// Discovered multi-user chat service address.
    pub muc: BareJid,
    /// Spaces returned by the discovery or listing operation.
    pub spaces: BareJid,
}

/// Waddle-private `<mark-read/>` IQ-set parameters carried under
/// `urn:waddle:inbox:0`. The query/response side of inbox lives on the
/// XEP-0430 surface (`waddle_xmpp_client::inbox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaddleInboxMarkRead {
    /// Other participant in the direct-message conversation.
    pub partner: String,
    /// Conversation thread identifier.
    pub thread: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Roster contacts and optional roster-version token returned by the server.
pub struct RosterResult {
    /// XEP-0115 capability verification hash or roster version token.
    pub ver: Option<String>,
    /// Items carried in the publication or response.
    pub items: Vec<RosterItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Text fields submitted to the XMPP user-search service.
pub struct UserSearchQuery {
    /// Nickname used by the occupant in this room.
    pub nick: Option<String>,
    /// Email address reported by the search or profile response.
    pub email: Option<String>,
    /// First archive identifier in this page.
    pub first: Option<String>,
    /// Last archive identifier in this page.
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Fields supported by the server's user-search form.
pub struct UserSearchForm {
    /// Instructions displayed to the user.
    pub instructions: Option<String>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// JID and display/profile fields of a matching user.
pub struct UserSearchItem {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Nickname used by the occupant in this room.
    pub nick: Option<String>,
    /// Email address reported by the search or profile response.
    pub email: Option<String>,
    /// First archive identifier in this page.
    pub first: Option<String>,
    /// Last archive identifier in this page.
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// User matches returned by the XMPP search service.
pub struct UserSearchResult {
    /// Items carried in the publication or response.
    pub items: Vec<UserSearchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Room affiliation assignment with optional nickname and reason.
pub struct MucAdminAffiliationItem {
    /// XMPP address identifying the referenced entity.
    pub jid: Option<String>,
    /// Nickname used by the occupant in this room.
    pub nick: Option<String>,
    /// Persistent MUC affiliation of this entity.
    pub affiliation: Option<MucAffiliation>,
    /// Human-readable explanation for the operation.
    pub reason: Option<String>,
}
