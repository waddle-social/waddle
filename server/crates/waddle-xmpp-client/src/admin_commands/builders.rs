//! IQ builders for the `urn:waddle:admin:*` commands.
//!
//! Every builder assembles a XEP-0004 submit form (hidden `FORM_TYPE`
//! mirroring the command node, per XEP-0068) and wraps it in a
//! XEP-0050 §2.4 execute request via
//! [`build_xep0050_command_request`]. Optional args are omitted from
//! the form entirely — the server applies its own defaults.

use minidom::Element;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

use crate::xep::xep0050::{build_xep0050_command_request, AdHocAction};

use super::types::{
    AdminChannelsAffiliationsArgs, AdminChannelsCreateArgs, AdminChannelsDeleteArgs,
    AdminChannelsKickArgs, AdminChannelsListArgs, AdminChannelsOccupantsArgs,
    AdminChannelsSetAffiliationArgs, AdminChannelsUpdateArgs, AdminSpacesCreateArgs,
    AdminSpacesDeleteArgs, AdminSpacesListArgs, AdminSpacesMembersArgs, AdminSpacesSetRoleArgs,
    AdminSpacesUpdateArgs, AdminUsersListArgs,
};
use super::{
    NODE_ADMIN_CHANNELS_AFFILIATIONS, NODE_ADMIN_CHANNELS_CREATE, NODE_ADMIN_CHANNELS_DELETE,
    NODE_ADMIN_CHANNELS_KICK, NODE_ADMIN_CHANNELS_LIST, NODE_ADMIN_CHANNELS_OCCUPANTS,
    NODE_ADMIN_CHANNELS_SET_AFFILIATION, NODE_ADMIN_CHANNELS_UPDATE, NODE_ADMIN_SPACES_CREATE,
    NODE_ADMIN_SPACES_DELETE, NODE_ADMIN_SPACES_LIST, NODE_ADMIN_SPACES_MEMBERS,
    NODE_ADMIN_SPACES_SET_ROLE, NODE_ADMIN_SPACES_UPDATE, NODE_ADMIN_USERS_LIST,
};

fn field(var: &str, field_type: FieldType, value: String) -> Field {
    Field {
        var: Some(var.to_string()),
        type_: field_type,
        label: None,
        required: false,
        desc: None,
        options: vec![],
        values: vec![value],
        media: vec![],
        validate: None,
    }
}

fn text_single(var: &str, value: &str) -> Field {
    field(var, FieldType::TextSingle, value.to_string())
}

fn boolean(var: &str, value: bool) -> Field {
    field(
        var,
        FieldType::Boolean,
        if value { "1" } else { "0" }.to_string(),
    )
}

fn jid_single(var: &str, value: &str) -> Field {
    field(var, FieldType::JidSingle, value.to_string())
}

fn push_opt_text(fields: &mut Vec<Field>, var: &str, value: Option<&str>) {
    if let Some(value) = value {
        fields.push(text_single(var, value));
    }
}

fn push_opt_page(fields: &mut Vec<Field>, page_size: Option<u32>, after_cursor: Option<&str>) {
    if let Some(page_size) = page_size {
        fields.push(text_single("page_size", &page_size.to_string()));
    }
    push_opt_text(fields, "after_cursor", after_cursor);
}

fn command_iq(server_domain: &str, node: &str, fields: Vec<Field>) -> Element {
    let form = DataForm::new(DataFormType::Submit, node, fields);
    build_xep0050_command_request(server_domain, node, AdHocAction::Execute, Some(form))
}

// ── V1 users panel ───────────────────────────────────────────────────

pub fn build_admin_users_list_iq(server_domain: &str, args: &AdminUsersListArgs) -> Element {
    let mut fields = Vec::new();
    push_opt_text(&mut fields, "prefix", args.prefix.as_deref());
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_USERS_LIST, fields)
}

// ── V2 spaces panel ──────────────────────────────────────────────────

pub fn build_admin_spaces_list_iq(server_domain: &str, args: &AdminSpacesListArgs) -> Element {
    let mut fields = Vec::new();
    push_opt_text(&mut fields, "prefix", args.prefix.as_deref());
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_SPACES_LIST, fields)
}

pub fn build_admin_spaces_create_iq(server_domain: &str, args: &AdminSpacesCreateArgs) -> Element {
    let mut fields = vec![text_single("name", &args.name)];
    push_opt_text(&mut fields, "description", args.description.as_deref());
    push_opt_text(&mut fields, "icon_url", args.icon_url.as_deref());
    command_iq(server_domain, NODE_ADMIN_SPACES_CREATE, fields)
}

pub fn build_admin_spaces_update_iq(server_domain: &str, args: &AdminSpacesUpdateArgs) -> Element {
    let mut fields = vec![jid_single("space_jid", &args.space_jid)];
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    push_opt_text(&mut fields, "name", args.name.as_deref());
    push_opt_text(&mut fields, "description", args.description.as_deref());
    push_opt_text(&mut fields, "icon_url", args.icon_url.as_deref());
    command_iq(server_domain, NODE_ADMIN_SPACES_UPDATE, fields)
}

