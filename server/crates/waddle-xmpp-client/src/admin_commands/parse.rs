//! Typed parsers for `urn:waddle:admin:*` command responses.
//!
//! Responses are XEP-0050 `status='completed'` commands carrying a
//! XEP-0004 result form. List commands put rows in `<item/>` children
//! and the pagination cursor in a top-level `next_cursor` field;
//! single-entity commands carry top-level fields only. Parsing walks
//! the raw `minidom::Element` (not `xmpp_parsers::DataForm`, which
//! does not model the §3.4 multiple-items shape) exactly once into
//! the typed result structs.

use minidom::Element;

use crate::xep::xep0050::{NS_COMMANDS, NS_DATA_FORMS};

use super::types::{
    AdminChannelAffiliationEntry, AdminChannelListEntry, AdminChannelOccupantEntry,
    AdminChannelRef, AdminChannelsAffiliationsResult, AdminChannelsKickResult,
    AdminChannelsListResult, AdminChannelsOccupantsResult, AdminChannelsSetAffiliationResult,
    AdminSpaceListEntry, AdminSpaceMemberEntry, AdminSpaceRef, AdminSpacesListResult,
    AdminSpacesMembersResult, AdminSpacesSetRoleResult, AdminUserEntry, AdminUsersPage,
};

/// Typed shape errors for admin command responses. These indicate a
/// server-side bug or schema drift, never a user mistake.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminResponseError {
    #[error("admin response missing <command/>")]
    MissingCommand,
    #[error("admin response missing <x xmlns='jabber:x:data'/>")]
    MissingForm,
    #[error("admin response missing required field `{var}`")]
    MissingField { var: String },
    #[error("admin response field `{var}` is not a boolean")]
    InvalidBool { var: String },
    #[error("admin response field `{var}` is not a u32")]
    InvalidCount { var: String },
}

type ParseResult<T> = Result<T, AdminResponseError>;

fn command_form(iq: &Element) -> ParseResult<&Element> {
    let command = iq
        .get_child("command", NS_COMMANDS)
        .ok_or(AdminResponseError::MissingCommand)?;
    command
        .get_child("x", NS_DATA_FORMS)
        .ok_or(AdminResponseError::MissingForm)
}

fn maybe_command_form(iq: &Element) -> Option<&Element> {
    iq.get_child("command", NS_COMMANDS)
        .and_then(|command| command.get_child("x", NS_DATA_FORMS))
}

fn field_text(item: &Element, var: &str) -> Option<String> {
    item.children()
        .filter(|child| child.name() == "field" && child.attr("var") == Some(var))
        .find_map(|field| field.get_child("value", NS_DATA_FORMS).map(Element::text))
}

fn field_text_opt(item: &Element, var: &str) -> Option<String> {
    field_text(item, var).filter(|value| !value.is_empty())
}

fn field_text_required(item: &Element, var: &str) -> ParseResult<String> {
    field_text(item, var)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AdminResponseError::MissingField {
            var: var.to_string(),
        })
}

fn parse_bool(raw: &str, var: &str) -> ParseResult<bool> {
    match raw {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(AdminResponseError::InvalidBool {
            var: var.to_string(),
        }),
    }
}

fn field_bool(item: &Element, var: &str) -> ParseResult<bool> {
    let raw = field_text_required(item, var)?;
    parse_bool(&raw, var)
}

fn field_u32(item: &Element, var: &str) -> ParseResult<u32> {
    let raw = field_text_required(item, var)?;
    raw.parse::<u32>()
        .map_err(|_| AdminResponseError::InvalidCount {
            var: var.to_string(),
        })
}

fn items(form: &Element) -> impl Iterator<Item = &Element> {
    form.children().filter(|child| child.name() == "item")
}

fn next_cursor(form: &Element) -> Option<String> {
    field_text_opt(form, "next_cursor")
}

// ── V1 users panel ───────────────────────────────────────────────────

pub fn parse_admin_users_list_result(iq: &Element) -> ParseResult<AdminUsersPage> {
    let command = iq
        .get_child("command", NS_COMMANDS)
        .ok_or(AdminResponseError::MissingCommand)?;
    let Some(form) = command.get_child("x", NS_DATA_FORMS) else {
        return Ok(AdminUsersPage::default());
    };
    let entries = items(form)
        .map(|item| AdminUserEntry {
            jid: field_text(item, "jid").unwrap_or_default(),
            display_name: field_text_opt(item, "display_name"),
            has_owner_hat: matches!(
                field_text(item, "has_owner_hat").as_deref(),
                Some("1") | Some("true")
            ),
        })
        .collect();
    Ok(AdminUsersPage {
        entries,
        next_cursor: next_cursor(form),
    })
}

// ── V2 spaces panel ──────────────────────────────────────────────────

