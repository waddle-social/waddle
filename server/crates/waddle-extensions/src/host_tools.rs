//! Typed capabilities available to extensions for messaging, discovery, and account state.

use crate::types::PluginId;
use crate::types::RoomMessageSource;
use crate::types::WaddleId;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use xmpp_parsers::jid::{BareJid, FullJid, Jid};

use crate::types::{
    DisplayText, ExtensionEnvelope, ExtensionPayload, PubSubItemId, PubSubNode, ReplyTarget,
    RoomJid, StanzaId, ThreadId,
};

/// Serializable because a clustered host forwards room sends to the room's
/// owning node with this context; a wire change needs a new relay message id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationContext {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Identifier of the extension owning this value.
    pub plugin_id: PluginId,
    /// Authenticated bare account JID supplying user authority, when this invocation has a user.
    pub requester: Option<BareJid>,
    /// Authoritative source room used to scope host access; absent for events without a room.
    pub source_room: Option<BareJid>,
    /// Event category used when applying invocation-specific host authorization.
    pub kind: InvocationKind,
    /// Explicit rooms a requester-less provider invocation may target.
    pub provider_room_grants: Vec<BareJid>,
}

/// Server-owned canonical authority, kept outside clustered invocation context.
/// Implementations bind the exact source, plugin generation, target, and lease
/// to Foundation's canonical key. Validation never constitutes provider acceptance.
#[async_trait]
pub trait ExtensionDeliveryCapability: Send + Sync + 'static {
    /// Validate that invocation authority and source still match the live canonical delivery key.
    ///
    /// # Errors
    ///
    /// Returns `Denied` for stale or mismatched authority/source, or a categorized host
    /// failure when validation cannot complete.
    async fn validate(
        &self,
        context: &InvocationContext,
        source: &RoomMessageSource,
    ) -> Result<(), HostToolError>;
}

/// Invocation context paired with server-owned delivery authority that is never serialized to
/// the guest.
pub struct DeliveryInvocation {
    /// Host-owned community, plugin, requester, and source-room authority for this delivery.
    pub context: InvocationContext,
    /// Server-owned delivery validator that binds invocation authority to the canonical source.
    pub capability: std::sync::Arc<dyn ExtensionDeliveryCapability>,
}

/// Event category used to scope the authority of host-tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationKind {
    /// Durable room-message observation with host-owned delivery authority.
    RoomMessageObserve,
    /// Message enrichment or non-durable observation hook.
    MessageHook,
    /// Authenticated ad-hoc command execution.
    Command,
    /// Authenticated invocation of a signed launch descriptor.
    Launch,
    /// Provider-originated event that may have no user requester.
    ProviderWebhook,
}

/// Visible channel identity and optional descriptive metadata returned by host discovery.
#[derive(Debug, Clone)]
pub struct ChannelSummary {
    /// Bare JID of the room in scope.
    pub room: BareJid,
    /// Human-readable name advertised to clients.
    pub name: Option<DisplayText>,
    /// Longer human-readable description.
    pub description: Option<DisplayText>,
}

/// Request the channels visible within the current invocation's authority.
#[derive(Debug, Clone)]
pub struct ListChannelsRequest;

/// Channels returned by host discovery after applying access checks.
#[derive(Debug, Clone)]
pub struct ListChannelsResponse {
    /// Channels visible to the invocation, in host-provided order.
    pub channels: Vec<ChannelSummary>,
}

/// Community PubSub space and its associated channels.
#[derive(Debug, Clone)]
pub struct SpaceSummary {
    /// Bare JID of the PubSub service hosting this space.
    pub service: BareJid,
    /// PubSub node identifying this space on its hosting service.
    pub node: PubSubNode,
    /// Human-readable name advertised to clients.
    pub name: Option<DisplayText>,
    /// Longer human-readable description.
    pub description: Option<DisplayText>,
    /// Bare room JIDs associated with this space.
    pub channels: Vec<RoomJid>,
}

/// Request the spaces visible within the current invocation's authority.
#[derive(Debug, Clone)]
pub struct ListSpacesRequest;

/// Spaces returned by host discovery after applying access checks.
#[derive(Debug, Clone)]
pub struct ListSpacesResponse {
    /// Spaces visible to the invocation, in host-provided order.
    pub spaces: Vec<SpaceSummary>,
}

/// Transient room role governing an occupant's current participation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucRole {
    /// No room role, including a departed occupant.
    None,
    /// Present in the room without permission to broadcast messages.
    Visitor,
    /// Present and permitted to broadcast messages to the room.
    Participant,
    /// Present with moderation authority over other occupants.
    Moderator,
}

