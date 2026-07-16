//! `urn:waddle:admin:*` server-administration ad-hoc commands.
//!
//! Shared typed builders and parsers for the Waddle community-admin
//! surface, used by both the wasm chat client and the FFI
//! (Android/Apple) clients — one implementation per command.
//!
//! Wire shape (XEP-0050 §2.4 "Executing Commands"): every command is
//! an `<iq type='set'/>` addressed to the bare server domain carrying
//! `<command xmlns='http://jabber.org/protocol/commands' node='…'
//! action='execute'>` with a XEP-0004 submit form whose hidden
//! `FORM_TYPE` (XEP-0068) mirrors the command node. Completed
//! responses carry `status='completed'` plus a `jabber:x:data`
//! result form: list-style results put rows in `<item/>` children
//! (XEP-0004 §3.4 multiple-items shape) with `next_cursor` as a
//! top-level field; single-entity results carry top-level fields
//! only.
//!
//! The command nodes are legitimately custom (`urn:waddle:*`): no XEP
//! defines paginated server-admin CRUD. Commands: V1 users list, plus
//! the V2 spaces (6) and channels (8) panels shipped server-side in
//! PRs #685 / #691.

mod builders;
mod parse;
mod types;

#[cfg(test)]
mod tests;

pub use builders::{
    build_admin_channels_affiliations_iq, build_admin_channels_create_iq,
    build_admin_channels_delete_iq, build_admin_channels_kick_iq, build_admin_channels_list_iq,
    build_admin_channels_occupants_iq, build_admin_channels_set_affiliation_iq,
    build_admin_channels_update_iq, build_admin_spaces_create_iq, build_admin_spaces_delete_iq,
    build_admin_spaces_list_iq, build_admin_spaces_members_iq, build_admin_spaces_set_role_iq,
    build_admin_spaces_update_iq, build_admin_users_list_iq,
};
pub use parse::{
    parse_admin_channel_ref_result, parse_admin_channels_affiliations_result,
    parse_admin_channels_kick_result, parse_admin_channels_list_result,
    parse_admin_channels_occupants_result, parse_admin_channels_set_affiliation_result,
    parse_admin_space_ref_result, parse_admin_spaces_list_result,
    parse_admin_spaces_members_result, parse_admin_spaces_set_role_result,
    parse_admin_users_list_result, AdminResponseError,
};
pub use types::{
    AdminChannelAffiliationEntry, AdminChannelListEntry, AdminChannelOccupantEntry,
    AdminChannelRef, AdminChannelsAffiliationsArgs, AdminChannelsAffiliationsResult,
    AdminChannelsCreateArgs, AdminChannelsDeleteArgs, AdminChannelsKickArgs,
    AdminChannelsKickResult, AdminChannelsListArgs, AdminChannelsListResult,
    AdminChannelsOccupantsArgs, AdminChannelsOccupantsResult, AdminChannelsSetAffiliationArgs,
    AdminChannelsSetAffiliationResult, AdminChannelsUpdateArgs, AdminSpaceListEntry,
    AdminSpaceMemberEntry, AdminSpaceRef, AdminSpacesCreateArgs, AdminSpacesDeleteArgs,
    AdminSpacesListArgs, AdminSpacesListResult, AdminSpacesMembersArgs, AdminSpacesMembersResult,
    AdminSpacesSetRoleArgs, AdminSpacesSetRoleResult, AdminSpacesUpdateArgs, AdminUserEntry,
    AdminUsersListArgs, AdminUsersPage,
};

/// V1 users panel command node.
pub const NODE_ADMIN_USERS_LIST: &str = "urn:waddle:admin:users:list:0";

/// V2 spaces panel command nodes.
pub const NODE_ADMIN_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
pub const NODE_ADMIN_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
pub const NODE_ADMIN_SPACES_UPDATE: &str = "urn:waddle:admin:spaces:update:0";
pub const NODE_ADMIN_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
pub const NODE_ADMIN_SPACES_MEMBERS: &str = "urn:waddle:admin:spaces:members:0";
pub const NODE_ADMIN_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

/// V2 channels panel command nodes.
pub const NODE_ADMIN_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
pub const NODE_ADMIN_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
pub const NODE_ADMIN_CHANNELS_UPDATE: &str = "urn:waddle:admin:channels:update:0";
pub const NODE_ADMIN_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
pub const NODE_ADMIN_CHANNELS_OCCUPANTS: &str = "urn:waddle:admin:channels:occupants:0";
pub const NODE_ADMIN_CHANNELS_AFFILIATIONS: &str = "urn:waddle:admin:channels:affiliations:0";
pub const NODE_ADMIN_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";
pub const NODE_ADMIN_CHANNELS_KICK: &str = "urn:waddle:admin:channels:kick:0";

/// Literal value the delete commands require in their `confirm`
/// field; mirrors the server-side guard.
pub const CONFIRM_YES: &str = "yes";
