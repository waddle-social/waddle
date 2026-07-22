//! `urn:waddle:admin:*` community-admin command exports (XEP-0050
//! execute requests to the bare server domain).
//!
//! Mirrors the wasm chat client's V1 users panel + V2 spaces/channels
//! panels one-to-one: builders and parsers live in the shared
//! `waddle_xmpp_client::admin_commands` module; this file adapts the
//! typed uniffi Records and maps the closed role/affiliation domains
//! onto the existing FFI enums. The delete commands hardcode the
//! server-required `confirm="yes"` literal — calling the export *is*
//! the confirmation (web wrapper parity).

use waddle_xmpp_client::admin_commands as core;
use waddle_xmpp_client::messaging::MucAffiliation as CoreAffiliation;
use waddle_xmpp_client::messaging::MucRole as CoreRole;

use crate::boundary_convert::jid_domain;
use crate::convert::{muc_affiliation_to_ffi, muc_role_to_ffi};
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleMucAffiliation, WaddleMucRole};

// ── Typed FFI payloads ───────────────────────────────────────────────

/// V1 users-list row.
#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminUserEntry {
    pub jid: String,
    pub display_name: Option<String>,
    /// Whether the user wears the community-owner hat (XEP-0317).
    pub has_owner_hat: bool,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminUsersPage {
    pub entries: Vec<WaddleAdminUserEntry>,
    pub next_cursor: Option<String>,
}

/// Space pubsub roster role (`urn:waddle:admin:spaces:*`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaddleSpaceRole {
    Owner,
    Admin,
    Member,
    None,
}

