//! Typed Args / Result payloads of the `urn:waddle:admin:*` commands.
//!
//! Field names are the wire form-field vars (snake_case) — the wasm
//! client round-trips these structs through `serde_wasm_bindgen`, so
//! renaming a field is a chat-client breaking change.

use serde::{Deserialize, Serialize};

// ── V1 users panel ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative users list command.
pub struct AdminUsersListArgs {
    /// Prefix recognized when selecting this route from the composer.
    pub prefix: Option<String>,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One user entry returned by an administrative listing.
pub struct AdminUserEntry {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// Whether this value carries owner hat.
    pub has_owner_hat: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// A page of users with the server-provided continuation cursor.
pub struct AdminUsersPage {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminUserEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

// ── V2 spaces panel ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces list command.
pub struct AdminSpacesListArgs {
    /// Prefix recognized when selecting this route from the composer.
    pub prefix: Option<String>,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One space list entry returned by an administrative listing.
pub struct AdminSpaceListEntry {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: String,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// URL of the icon displayed for this route or profile.
    pub icon_url: Option<String>,
    /// Number of channels included in the aggregate.
    pub channel_count: u32,
    /// Number of users with member affiliation.
    pub member_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative spaces list command.
pub struct AdminSpacesListResult {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminSpaceListEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces create command.
pub struct AdminSpacesCreateArgs {
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// URL of the icon displayed for this route or profile.
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Stable identity and display metadata returned after creating or updating a space.
pub struct AdminSpaceRef {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: String,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// URL of the icon displayed for this route or profile.
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces update command.
pub struct AdminSpacesUpdateArgs {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
    /// Longer human-readable description.
    pub description: Option<String>,
    /// URL of the icon displayed for this route or profile.
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces delete command.
pub struct AdminSpacesDeleteArgs {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// MUST be the literal string "yes"; mirrors the server-side guard.
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces members command.
pub struct AdminSpacesMembersArgs {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One space member entry returned by an administrative listing.
pub struct AdminSpaceMemberEntry {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative spaces members command.
pub struct AdminSpacesMembersResult {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminSpaceMemberEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative spaces set role command.
pub struct AdminSpacesSetRoleArgs {
    /// Bare JID of the spaces service.
    pub space_jid: String,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// Bare JID of the listed community or room member.
    pub member_jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative spaces set role command.
pub struct AdminSpacesSetRoleResult {
    /// Bare JID of the listed community or room member.
    pub member_jid: String,
    /// Role assigned to this attendee or participant.
    pub role: String,
}

// ── V2 channels panel ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels list command.
pub struct AdminChannelsListArgs {
    /// Bare JID of the spaces service.
    pub space_jid: Option<String>,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// Prefix recognized when selecting this route from the composer.
    pub prefix: Option<String>,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One channel list entry returned by an administrative listing.
pub struct AdminChannelListEntry {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Current room subject or discussion topic.
    pub topic: Option<String>,
    /// Presentation and behavior classification of the channel.
    pub channel_type: String,
    /// Whether the channel is discoverable to users who are not members.
    pub is_public: bool,
    /// Whether the room requires membership before joining.
    pub members_only: bool,
    /// Number of current room occupants.
    pub occupant_count: u32,
    /// Number of users with owner affiliation.
    pub owner_count: u32,
    /// Number of users with administrator affiliation.
    pub admin_count: u32,
    /// Number of users with member affiliation.
    pub member_count: u32,
    /// Number of users banned from the room.
    pub outcast_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative channels list command.
pub struct AdminChannelsListResult {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminChannelListEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels create command.
pub struct AdminChannelsCreateArgs {
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Current room subject or discussion topic.
    pub topic: Option<String>,
    /// Presentation and behavior classification of the channel.
    pub channel_type: Option<String>,
    /// Bare JID of the spaces service.
    pub space_jid: Option<String>,
    /// PubSub node identifying the space.
    pub space_node: Option<String>,
    /// Defaults to `true` per the V2 spec; the client wrapper passes
    /// this through verbatim. The server enforces the same default
    /// when the field is absent.
    pub is_public: Option<bool>,
    /// Optional XEP-0045 `muc#roomconfig_membersonly`; omitted lets the
    /// server derive the default from `is_public`.
    pub members_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Stable identity and room address returned after creating or updating a channel.
pub struct AdminChannelRef {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Current room subject or discussion topic.
    pub topic: Option<String>,
    /// Presentation and behavior classification of the channel.
    pub channel_type: String,
    /// Whether the channel is discoverable to users who are not members.
    pub is_public: bool,
    /// Whether the room requires membership before joining.
    pub members_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels update command.
pub struct AdminChannelsUpdateArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
    /// Current room subject or discussion topic.
    pub topic: Option<String>,
    /// Presentation and behavior classification of the channel.
    pub channel_type: Option<String>,
    /// Whether the channel is discoverable to users who are not members.
    pub is_public: Option<bool>,
    /// Whether the room requires membership before joining.
    pub members_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels delete command.
pub struct AdminChannelsDeleteArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// MUST be the literal string "yes".
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels occupants command.
pub struct AdminChannelsOccupantsArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One channel occupant entry returned by an administrative listing.
pub struct AdminChannelOccupantEntry {
    /// Nickname used by the occupant in this room.
    pub nick: String,
    /// Real XMPP address disclosed for the room occupant, when permitted.
    pub real_jid: String,
    /// `moderator` | `participant` | `visitor` | `none`.
    pub role: String,
    /// `owner` | `admin` | `member` | `none` | `outcast`.
    pub affiliation: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative channels occupants command.
pub struct AdminChannelsOccupantsResult {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminChannelOccupantEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels affiliations command.
pub struct AdminChannelsAffiliationsArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Optional tier filter: `owner` | `admin` | `member` | `outcast` | `none`.
    pub filter: Option<String>,
    /// Maximum number of entries requested per page.
    pub page_size: Option<u32>,
    /// RSM cursor after which results should be returned.
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One channel affiliation entry returned by an administrative listing.
pub struct AdminChannelAffiliationEntry {
    /// XMPP address identifying the referenced entity.
    pub jid: String,
    /// Persistent MUC affiliation of this entity.
    pub affiliation: String,
    /// Human-readable explanation for the operation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative channels affiliations command.
pub struct AdminChannelsAffiliationsResult {
    /// Entries returned in their protocol-defined order.
    pub entries: Vec<AdminChannelAffiliationEntry>,
    /// Cursor to use when requesting the next page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels set affiliation command.
pub struct AdminChannelsSetAffiliationArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Bare JID of the listed community or room member.
    pub member_jid: String,
    /// Persistent MUC affiliation of this entity.
    pub affiliation: String,
    /// Human-readable explanation for the operation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative channels set affiliation command.
pub struct AdminChannelsSetAffiliationResult {
    /// Bare JID of the listed community or room member.
    pub member_jid: String,
    /// Persistent MUC affiliation of this entity.
    pub affiliation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Arguments submitted to the administrative channels kick command.
pub struct AdminChannelsKickArgs {
    /// Bare JID of the channel's MUC room.
    pub channel_jid: String,
    /// Full room JID identifying the occupant's session nickname.
    pub occupant_jid: String,
    /// Human-readable explanation for the operation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Values returned by the administrative channels kick command.
pub struct AdminChannelsKickResult {
    /// Full room JID identifying the occupant's session nickname.
    pub occupant_jid: String,
}