/// Persistent relationship between a user and a MUC room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucAffiliation {
    /// No persistent affiliation with the room.
    None,
    /// Banned from entering the room.
    Outcast,
    /// Persistent room member.
    Member,
    /// Room administrator with persistent administrative authority.
    Admin,
    /// Room owner with configuration and ownership authority.
    Owner,
}

/// Room occupant or member identity with its role, affiliation, and optional nickname.
#[derive(Debug, Clone)]
pub struct RoomMember {
    /// Bare JID of the room in scope.
    pub room: BareJid,
    /// User or occupant JID disclosed by the room under its visibility policy.
    pub jid: Jid,
    /// Display nickname associated with the room occupant, when known.
    pub nick: Option<DisplayText>,
    /// Role assigned to this attendee or participant.
    pub role: MucRole,
    /// Persistent membership or administrative relationship with the room.
    pub affiliation: MucAffiliation,
}

/// Room whose accessible occupant or membership information is requested.
#[derive(Debug, Clone)]
pub struct ListRoomMembersRequest {
    /// Bare JID of the room in scope.
    pub room: BareJid,
}

/// Room members returned by an authorized host query.
#[derive(Debug, Clone)]
pub struct ListRoomMembersResponse {
    /// Members or occupants visible to this room query.
    pub members: Vec<RoomMember>,
}

/// Whether a specific connected resource is available for messaging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceAvailability {
    /// Resource is available for message delivery.
    Available,
    /// Resource has withdrawn its presence.
    Unavailable,
}

/// Optional XMPP availability detail advertised by an available resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceShow {
    /// Available and interested in chatting.
    Chat,
    /// Temporarily away from the client.
    Away,
    /// Away for an extended period.
    ExtendedAway,
    /// Available but requests not to be disturbed.
    DoNotDisturb,
}

/// Availability and routing priority advertised by one full-JID resource.
#[derive(Debug, Clone)]
pub struct PresenceState {
    /// Full resource JID that advertised this presence.
    pub jid: FullJid,
    /// Whether the full-JID resource is available for message delivery.
    pub availability: PresenceAvailability,
    /// Advertised availability state.
    pub show: Option<PresenceShow>,
    /// Optional human-readable status text.
    pub status: Option<DisplayText>,
    /// Resource priority used by XMPP presence routing.
    pub priority: i32,
}

/// Account whose visible resource presence is requested.
#[derive(Debug, Clone)]
pub struct GetPresenceRequest {
    /// Bare account JID whose resource presence is requested.
    pub subject: BareJid,
}

/// Visible resource presence records returned for an account.
#[derive(Debug, Clone)]
pub struct GetPresenceResponse {
    /// Visible presence records for the account's individual resources.
    pub resources: Vec<PresenceState>,
}

/// Direction of the presence subscription shared with a roster contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterSubscription {
    /// Neither party receives the other's presence through a subscription.
    None,
    /// The roster owner receives this contact's presence.
    To,
    /// This contact receives the roster owner's presence.
    From,
    /// Both parties receive each other's presence.
    Both,
    /// The contact entry is being removed from the roster.
    Remove,
}

/// Pending subscription request associated with a roster contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterAsk {
    /// The roster owner has an outstanding request to receive the contact's presence.
    Subscribe,
}

/// Roster contact identity, presence subscription, and display grouping.
#[derive(Debug, Clone)]
pub struct RosterEntry {
    /// Bare JID of the contact represented by this roster entry.
    pub jid: BareJid,
    /// Contact name stored in the requesting account's roster, when set.
    pub name: Option<DisplayText>,
    /// Direction of the account's presence subscription with this contact.
    pub subscription: RosterSubscription,
    /// Outstanding presence subscription request, when one exists.
    pub ask: Option<RosterAsk>,
    /// User-defined roster group labels assigned to this contact.
    pub groups: Vec<DisplayText>,
}

/// Account whose roster is requested under the invocation's authority.
#[derive(Debug, Clone)]
pub struct GetRosterRequest {
    /// Bare account JID whose roster is being requested.
    pub owner: BareJid,
}

/// Roster contacts returned by an authorized host query.
#[derive(Debug, Clone)]
pub struct GetRosterResponse {
    /// Roster contacts returned by the host in response to this request.
    pub entries: Vec<RosterEntry>,
}

/// Archive scope for an authorized room or direct-conversation history query.
#[derive(Debug, Clone)]
pub enum MamTarget {
    /// Query the archive belonging to this bare MUC room JID.
    Room(BareJid),
    /// Query direct-chat history involving this bare peer JID.
    Conversation(BareJid),
}