#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct WaddleAdminSpacesListArgs {
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpaceEntry {
    pub space_jid: String,
    pub space_node: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub channel_count: u32,
    pub member_count: u32,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesListPage {
    pub entries: Vec<WaddleAdminSpaceEntry>,
    pub next_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesCreateArgs {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpaceRef {
    pub space_jid: String,
    pub space_node: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesUpdateArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesMembersArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpaceMemberEntry {
    pub jid: String,
    pub role: WaddleSpaceRole,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesMembersPage {
    pub entries: Vec<WaddleAdminSpaceMemberEntry>,
    pub next_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminSpacesSetRoleResult {
    pub member_jid: String,
    pub role: WaddleSpaceRole,
}

#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct WaddleAdminChannelsListArgs {
    pub space_jid: Option<String>,
    pub space_node: Option<String>,
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelListEntry {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    /// `text` | `announcement` | `forum` | `group_dm` (open set).
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
    pub occupant_count: u32,
    pub owner_count: u32,
    pub admin_count: u32,
    pub member_count: u32,
    pub outcast_count: u32,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsListPage {
    pub entries: Vec<WaddleAdminChannelListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsCreateArgs {
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: Option<String>,
    pub space_jid: Option<String>,
    pub space_node: Option<String>,
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelRef {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsUpdateArgs {
    pub channel_jid: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub channel_type: Option<String>,
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsOccupantsArgs {
    pub channel_jid: String,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelOccupantEntry {
    pub nick: String,
    pub real_jid: String,
    pub role: WaddleMucRole,
    pub affiliation: WaddleMucAffiliation,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsOccupantsPage {
    pub entries: Vec<WaddleAdminChannelOccupantEntry>,
    pub next_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsAffiliationsArgs {
    pub channel_jid: String,
    /// Optional tier filter.
    pub filter: Option<WaddleMucAffiliation>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelAffiliationEntry {
    pub jid: String,
    pub affiliation: WaddleMucAffiliation,
    pub reason: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsAffiliationsPage {
    pub entries: Vec<WaddleAdminChannelAffiliationEntry>,
    pub next_cursor: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsSetAffiliationResult {
    pub member_jid: String,
    pub affiliation: WaddleMucAffiliation,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct WaddleAdminChannelsKickResult {
    pub occupant_jid: String,
}

// ── Wire mappings ────────────────────────────────────────────────────

pub(crate) fn space_role_wire(role: WaddleSpaceRole) -> &'static str {
    match role {
        WaddleSpaceRole::Owner => "owner",
        WaddleSpaceRole::Admin => "admin",
        WaddleSpaceRole::Member => "member",
        WaddleSpaceRole::None => "none",
    }
}

pub(crate) fn space_role_from_wire(value: &str) -> Result<WaddleSpaceRole, WaddleError> {
    match value {
        "owner" => Ok(WaddleSpaceRole::Owner),
        "admin" => Ok(WaddleSpaceRole::Admin),
        "member" => Ok(WaddleSpaceRole::Member),
        "none" => Ok(WaddleSpaceRole::None),
        _ => Err(WaddleError::MalformedResponse),
    }
}

pub(crate) fn muc_affiliation_wire(value: WaddleMucAffiliation) -> &'static str {
    match value {
        WaddleMucAffiliation::Owner => CoreAffiliation::Owner.as_str(),
        WaddleMucAffiliation::Admin => CoreAffiliation::Admin.as_str(),
        WaddleMucAffiliation::Member => CoreAffiliation::Member.as_str(),
        WaddleMucAffiliation::Outcast => CoreAffiliation::Outcast.as_str(),
        WaddleMucAffiliation::None => CoreAffiliation::None.as_str(),
    }
}

pub(crate) fn muc_affiliation_from_wire(value: &str) -> Result<WaddleMucAffiliation, WaddleError> {
    CoreAffiliation::from_attr(value)
        .map(muc_affiliation_to_ffi)
        .ok_or(WaddleError::MalformedResponse)
}

pub(crate) fn muc_role_from_wire(value: &str) -> Result<WaddleMucRole, WaddleError> {
    CoreRole::from_attr(value)
        .map(muc_role_to_ffi)
        .ok_or(WaddleError::MalformedResponse)
}

fn admin_parse_error(_: core::AdminResponseError) -> WaddleError {
    WaddleError::MalformedResponse
}

// ── Result conversions ───────────────────────────────────────────────

fn users_page_to_ffi(page: core::AdminUsersPage) -> WaddleAdminUsersPage {
    WaddleAdminUsersPage {
        entries: page
            .entries
            .into_iter()
            .map(|entry| WaddleAdminUserEntry {
                jid: entry.jid,
                display_name: entry.display_name,
                has_owner_hat: entry.has_owner_hat,
            })
            .collect(),
        next_cursor: page.next_cursor,
    }
}

fn space_entry_to_ffi(entry: core::AdminSpaceListEntry) -> WaddleAdminSpaceEntry {
    WaddleAdminSpaceEntry {
        space_jid: entry.space_jid,
        space_node: entry.space_node,
        name: entry.name,
        description: entry.description,
        icon_url: entry.icon_url,
        channel_count: entry.channel_count,
        member_count: entry.member_count,
    }
}

fn space_ref_to_ffi(space: core::AdminSpaceRef) -> WaddleAdminSpaceRef {
    WaddleAdminSpaceRef {
        space_jid: space.space_jid,
        space_node: space.space_node,
        name: space.name,
        description: space.description,
        icon_url: space.icon_url,
    }
}

fn channel_entry_to_ffi(entry: core::AdminChannelListEntry) -> WaddleAdminChannelListEntry {
    WaddleAdminChannelListEntry {
        channel_jid: entry.channel_jid,
        name: entry.name,
        topic: entry.topic,
        channel_type: entry.channel_type,
        is_public: entry.is_public,
        members_only: entry.members_only,
        occupant_count: entry.occupant_count,
        owner_count: entry.owner_count,
        admin_count: entry.admin_count,
        member_count: entry.member_count,
        outcast_count: entry.outcast_count,
    }
}

fn channel_ref_to_ffi(channel: core::AdminChannelRef) -> WaddleAdminChannelRef {
    WaddleAdminChannelRef {
        channel_jid: channel.channel_jid,
        name: channel.name,
        topic: channel.topic,
        channel_type: channel.channel_type,
        is_public: channel.is_public,
        members_only: channel.members_only,
    }
}

impl WaddleClient {
    /// Send one admin command IQ and hand back the raw result element.
    async fn send_admin_command(
        &self,
        iq: minidom::Element,
    ) -> Result<minidom::Element, WaddleError> {
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        send_iq_with_timeout(&handle, iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))
    }

    fn server_domain(&self) -> String {
        jid_domain(&self.config.jid).to_string()
    }
}

// ── Exported commands ────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// `true` iff the authenticated user is the community owner —
    /// i.e. the server accepts a `page_size=1` probe of the V1 users
    /// command. Any error (including `forbidden` and transport
    /// failures) resolves to `false`: the probe is best-effort
    /// gating for the admin UI entry point, never an authorization
    /// mechanism — the server re-checks every actual command.
    pub async fn is_community_owner(&self) -> bool {
        let args = core::AdminUsersListArgs {
            prefix: None,
            page_size: Some(1),
            after_cursor: None,
        };
        let iq = core::build_admin_users_list_iq(&self.server_domain(), &args);
        self.send_admin_command(iq).await.is_ok()
    }

    /// `urn:waddle:admin:users:list:0`: paginated user directory for
    /// the community owner. Non-owners receive a `forbidden` stanza
    /// error.
    pub async fn admin_users_list(
        &self,
        prefix: Option<String>,
        page_size: Option<u32>,
        after_cursor: Option<String>,
    ) -> Result<WaddleAdminUsersPage, WaddleError> {
        let args = core::AdminUsersListArgs {
            prefix,
            page_size,
            after_cursor,
        };
        let iq = core::build_admin_users_list_iq(&self.server_domain(), &args);
        let result = self.send_admin_command(iq).await?;
        core::parse_admin_users_list_result(&result)
            .map(users_page_to_ffi)
            .map_err(admin_parse_error)
    }

    /// `urn:waddle:admin:spaces:list:0`.
    pub async fn admin_spaces_list(
        &self,
        args: WaddleAdminSpacesListArgs,
    ) -> Result<WaddleAdminSpacesListPage, WaddleError> {
        let core_args = core::AdminSpacesListArgs {
            prefix: args.prefix,
            page_size: args.page_size,
            after_cursor: args.after_cursor,
        };
        let iq = core::build_admin_spaces_list_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let page = core::parse_admin_spaces_list_result(&result).map_err(admin_parse_error)?;
        Ok(WaddleAdminSpacesListPage {
            entries: page.entries.into_iter().map(space_entry_to_ffi).collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// `urn:waddle:admin:spaces:create:0`.
    pub async fn admin_spaces_create(
        &self,
        args: WaddleAdminSpacesCreateArgs,
    ) -> Result<WaddleAdminSpaceRef, WaddleError> {
        let core_args = core::AdminSpacesCreateArgs {
            name: args.name,
            description: args.description,
            icon_url: args.icon_url,
        };
        let iq = core::build_admin_spaces_create_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        core::parse_admin_space_ref_result(&result)
            .map(space_ref_to_ffi)
            .map_err(admin_parse_error)
    }

    /// `urn:waddle:admin:spaces:update:0`.
    pub async fn admin_spaces_update(
        &self,
        args: WaddleAdminSpacesUpdateArgs,
    ) -> Result<WaddleAdminSpaceRef, WaddleError> {
        let core_args = core::AdminSpacesUpdateArgs {
            space_jid: args.space_jid,
            space_node: args.space_node,
            name: args.name,
            description: args.description,
            icon_url: args.icon_url,
        };
        let iq = core::build_admin_spaces_update_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        core::parse_admin_space_ref_result(&result)
            .map(space_ref_to_ffi)
            .map_err(admin_parse_error)
    }

    /// `urn:waddle:admin:spaces:delete:0` — cascade-destroys the
    /// space and its channels. Calling this export is the
    /// confirmation: the server-required `confirm="yes"` literal is
    /// supplied here, so UIs MUST show their own confirm dialog
    /// first.
    pub async fn admin_spaces_delete(
        &self,
        space_jid: String,
        space_node: Option<String>,
    ) -> Result<(), WaddleError> {
        let core_args = core::AdminSpacesDeleteArgs {
            space_jid,
            space_node,
            confirm: core::CONFIRM_YES.to_string(),
        };
        let iq = core::build_admin_spaces_delete_iq(&self.server_domain(), &core_args);
        self.send_admin_command(iq).await.map(|_| ())
    }

    /// `urn:waddle:admin:spaces:members:0`.
    pub async fn admin_spaces_members(
        &self,
        args: WaddleAdminSpacesMembersArgs,
    ) -> Result<WaddleAdminSpacesMembersPage, WaddleError> {
        let core_args = core::AdminSpacesMembersArgs {
            space_jid: args.space_jid,
            space_node: args.space_node,
            page_size: args.page_size,
            after_cursor: args.after_cursor,
        };
        let iq = core::build_admin_spaces_members_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let page = core::parse_admin_spaces_members_result(&result).map_err(admin_parse_error)?;
        let entries = page
            .entries
            .into_iter()
            .map(|entry| {
                Ok(WaddleAdminSpaceMemberEntry {
                    jid: entry.jid,
                    role: space_role_from_wire(&entry.role)?,
                })
            })
            .collect::<Result<Vec<_>, WaddleError>>()?;
        Ok(WaddleAdminSpacesMembersPage {
            entries,
            next_cursor: page.next_cursor,
        })
    }

    /// `urn:waddle:admin:spaces:set-role:0`.
    pub async fn admin_spaces_set_role(
        &self,
        space_jid: String,
        space_node: Option<String>,
        member_jid: String,
        role: WaddleSpaceRole,
    ) -> Result<WaddleAdminSpacesSetRoleResult, WaddleError> {
        let core_args = core::AdminSpacesSetRoleArgs {
            space_jid,
            space_node,
            member_jid,
            role: space_role_wire(role).to_string(),
        };
        let iq = core::build_admin_spaces_set_role_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let payload =
            core::parse_admin_spaces_set_role_result(&result).map_err(admin_parse_error)?;
        Ok(WaddleAdminSpacesSetRoleResult {
            member_jid: payload.member_jid,
            role: space_role_from_wire(&payload.role)?,
        })
    }

    /// `urn:waddle:admin:channels:list:0`.
    pub async fn admin_channels_list(
        &self,
        args: WaddleAdminChannelsListArgs,
    ) -> Result<WaddleAdminChannelsListPage, WaddleError> {
        let core_args = core::AdminChannelsListArgs {
            space_jid: args.space_jid,
            space_node: args.space_node,
            prefix: args.prefix,
            page_size: args.page_size,
            after_cursor: args.after_cursor,
        };
        let iq = core::build_admin_channels_list_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let page = core::parse_admin_channels_list_result(&result).map_err(admin_parse_error)?;
        Ok(WaddleAdminChannelsListPage {
            entries: page.entries.into_iter().map(channel_entry_to_ffi).collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// `urn:waddle:admin:channels:create:0`.
    pub async fn admin_channels_create(
        &self,
        args: WaddleAdminChannelsCreateArgs,
    ) -> Result<WaddleAdminChannelRef, WaddleError> {
        let core_args = core::AdminChannelsCreateArgs {
            name: args.name,
            topic: args.topic,
            channel_type: args.channel_type,
            space_jid: args.space_jid,
            space_node: args.space_node,
            is_public: args.is_public,
            members_only: args.members_only,
        };
        let iq = core::build_admin_channels_create_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        core::parse_admin_channel_ref_result(&result)
            .map(channel_ref_to_ffi)
            .map_err(admin_parse_error)
    }

    /// `urn:waddle:admin:channels:update:0`.
    pub async fn admin_channels_update(
        &self,
        args: WaddleAdminChannelsUpdateArgs,
    ) -> Result<WaddleAdminChannelRef, WaddleError> {
        let core_args = core::AdminChannelsUpdateArgs {
            channel_jid: args.channel_jid,
            name: args.name,
            topic: args.topic,
            channel_type: args.channel_type,
            is_public: args.is_public,
            members_only: args.members_only,
        };
        let iq = core::build_admin_channels_update_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        core::parse_admin_channel_ref_result(&result)
            .map(channel_ref_to_ffi)
            .map_err(admin_parse_error)
    }

    /// `urn:waddle:admin:channels:delete:0` — destroys the MUC room.
    /// Calling this export is the confirmation (see
    /// [`Self::admin_spaces_delete`]).
    pub async fn admin_channels_delete(&self, channel_jid: String) -> Result<(), WaddleError> {
        let core_args = core::AdminChannelsDeleteArgs {
            channel_jid,
            confirm: core::CONFIRM_YES.to_string(),
        };
        let iq = core::build_admin_channels_delete_iq(&self.server_domain(), &core_args);
        self.send_admin_command(iq).await.map(|_| ())
    }

    /// `urn:waddle:admin:channels:occupants:0`.
    pub async fn admin_channels_occupants(
        &self,
        args: WaddleAdminChannelsOccupantsArgs,
    ) -> Result<WaddleAdminChannelsOccupantsPage, WaddleError> {
        let core_args = core::AdminChannelsOccupantsArgs {
            channel_jid: args.channel_jid,
            page_size: args.page_size,
            after_cursor: args.after_cursor,
        };
        let iq = core::build_admin_channels_occupants_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let page =
            core::parse_admin_channels_occupants_result(&result).map_err(admin_parse_error)?;
        let entries = page
            .entries
            .into_iter()
            .map(|entry| {
                Ok(WaddleAdminChannelOccupantEntry {
                    nick: entry.nick,
                    real_jid: entry.real_jid,
                    role: muc_role_from_wire(&entry.role)?,
                    affiliation: muc_affiliation_from_wire(&entry.affiliation)?,
                })
            })
            .collect::<Result<Vec<_>, WaddleError>>()?;
        Ok(WaddleAdminChannelsOccupantsPage {
            entries,
            next_cursor: page.next_cursor,
        })
    }

    /// `urn:waddle:admin:channels:affiliations:0`.
    pub async fn admin_channels_affiliations(
        &self,
        args: WaddleAdminChannelsAffiliationsArgs,
    ) -> Result<WaddleAdminChannelsAffiliationsPage, WaddleError> {
        let core_args = core::AdminChannelsAffiliationsArgs {
            channel_jid: args.channel_jid,
            filter: args
                .filter
                .map(|tier| muc_affiliation_wire(tier).to_string()),
            page_size: args.page_size,
            after_cursor: args.after_cursor,
        };
        let iq = core::build_admin_channels_affiliations_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let page =
            core::parse_admin_channels_affiliations_result(&result).map_err(admin_parse_error)?;
        let entries = page
            .entries
            .into_iter()
            .map(|entry| {
                Ok(WaddleAdminChannelAffiliationEntry {
                    jid: entry.jid,
                    affiliation: muc_affiliation_from_wire(&entry.affiliation)?,
                    reason: entry.reason,
                })
            })
            .collect::<Result<Vec<_>, WaddleError>>()?;
        Ok(WaddleAdminChannelsAffiliationsPage {
            entries,
            next_cursor: page.next_cursor,
        })
    }

    /// `urn:waddle:admin:channels:set-affiliation:0`.
    pub async fn admin_channels_set_affiliation(
        &self,
        channel_jid: String,
        member_jid: String,
        affiliation: WaddleMucAffiliation,
        reason: Option<String>,
    ) -> Result<WaddleAdminChannelsSetAffiliationResult, WaddleError> {
        let core_args = core::AdminChannelsSetAffiliationArgs {
            channel_jid,
            member_jid,
            affiliation: muc_affiliation_wire(affiliation).to_string(),
            reason,
        };
        let iq = core::build_admin_channels_set_affiliation_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let payload = core::parse_admin_channels_set_affiliation_result(&result)
            .map_err(admin_parse_error)?;
        Ok(WaddleAdminChannelsSetAffiliationResult {
            member_jid: payload.member_jid,
            affiliation: muc_affiliation_from_wire(&payload.affiliation)?,
        })
    }

    /// `urn:waddle:admin:channels:kick:0` — operator kick by full
    /// occupant JID (server-side XEP-0045 §8.2 role→none).
    pub async fn admin_channels_kick(
        &self,
        channel_jid: String,
        occupant_jid: String,
        reason: Option<String>,
    ) -> Result<WaddleAdminChannelsKickResult, WaddleError> {
        let core_args = core::AdminChannelsKickArgs {
            channel_jid,
            occupant_jid,
            reason,
        };
        let iq = core::build_admin_channels_kick_iq(&self.server_domain(), &core_args);
        let result = self.send_admin_command(iq).await?;
        let payload = core::parse_admin_channels_kick_result(&result).map_err(admin_parse_error)?;
        Ok(WaddleAdminChannelsKickResult {
            occupant_jid: payload.occupant_jid,
        })
    }
}