pub fn parse_admin_spaces_list_result(iq: &Element) -> ParseResult<AdminSpacesListResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(AdminSpacesListResult::default());
    };
    let mut entries = Vec::new();
    for item in items(form) {
        entries.push(AdminSpaceListEntry {
            space_jid: field_text_required(item, "space_jid")?,
            space_node: field_text_required(item, "space_node")?,
            name: field_text_required(item, "name")?,
            description: field_text_opt(item, "description"),
            icon_url: field_text_opt(item, "icon_url"),
            channel_count: field_u32(item, "channel_count")?,
            member_count: field_u32(item, "member_count")?,
        });
    }
    Ok(AdminSpacesListResult {
        entries,
        next_cursor: next_cursor(form),
    })
}

pub fn parse_admin_space_ref_result(iq: &Element) -> ParseResult<AdminSpaceRef> {
    let form = command_form(iq)?;
    Ok(AdminSpaceRef {
        space_jid: field_text_required(form, "space_jid")?,
        space_node: field_text_required(form, "space_node")?,
        name: field_text_required(form, "name")?,
        description: field_text_opt(form, "description"),
        icon_url: field_text_opt(form, "icon_url"),
    })
}

pub fn parse_admin_spaces_members_result(iq: &Element) -> ParseResult<AdminSpacesMembersResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(AdminSpacesMembersResult::default());
    };
    let mut entries = Vec::new();
    for item in items(form) {
        entries.push(AdminSpaceMemberEntry {
            jid: field_text_required(item, "jid")?,
            role: field_text_required(item, "role")?,
        });
    }
    Ok(AdminSpacesMembersResult {
        entries,
        next_cursor: next_cursor(form),
    })
}

pub fn parse_admin_spaces_set_role_result(iq: &Element) -> ParseResult<AdminSpacesSetRoleResult> {
    let form = command_form(iq)?;
    Ok(AdminSpacesSetRoleResult {
        member_jid: field_text_required(form, "member_jid")?,
        role: field_text_required(form, "role")?,
    })
}

// ── V2 channels panel ────────────────────────────────────────────────

pub fn parse_admin_channels_list_result(iq: &Element) -> ParseResult<AdminChannelsListResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(AdminChannelsListResult::default());
    };
    let mut entries = Vec::new();
    for item in items(form) {
        entries.push(AdminChannelListEntry {
            channel_jid: field_text_required(item, "channel_jid")?,
            name: field_text_required(item, "name")?,
            topic: field_text_opt(item, "topic"),
            channel_type: field_text_required(item, "channel_type")?,
            is_public: field_bool(item, "is_public")?,
            members_only: field_bool(item, "members_only")?,
            occupant_count: field_u32(item, "occupant_count")?,
            owner_count: field_u32(item, "owner_count")?,
            admin_count: field_u32(item, "admin_count")?,
            member_count: field_u32(item, "member_count")?,
            outcast_count: field_u32(item, "outcast_count")?,
        });
    }
    Ok(AdminChannelsListResult {
        entries,
        next_cursor: next_cursor(form),
    })
}

pub fn parse_admin_channel_ref_result(iq: &Element) -> ParseResult<AdminChannelRef> {
    let form = command_form(iq)?;
    Ok(AdminChannelRef {
        channel_jid: field_text_required(form, "channel_jid")?,
        name: field_text_required(form, "name")?,
        topic: field_text_opt(form, "topic"),
        channel_type: field_text_required(form, "channel_type")?,
        is_public: field_bool(form, "is_public")?,
        members_only: field_bool(form, "members_only")?,
    })
}

pub fn parse_admin_channels_occupants_result(
    iq: &Element,
) -> ParseResult<AdminChannelsOccupantsResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(AdminChannelsOccupantsResult::default());
    };
    let mut entries = Vec::new();
    for item in items(form) {
        entries.push(AdminChannelOccupantEntry {
            nick: field_text_required(item, "nick")?,
            real_jid: field_text_required(item, "real_jid")?,
            role: field_text_required(item, "role")?,
            affiliation: field_text_required(item, "affiliation")?,
        });
    }
    Ok(AdminChannelsOccupantsResult {
        entries,
        next_cursor: next_cursor(form),
    })
}

pub fn parse_admin_channels_affiliations_result(
    iq: &Element,
) -> ParseResult<AdminChannelsAffiliationsResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(AdminChannelsAffiliationsResult::default());
    };
    let mut entries = Vec::new();
    for item in items(form) {
        entries.push(AdminChannelAffiliationEntry {
            jid: field_text_required(item, "jid")?,
            affiliation: field_text_required(item, "affiliation")?,
            reason: field_text_opt(item, "reason"),
        });
    }
    Ok(AdminChannelsAffiliationsResult {
        entries,
        next_cursor: next_cursor(form),
    })
}

pub fn parse_admin_channels_set_affiliation_result(
    iq: &Element,
) -> ParseResult<AdminChannelsSetAffiliationResult> {
    let form = command_form(iq)?;
    Ok(AdminChannelsSetAffiliationResult {
        member_jid: field_text_required(form, "member_jid")?,
        affiliation: field_text_required(form, "affiliation")?,
    })
}

pub fn parse_admin_channels_kick_result(iq: &Element) -> ParseResult<AdminChannelsKickResult> {
    let form = command_form(iq)?;
    Ok(AdminChannelsKickResult {
        occupant_jid: field_text_required(form, "occupant_jid")?,
    })
}