/// Archive target and optional time, thread, sender, and text filters.
#[derive(Debug, Clone)]
pub struct MamQuery {
    /// Room archive or direct-conversation scope to query under host authorization.
    pub target: MamTarget,
    /// Optional UTC lower time boundary of the archive query.
    pub start: Option<DateTime<Utc>>,
    /// Optional UTC upper time boundary of the archive query.
    pub end: Option<DateTime<Utc>>,
    /// Conversation thread identifier.
    pub thread_id: Option<ThreadId>,
    /// Optional bare-JID filter restricting archive results to one sender.
    pub sender: Option<BareJid>,
    /// Human-readable text associated with this value.
    pub text: Option<DisplayText>,
    /// Maximum number of archive messages requested from the host.
    pub max_results: u32,
}

/// Message content and routing metadata returned by a host archive query.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    /// Identifier used for stanza correlation.
    pub stanza_id: StanzaId,
    /// XMPP address of the sender.
    pub from: Jid,
    /// XMPP address of the recipient.
    pub to: Jid,
    /// UTC timestamp associated with the archived message.
    pub sent_at: DateTime<Utc>,
    /// Text body of the message.
    pub body: Option<DisplayText>,
    /// Conversation thread identifier.
    pub thread_id: Option<ThreadId>,
    /// Original message identity and optional author addressed by a reply marker.
    pub reply_to: Option<ReplyTarget>,
}

/// Archived messages and whether the requested result set is exhausted.
#[derive(Debug, Clone)]
pub struct MamQueryResponse {
    /// Archived messages matching the query, in host-provided order.
    pub messages: Vec<ArchivedMessage>,
    /// Whether the host reports that the matching archive result set is exhausted.
    pub complete: bool,
}

/// Room or direct-chat recipient for an extension-originated message.
#[derive(Debug, Clone)]
pub enum MessageTarget {
    /// Send a groupchat message to this bare room JID.
    Muc(BareJid),
    /// Send a direct chat message to this bare peer JID.
    Direct(BareJid),
}

/// Supported body annotation carried by host-generated messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageMarkupKind {
    /// Render the selected character range as a block quotation.
    Blockquote,
}

/// Half-open Unicode character range annotated with a supported markup kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageMarkupSpan {
    /// Markup treatment applied to this character range.
    pub kind: MessageMarkupKind,
    /// Inclusive character offset in the message body.
    pub start: u32,
    /// Exclusive character offset in the message body.
    pub end: u32,
}

/// Content, thread, reply target, and extension metadata for a host-mediated send.
#[derive(Debug, Clone)]
pub struct SendMessageRequest {
    /// Bare room or peer JID receiving this host-mediated message.
    pub target: MessageTarget,
    /// Text body of the message.
    pub body: DisplayText,
    /// Conversation thread identifier.
    pub thread_id: Option<ThreadId>,
    /// Original message identity and optional author addressed by a reply marker.
    pub reply_to: Option<ReplyTarget>,
    /// Body spans rendered using the supported message markup vocabulary.
    pub markup: Vec<MessageMarkupSpan>,
    /// Optional extension enrichment envelope attached to the outgoing message.
    pub extensions: Option<ExtensionEnvelope>,
}

/// Stable stanza identity assigned to the host-mediated send.
#[derive(Debug, Clone)]
pub struct SendMessageResponse {
    /// Identifier used for stanza correlation.
    pub stanza_id: StanzaId,
}

/// Extension-owned PubSub node, optional item selection, and result limit.
#[derive(Debug, Clone)]
pub struct PubSubGetItemsRequest {
    /// Extension-owned PubSub node from which to retrieve items.
    pub node: PubSubNode,
    /// Maximum number of retained or returned items.
    pub max_items: Option<u32>,
    /// Item identifiers selected by the request.
    pub item_ids: Vec<PubSubItemId>,
}

/// Stored extension PubSub payload with its item ID and optional publisher.
#[derive(Debug, Clone)]
pub struct PubSubStoredItem {
    /// PubSub item ID assigned to this stored payload.
    pub id: PubSubItemId,
    /// Validated application XML stored under this item ID.
    pub payload: ExtensionPayload,
    /// Bare JID of the item publisher, when supplied.
    pub publisher: Option<BareJid>,
}

/// Stored items returned by an authorized PubSub read.
#[derive(Debug, Clone)]
pub struct PubSubGetItemsResponse {
    /// Items carried in the publication or response.
    pub items: Vec<PubSubStoredItem>,
}

/// Stable categories used to distinguish denied, invalid, unavailable, and retryable host calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HostToolErrorCode {
    /// Invocation authority or capability grants do not permit this operation.
    Denied,
    /// The supplied request violates the host-tool contract.
    InvalidRequest,
    /// The requested room, account, node, or item does not exist.
    NotFound,
    /// This host does not implement the requested service or operation.
    Unsupported,
    /// A transient host failure may allow the operation to succeed on retry.
    TemporaryFailure,
}

