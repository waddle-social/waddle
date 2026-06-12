//! Admin protocol namespace constants for Waddle V1.
//!
//! The admin command surface is XEP-0050 ad-hoc commands under
//! `urn:waddle:admin:*`. We mint our own namespace prefix
//! because no XEP defines a "list users with prefix search"
//! command and `urn:waddle:*` is honestly ours (no XSF
//! registration claimed). The wire shape of the commands and the
//! data forms they carry is plain XEP-0050 + XEP-0004 — the only
//! Waddle-specific bit is the node identifier and the FORM_TYPE.
//!
//! Constants live here so the disco-feature advertisement in
//! `crate::disco::info::server_features` and the command handler
//! in `waddle-server`'s `crate::admin::users_list` reference the
//! same string, preventing the "feature advert and handler node
//! drifted apart" class of bugs.

/// XEP-0050 node identifier and disco feature URI for the
/// admin users-list command. The same string serves both roles
/// per XEP-0050 §"Discovering Support" — a client that wants to
/// know whether `urn:waddle:admin:users:list:0` is
/// available can either disco#info the server for the feature
/// var or disco#items the commands node and look for the entry.
pub const NS_ADMIN_USERS_LIST: &str = "urn:waddle:admin:users:list:0";

// ---------------------------------------------------------------------------
// Admin V2 — Spaces + Channels CRUD command namespaces.
//
// These are the XEP-0050 node identifiers (and matching disco feature URIs
// and `FORM_TYPE` values) for the V2 admin command surface. They follow the
// V1 convention from `NS_ADMIN_USERS_LIST`: the URI doubles as the
// command-node identifier and the XEP-0004 `FORM_TYPE` for the args/result
// data forms carried inside the `<command/>`.
//
// Every constant here MUST also appear in:
//
// - `crate::disco::info::server_features` so clients can discover support
//   via disco#info without first walking the commands list;
// - The `ADVERTISED_FEATURE_EXEMPTIONS` array in
//   `waddle-server/tests/xmpp_e2e_cue.rs`, because the
//   `advertised_features_have_cue_xep_coverage` guard treats anything
//   advertised as needing either a XEP mapping or an explicit exemption,
//   and Waddle-owned `urn:waddle:*` URIs are by definition not XEP
//   features.
// ---------------------------------------------------------------------------

// Spaces (6 commands)

/// `spaces:list` — paginated read of all spaces (community-owner only).
pub const NS_ADMIN_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
/// `spaces:create` — create a new space (community-owner only).
pub const NS_ADMIN_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
/// `spaces:update` — edit a space's name / description / icon URL.
pub const NS_ADMIN_SPACES_UPDATE: &str = "urn:waddle:admin:spaces:update:0";
/// `spaces:delete` — destroy a space and cascade-destroy its channels.
pub const NS_ADMIN_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
/// `spaces:members` — paginated read of a space's membership.
pub const NS_ADMIN_SPACES_MEMBERS: &str = "urn:waddle:admin:spaces:members:0";
/// `spaces:set-role` — change a member's role on a space.
pub const NS_ADMIN_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

// Channels (8 commands)

/// `channels:list` — paginated read of channels, optionally filtered by space.
pub const NS_ADMIN_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
/// `channels:create` — create a new MUC channel; defaults to public.
pub const NS_ADMIN_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
/// `channels:update` — edit a channel's config (name, topic, visibility).
pub const NS_ADMIN_CHANNELS_UPDATE: &str = "urn:waddle:admin:channels:update:0";
/// `channels:delete` — destroy a MUC channel (XEP-0045 §10.9).
pub const NS_ADMIN_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
/// `channels:occupants` — list live occupants of a channel.
pub const NS_ADMIN_CHANNELS_OCCUPANTS: &str = "urn:waddle:admin:channels:occupants:0";
/// `channels:affiliations` — list persistent affiliations on a channel.
pub const NS_ADMIN_CHANNELS_AFFILIATIONS: &str = "urn:waddle:admin:channels:affiliations:0";
/// `channels:set-affiliation` — grant/revoke owner/admin/member/outcast.
pub const NS_ADMIN_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";
/// `channels:kick` — XEP-0045 §9.1 role-change to `none`.
pub const NS_ADMIN_CHANNELS_KICK: &str = "urn:waddle:admin:channels:kick:0";

/// `group-dm:create` — provision a private group DM as a hidden,
/// members-only, persistent XEP-0045 room.
pub const NS_GROUP_DM_CREATE: &str = "urn:waddle:group-dm:create:0";

/// `group-dm:leave` — explicit service-mediated removal of the caller from
/// a private group DM. Presence-unavailable is only an occupancy signal and
/// MUST NOT remove durable membership.
pub const NS_GROUP_DM_LEAVE: &str = "urn:waddle:group-dm:leave:0";

/// Disco feature advertised by group-DM rooms so clients can classify them
/// into the DM surface instead of the channel surface.
pub const NS_GROUP_DM_FEATURE: &str = "urn:waddle:group-dm:0";

/// Persisted channel type for private group-DM MUC rooms.
pub const CHANNEL_TYPE_GROUP_DM: &str = "group-dm";