pub fn build_admin_spaces_delete_iq(server_domain: &str, args: &AdminSpacesDeleteArgs) -> Element {
    let mut fields = vec![jid_single("space_jid", &args.space_jid)];
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    fields.push(text_single("confirm", &args.confirm));
    command_iq(server_domain, NODE_ADMIN_SPACES_DELETE, fields)
}

pub fn build_admin_spaces_members_iq(
    server_domain: &str,
    args: &AdminSpacesMembersArgs,
) -> Element {
    let mut fields = vec![jid_single("space_jid", &args.space_jid)];
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_SPACES_MEMBERS, fields)
}

pub fn build_admin_spaces_set_role_iq(
    server_domain: &str,
    args: &AdminSpacesSetRoleArgs,
) -> Element {
    let mut fields = vec![jid_single("space_jid", &args.space_jid)];
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    fields.push(jid_single("member_jid", &args.member_jid));
    fields.push(text_single("role", &args.role));
    command_iq(server_domain, NODE_ADMIN_SPACES_SET_ROLE, fields)
}

// ── V2 channels panel ────────────────────────────────────────────────

pub fn build_admin_channels_list_iq(server_domain: &str, args: &AdminChannelsListArgs) -> Element {
    let mut fields = Vec::new();
    if let Some(space_jid) = args.space_jid.as_deref() {
        fields.push(jid_single("space_jid", space_jid));
    }
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    push_opt_text(&mut fields, "prefix", args.prefix.as_deref());
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_CHANNELS_LIST, fields)
}

pub fn build_admin_channels_create_iq(
    server_domain: &str,
    args: &AdminChannelsCreateArgs,
) -> Element {
    let mut fields = vec![text_single("name", &args.name)];
    push_opt_text(&mut fields, "topic", args.topic.as_deref());
    push_opt_text(&mut fields, "channel_type", args.channel_type.as_deref());
    if let Some(space_jid) = args.space_jid.as_deref() {
        fields.push(jid_single("space_jid", space_jid));
    }
    push_opt_text(&mut fields, "space_node", args.space_node.as_deref());
    if let Some(is_public) = args.is_public {
        fields.push(boolean("is_public", is_public));
    }
    if let Some(members_only) = args.members_only {
        fields.push(boolean("members_only", members_only));
    }
    command_iq(server_domain, NODE_ADMIN_CHANNELS_CREATE, fields)
}

pub fn build_admin_channels_update_iq(
    server_domain: &str,
    args: &AdminChannelsUpdateArgs,
) -> Element {
    let mut fields = vec![jid_single("channel_jid", &args.channel_jid)];
    push_opt_text(&mut fields, "name", args.name.as_deref());
    push_opt_text(&mut fields, "topic", args.topic.as_deref());
    push_opt_text(&mut fields, "channel_type", args.channel_type.as_deref());
    if let Some(is_public) = args.is_public {
        fields.push(boolean("is_public", is_public));
    }
    if let Some(members_only) = args.members_only {
        fields.push(boolean("members_only", members_only));
    }
    command_iq(server_domain, NODE_ADMIN_CHANNELS_UPDATE, fields)
}

pub fn build_admin_channels_delete_iq(
    server_domain: &str,
    args: &AdminChannelsDeleteArgs,
) -> Element {
    let fields = vec![
        jid_single("channel_jid", &args.channel_jid),
        text_single("confirm", &args.confirm),
    ];
    command_iq(server_domain, NODE_ADMIN_CHANNELS_DELETE, fields)
}

pub fn build_admin_channels_occupants_iq(
    server_domain: &str,
    args: &AdminChannelsOccupantsArgs,
) -> Element {
    let mut fields = vec![jid_single("channel_jid", &args.channel_jid)];
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_CHANNELS_OCCUPANTS, fields)
}

pub fn build_admin_channels_affiliations_iq(
    server_domain: &str,
    args: &AdminChannelsAffiliationsArgs,
) -> Element {
    let mut fields = vec![jid_single("channel_jid", &args.channel_jid)];
    push_opt_text(&mut fields, "filter", args.filter.as_deref());
    push_opt_page(&mut fields, args.page_size, args.after_cursor.as_deref());
    command_iq(server_domain, NODE_ADMIN_CHANNELS_AFFILIATIONS, fields)
}

pub fn build_admin_channels_set_affiliation_iq(
    server_domain: &str,
    args: &AdminChannelsSetAffiliationArgs,
) -> Element {
    let mut fields = vec![
        jid_single("channel_jid", &args.channel_jid),
        jid_single("member_jid", &args.member_jid),
        text_single("affiliation", &args.affiliation),
    ];
    push_opt_text(&mut fields, "reason", args.reason.as_deref());
    command_iq(server_domain, NODE_ADMIN_CHANNELS_SET_AFFILIATION, fields)
}

pub fn build_admin_channels_kick_iq(server_domain: &str, args: &AdminChannelsKickArgs) -> Element {
    let mut fields = vec![
        jid_single("channel_jid", &args.channel_jid),
        jid_single("occupant_jid", &args.occupant_jid),
    ];
    push_opt_text(&mut fields, "reason", args.reason.as_deref());
    command_iq(server_domain, NODE_ADMIN_CHANNELS_KICK, fields)
}