/// Categorized host-tool failure and a nonblank human-readable diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{code:?}: {message}")]
pub struct HostToolError {
    /// Stable failure category used by the guest to decide how to handle the error.
    pub code: HostToolErrorCode,
    /// Human-readable explanation of the host failure; protocol control remains in `code`.
    pub message: DisplayText,
}

impl HostToolError {
    /// Create a permission-denied host-tool failure with the supplied diagnostic.
    #[must_use]
    pub fn denied(message: DisplayText) -> Self {
        Self {
            code: HostToolErrorCode::Denied,
            message,
        }
    }

    /// Create a malformed-request host-tool failure with the supplied diagnostic.
    #[must_use]
    pub fn invalid_request(message: DisplayText) -> Self {
        Self {
            code: HostToolErrorCode::InvalidRequest,
            message,
        }
    }
}

/// Host services available to a guest under its invocation context and granted capabilities.
#[async_trait]
pub trait ExtensionHostTools: Send + Sync + 'static {
    /// List channels visible to this invocation under the granted channel-read capability.
    ///
    /// # Errors
    ///
    /// Returns a permission failure when the invocation cannot discover channels, or a
    /// categorized discovery/backend failure.
    async fn list_channels(
        &self,
        context: &InvocationContext,
        request: ListChannelsRequest,
    ) -> Result<ListChannelsResponse, HostToolError>;

    /// List community spaces visible under the invocation's granted space-read capability.
    ///
    /// # Errors
    ///
    /// Returns a permission failure when space discovery is not authorized, or a
    /// categorized discovery/backend failure.
    async fn list_spaces(
        &self,
        context: &InvocationContext,
        request: ListSpacesRequest,
    ) -> Result<ListSpacesResponse, HostToolError>;

    /// Read members or occupants disclosed by the selected room's access and visibility policy.
    ///
    /// # Errors
    ///
    /// Returns a permission failure for inaccessible room membership, or a categorized
    /// missing-room/query failure.
    async fn list_room_members(
        &self,
        context: &InvocationContext,
        request: ListRoomMembersRequest,
    ) -> Result<ListRoomMembersResponse, HostToolError>;

    /// Read visible resource presence for the selected account under the invocation's authority.
    ///
    /// # Errors
    ///
    /// Returns a permission failure for unauthorized presence access, or a categorized
    /// lookup/backend failure.
    async fn get_presence(
        &self,
        context: &InvocationContext,
        request: GetPresenceRequest,
    ) -> Result<GetPresenceResponse, HostToolError>;

    /// Read the selected account's roster under the invocation's authority.
    ///
    /// # Errors
    ///
    /// Returns a permission failure if the invocation cannot read this owner's roster, or
    /// a categorized lookup/backend failure.
    async fn get_roster(
        &self,
        context: &InvocationContext,
        request: GetRosterRequest,
    ) -> Result<GetRosterResponse, HostToolError>;

    /// Query the authorized room or conversation archive using the supplied filters and
    /// result bound.
    ///
    /// # Errors
    ///
    /// Returns a permission failure for inaccessible archive scope, an invalid-query
    /// failure, or a categorized archive/backend failure.
    async fn query_mam(
        &self,
        context: &InvocationContext,
        query: MamQuery,
    ) -> Result<MamQueryResponse, HostToolError>;

    /// Send a message through host-owned XMPP routing after validating recipient scope
    /// and send authority.
    ///
    /// # Errors
    ///
    /// Returns a permission failure for unauthorized recipients, an invalid-request
    /// failure for invalid content, or a categorized routing/backend failure.
    async fn send_message(
        &self,
        context: &InvocationContext,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, HostToolError>;

    /// Read selected items from a permitted extension PubSub node.
    ///
    /// # Errors
    ///
    /// Returns a permission failure for nodes outside the plugin's authority, an
    /// invalid-request failure, or a categorized PubSub/backend failure.
    async fn pubsub_get_items(
        &self,
        context: &InvocationContext,
        request: PubSubGetItemsRequest,
    ) -> Result<PubSubGetItemsResponse, HostToolError>;
}

/// Default host service implementation that returns `Unsupported` for every request.
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

    async fn pubsub_get_items(
        &self,
        _context: &InvocationContext,
        _request: PubSubGetItemsRequest,
    ) -> Result<PubSubGetItemsResponse, HostToolError> {
        Err(unavailable())
    }
}

fn unavailable() -> HostToolError {
    HostToolError {
        code: HostToolErrorCode::Unsupported,
        message: DisplayText::try_new("extension host tools are not configured")
            .expect("static host-tool error is non-empty"),
    }
}
