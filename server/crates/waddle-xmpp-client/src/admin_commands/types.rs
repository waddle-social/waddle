//! Typed Args / Result payloads of the `urn:waddle:admin:*` commands.
//!
//! Field names are the wire form-field vars (snake_case) — the wasm
//! client round-trips these structs through `serde_wasm_bindgen`, so
//! renaming a field is a chat-client breaking change.

use serde::{Deserialize, Serialize};

// ── V1 users panel ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUsersListArgs {
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUserEntry {
    pub jid: String,
    pub display_name: Option<String>,
    pub has_owner_hat: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUsersPage {
    pub entries: Vec<AdminUserEntry>,
    pub next_cursor: Option<String>,
}

// ── V2 spaces panel ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesListArgs {
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceListEntry {
    pub space_jid: String,
    pub space_node: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub channel_count: u32,
    pub member_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesListResult {
    pub entries: Vec<AdminSpaceListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesCreateArgs {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceRef {
    pub space_jid: String,
    pub space_node: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesUpdateArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesDeleteArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    /// MUST be the literal string "yes"; mirrors the server-side guard.
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesMembersArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpaceMemberEntry {
    pub jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesMembersResult {
    pub entries: Vec<AdminSpaceMemberEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesSetRoleArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub member_jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSpacesSetRoleResult {
    pub member_jid: String,
    pub role: String,
}

// ── V2 channels panel ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsListArgs {
    pub space_jid: Option<String>,
    pub space_node: Option<String>,
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelListEntry {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
    pub occupant_count: u32,
    pub owner_count: u32,
    pub admin_count: u32,
    pub member_count: u32,
    pub outcast_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsListResult {
    pub entries: Vec<AdminChannelListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsCreateArgs {
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: Option<String>,
    pub space_jid: Option<String>,
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
pub struct AdminChannelRef {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsUpdateArgs {
    pub channel_jid: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub channel_type: Option<String>,
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsDeleteArgs {
    pub channel_jid: String,
    /// MUST be the literal string "yes".
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsOccupantsArgs {
    pub channel_jid: String,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelOccupantEntry {
    pub nick: String,
    pub real_jid: String,
    /// `moderator` | `participant` | `visitor` | `none`.
    pub role: String,
    /// `owner` | `admin` | `member` | `none` | `outcast`.
    pub affiliation: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsOccupantsResult {
    pub entries: Vec<AdminChannelOccupantEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsAffiliationsArgs {
    pub channel_jid: String,
    /// Optional tier filter: `owner` | `admin` | `member` | `outcast` | `none`.
    pub filter: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelAffiliationEntry {
    pub jid: String,
    pub affiliation: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsAffiliationsResult {
    pub entries: Vec<AdminChannelAffiliationEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsSetAffiliationArgs {
    pub channel_jid: String,
    pub member_jid: String,
    pub affiliation: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsSetAffiliationResult {
    pub member_jid: String,
    pub affiliation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsKickArgs {
    pub channel_jid: String,
    pub occupant_jid: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminChannelsKickResult {
    pub occupant_jid: String,
}
