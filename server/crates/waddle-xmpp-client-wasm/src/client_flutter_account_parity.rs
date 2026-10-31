//! Native-compatible account/discovery verbs for the independent Flutter client.
//! The boundary uses typed DTOs; XMPP payloads are existing shared builders.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use jid::BareJid;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::discovery::{
    build_disco_info_iq, build_disco_items_iq, build_pubsub_items_iq, parse_disco_info_result,
    parse_disco_items_result, parse_space_channels_result, space_from_disco_item, DiscoInfoResult,
    DiscoveredChannel, DiscoveredChannelType, DiscoveredComponentServices, DiscoveredSpace,
    SpaceNode, MUC_NS, SPACES_NS, STANDALONE_SPACE_ID, WADDLE_GROUP_DM_FEATURE_NS,
    WADDLE_ROOM_METADATA_FORM_TYPE,
};
use waddle_xmpp_client::extension_commands::{
    build_extension_invoke_iq, build_extension_submit_iq, extension_command_descriptor,
    extension_command_refs, parse_extension_command_metadata, parse_extension_command_result,
    ExtensionCommandDescriptor, ExtensionCommandMetadata, ExtensionCommandResult,
    ExtensionNoteType, ExtensionSubmitField,
};
use waddle_xmpp_client::group_dm::{
    build_group_dm_create_iq, build_group_dm_invite_message, build_group_dm_leave_iq,
    build_group_dm_rename_iq, parse_group_dm_create_result, parse_group_dm_leave_result,
    parse_group_dm_rename_result, GroupDmCreateArgs, GroupDmHistoryAccess,
};
use waddle_xmpp_client::xep::xep0050::{AdHocAction, AdHocStatus, NS_COMMANDS};
use waddle_xmpp_client::xep::xep0402::{build_fetch_bookmarks_iq, parse_bookmarks_response};
use wasm_bindgen::JsValue;

use crate::commands::{send_iq_command, send_stanza_command};
use crate::extension_routes::discover_extension_command_service;
use crate::helpers::{js_error, to_js_value};
use crate::WaddleClientInner;

const MUC_ROOMINFO_FORM_TYPE: &str = "http://jabber.org/protocol/muc#roominfo";

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "args", rename_all = "snake_case")]
pub(crate) enum AccountCommand {
    DiscoverTopology {},
    CreateGroupDm {
        name: String,
        member_jids: Vec<BareJid>,
    },
    RenameGroupDm {
        room_jid: BareJid,
        name: Option<String>,
    },
    LeaveGroupDm {
        room_jid: BareJid,
    },
    InviteToGroupDm {
        room_jid: BareJid,
        invitee_jid: BareJid,
        history: HistoryAccess,
    },
    DiscoverExtensionCommands {},
    InvokeExtensionCommand {
        service_jid: BareJid,
        node: String,
        room_jid: Option<BareJid>,
    },
    SubmitExtensionCommandForm {
        service_jid: BareJid,
        node: String,
        session_id: Option<String>,
        fields: Vec<SubmitField>,
        action: Action,
        room_jid: Option<BareJid>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HistoryAccess {
    FromJoin,
    Full,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Execute,
    Cancel,
    Next,
    Prev,
    Complete,
}

impl From<Action> for AdHocAction {
    fn from(action: Action) -> Self {
        match action {
            Action::Execute => Self::Execute,
            Action::Cancel => Self::Cancel,
            Action::Next => Self::Next,
            Action::Prev => Self::Prev,
            Action::Complete => Self::Complete,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubmitField {
    name: String,
    values: Vec<String>,
}

pub(crate) async fn execute_account(
    inner: Rc<RefCell<WaddleClientInner>>,
    command: AccountCommand,
) -> Result<JsValue, JsValue> {
    match command {
        AccountCommand::DiscoverTopology {} => discover_topology(inner).await,
        AccountCommand::CreateGroupDm { name, member_jids } => {
            let domain = server_domain(&inner)?;
            let args = GroupDmCreateArgs { name, member_jids };
            let result = send_iq_command(
                inner,
                build_group_dm_create_iq(domain.as_str(), &args, &Uuid::new_v4().to_string()),
            )
            .await?;
            let created = parse_group_dm_create_result(&result)
                .map_err(|error| js_error(error.to_string()))?;
            Ok(JsValue::from_str(created.room_jid.as_str()))
        }
        AccountCommand::RenameGroupDm { room_jid, name } => {
            let result = send_iq_command(
                inner,
                build_group_dm_rename_iq(&room_jid, name.as_deref(), &Uuid::new_v4().to_string()),
            )
            .await?;
            parse_group_dm_rename_result(&result).map_err(|error| js_error(error.to_string()))?;
            Ok(JsValue::UNDEFINED)
        }
        AccountCommand::LeaveGroupDm { room_jid } => {
            let domain = server_domain(&inner)?;
            let result = send_iq_command(
                inner,
                build_group_dm_leave_iq(domain.as_str(), &room_jid, &Uuid::new_v4().to_string()),
            )
            .await?;
            parse_group_dm_leave_result(&result).map_err(|error| js_error(error.to_string()))?;
            Ok(JsValue::UNDEFINED)
        }
        AccountCommand::InviteToGroupDm {
            room_jid,
            invitee_jid,
            history,
        } => {
            let history = match history {
                HistoryAccess::FromJoin => GroupDmHistoryAccess::FromJoin,
                HistoryAccess::Full => GroupDmHistoryAccess::Full,
            };
            send_stanza_command(
                inner,
                build_group_dm_invite_message(&room_jid, &invitee_jid, history),
            )
            .await?;
            Ok(JsValue::UNDEFINED)
        }
        AccountCommand::DiscoverExtensionCommands {} => discover_commands(inner).await,
        AccountCommand::InvokeExtensionCommand {
            service_jid,
            node,
            room_jid,
        } => {
            if node.trim().is_empty() {
                return Err(js_error("command node must not be empty"));
            }
            let result = send_iq_command(
                inner,
                build_extension_invoke_iq(
                    service_jid.as_str(),
                    &node,
                    room_jid.as_ref().map(|jid| jid.as_str()),
                ),
            )
            .await?;
            command_response(&result)
        }
        AccountCommand::SubmitExtensionCommandForm {
            service_jid,
            node,
            session_id,
            fields,
            action,
            room_jid,
        } => {
            if node.trim().is_empty() {
                return Err(js_error("command node must not be empty"));
            }
            let fields: Vec<_> = fields
                .into_iter()
                .map(|field| ExtensionSubmitField {
                    var: field.name,
                    values: field.values,
                })
                .collect();
            let result = send_iq_command(
                inner,
                build_extension_submit_iq(
                    service_jid.as_str(),
                    &node,
                    session_id.as_deref(),
                    action.into(),
                    &fields,
                    room_jid.as_ref().map(|jid| jid.as_str()),
                ),
            )
            .await?;
            command_response(&result)
        }
    }
}

fn server_domain(inner: &Rc<RefCell<WaddleClientInner>>) -> Result<BareJid, JsValue> {
    let account: BareJid = inner
        .borrow()
        .config
        .jid
        .parse()
        .map_err(|error: jid::Error| js_error(error.to_string()))?;
    Ok(BareJid::from_parts(None, account.domain()))
}

async fn disco_info(
    inner: Rc<RefCell<WaddleClientInner>>,
    target: &BareJid,
    node: Option<&str>,
) -> Result<DiscoInfoResult, JsValue> {
    let result = send_iq_command(inner, build_disco_info_iq(target.as_str(), node)).await?;
    parse_disco_info_result(&result, target.as_str())
        .ok_or_else(|| js_error("invalid disco info response"))
}

/// Native service-selection semantics, reused by Flutter room creation.
pub(crate) async fn discover_services(
    inner: Rc<RefCell<WaddleClientInner>>,
) -> Result<DiscoveredComponentServices, JsValue> {
    let domain = server_domain(&inner)?;
    let mut services = DiscoveredComponentServices {
        muc: format!("muc.{domain}")
            .parse()
            .map_err(|error: jid::Error| js_error(error.to_string()))?,
        spaces: format!("spaces.{domain}")
            .parse()
            .map_err(|error: jid::Error| js_error(error.to_string()))?,
    };
    let items =
        match send_iq_command(inner.clone(), build_disco_items_iq(domain.as_str(), None)).await {
            Ok(result) => parse_disco_items_result(&result).unwrap_or_default(),
            Err(_) => return Ok(services),
        };
    let (mut found_muc, mut found_spaces) = (false, false);
    for item in items {
        let Ok(target) = item.jid.parse::<BareJid>() else {
            continue;
        };
        let Ok(info) = disco_info(inner.clone(), &target, None).await else {
            continue;
        };
        if !found_muc
            && (info.has_feature(MUC_NS)
                || info
                    .identities
                    .iter()
                    .any(|identity| identity.category == "conference"))
        {
            services.muc = target.clone();
            found_muc = true;
        }
        if !found_spaces && info.has_feature(SPACES_NS) {
            services.spaces = target;
            found_spaces = true;
        }
    }
    Ok(services)
}

fn hydrate_channel(channel: &mut DiscoveredChannel, info: &DiscoInfoResult) {
    if let Some(kind) = info
        .form_value(WADDLE_ROOM_METADATA_FORM_TYPE, "waddle#channel_type")
        .and_then(DiscoveredChannelType::from_metadata)
    {
        channel.channel_type = kind;
    }
    channel.description = info
        .form_value(MUC_ROOMINFO_FORM_TYPE, "muc#roominfo_description")
        .filter(|description| !description.trim().is_empty())
        .map(str::to_owned);
    channel.is_group_dm = info.has_feature(WADDLE_GROUP_DM_FEATURE_NS);
}

async fn discover_topology(inner: Rc<RefCell<WaddleClientInner>>) -> Result<JsValue, JsValue> {
    let services = discover_services(inner.clone()).await?;
    let mut spaces = Vec::<DiscoveredSpace>::new();
    let mut channels = Vec::<DiscoveredChannel>::new();
    if let Ok(result) = send_iq_command(
        inner.clone(),
        build_disco_items_iq(services.spaces.as_str(), None),
    )
    .await
    {
        for item in parse_disco_items_result(&result).unwrap_or_default() {
            let Some(node) = item.node.as_deref() else {
                continue;
            };
            let Ok(info) = disco_info(inner.clone(), &services.spaces, Some(node)).await else {
                continue;
            };
            let Some(space) = space_from_disco_item(&services.spaces, item, &info) else {
                continue;
            };
            if let Ok(result) = send_iq_command(
                inner.clone(),
                build_pubsub_items_iq(&services.spaces, &space.id),
            )
            .await
            {
                for mut channel in
                    parse_space_channels_result(&result, &space.id).unwrap_or_default()
                {
                    if let Ok(info) = disco_info(inner.clone(), &channel.room_jid, None).await {
                        hydrate_channel(&mut channel, &info);
                    }
                    channels.push(channel);
                }
            }
            spaces.push(space);
        }
    }
    let standalone = SpaceNode::try_new(STANDALONE_SPACE_ID)
        .ok_or_else(|| js_error("invalid standalone node"))?;
    let mut seen: HashSet<_> = channels
        .iter()
        .map(|channel| channel.room_jid.clone())
        .collect();
    if let Ok(result) = send_iq_command(
        inner.clone(),
        build_disco_items_iq(services.muc.as_str(), None),
    )
    .await
    {
        for item in parse_disco_items_result(&result).unwrap_or_default() {
            let Ok(room) = item.jid.parse::<BareJid>() else {
                continue;
            };
            if !seen.insert(room.clone()) {
                continue;
            }
            let mut channel = standalone_channel(room, item.name, &standalone, channels.len());
            if let Ok(info) = disco_info(inner.clone(), &channel.room_jid, None).await {
                hydrate_channel(&mut channel, &info);
            }
            channels.push(channel);
        }
    }
    if let Ok(result) = send_iq_command(
        inner.clone(),
        build_fetch_bookmarks_iq(&Uuid::new_v4().to_string()),
    )
    .await
    {
        for bookmark in parse_bookmarks_response(&result) {
            if let Some(channel) = channels
                .iter_mut()
                .find(|channel| channel.room_jid == bookmark.jid)
            {
                channel.autojoin = bookmark.autojoin;
                channel.bookmark_name = bookmark.name;
            } else if let Ok(info) = disco_info(inner.clone(), &bookmark.jid, None).await {
                if !info.has_feature(MUC_NS) {
                    continue;
                }
                let mut channel = standalone_channel(
                    bookmark.jid,
                    bookmark.name.clone(),
                    &standalone,
                    channels.len(),
                );
                channel.autojoin = bookmark.autojoin;
                channel.bookmark_name = bookmark.name;
                hydrate_channel(&mut channel, &info);
                channels.push(channel);
            }
        }
    }
    if channels
        .iter()
        .any(|channel| channel.space_id == standalone)
    {
        spaces.push(DiscoveredSpace {
            id: standalone,
            service_jid: services.muc,
            name: "Rooms".into(),
            description: None,
        });
    }
    to_js_value(&TopologyBoundary {
        spaces: spaces.iter().map(SpaceBoundary::from).collect(),
        channels: channels.iter().map(ChannelBoundary::from).collect(),
    })
}

fn standalone_channel(
    room: BareJid,
    name: Option<String>,
    space: &SpaceNode,
    position: usize,
) -> DiscoveredChannel {
    DiscoveredChannel {
        id: format!("{}::{room}", space.as_str()),
        name: name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                room.node()
                    .map(|node| node.as_str())
                    .unwrap_or(room.as_str())
                    .to_owned()
            }),
        room_jid: room,
        description: None,
        channel_type: DiscoveredChannelType::Text,
        position: i32::try_from(position).unwrap_or(i32::MAX),
        space_id: space.clone(),
        autojoin: true,
        bookmark_name: None,
        is_group_dm: false,
    }
}

#[derive(Serialize)]
struct TopologyBoundary<'a> {
    spaces: Vec<SpaceBoundary<'a>>,
    channels: Vec<ChannelBoundary<'a>>,
}
#[derive(Serialize)]
struct SpaceBoundary<'a> {
    id: &'a str,
    service_jid: &'a BareJid,
    name: &'a str,
    description: &'a Option<String>,
}
impl<'a> From<&'a DiscoveredSpace> for SpaceBoundary<'a> {
    fn from(value: &'a DiscoveredSpace) -> Self {
        Self {
            id: value.id.as_str(),
            service_jid: &value.service_jid,
            name: &value.name,
            description: &value.description,
        }
    }
}
#[derive(Serialize)]
struct ChannelBoundary<'a> {
    id: &'a str,
    room_jid: &'a BareJid,
    name: &'a str,
    description: &'a Option<String>,
    channel_type: &'a str,
    position: i32,
    space_id: &'a str,
    autojoin: bool,
    bookmark_name: &'a Option<String>,
    is_group_dm: bool,
}
impl<'a> From<&'a DiscoveredChannel> for ChannelBoundary<'a> {
    fn from(value: &'a DiscoveredChannel) -> Self {
        Self {
            id: &value.id,
            room_jid: &value.room_jid,
            name: &value.name,
            description: &value.description,
            channel_type: value.channel_type.as_str(),
            position: value.position,
            space_id: value.space_id.as_str(),
            autojoin: value.autojoin,
            bookmark_name: &value.bookmark_name,
            is_group_dm: value.is_group_dm,
        }
    }
}

async fn discover_commands(inner: Rc<RefCell<WaddleClientInner>>) -> Result<JsValue, JsValue> {
    let domain = server_domain(&inner)?;
    let service = discover_extension_command_service(inner.clone(), domain.as_str()).await;
    let result = send_iq_command(
        inner.clone(),
        build_disco_items_iq(&service, Some(NS_COMMANDS)),
    )
    .await?;
    let items =
        parse_disco_items_result(&result).ok_or_else(|| js_error("invalid command list"))?;
    let mut commands = Vec::new();
    for item in extension_command_refs(items, &service) {
        let target: BareJid = item
            .service_jid
            .parse()
            .map_err(|error: jid::Error| js_error(error.to_string()))?;
        let metadata = disco_info(inner.clone(), &target, Some(&item.node))
            .await
            .map(|info| parse_extension_command_metadata(&info))
            .unwrap_or_else(|_| ExtensionCommandMetadata::default());
        commands.push(CommandBoundary::from(extension_command_descriptor(
            item, metadata,
        )));
    }
    to_js_value(&commands)
}

#[derive(Serialize)]
struct CommandBoundary {
    service_jid: String,
    node: String,
    name: String,
    scope: &'static str,
    composer_prefix: Option<String>,
    inline_field: Option<String>,
    composer_execute: bool,
}
impl From<ExtensionCommandDescriptor> for CommandBoundary {
    fn from(value: ExtensionCommandDescriptor) -> Self {
        Self {
            service_jid: value.service_jid,
            node: value.node,
            name: value.name,
            scope: value.scope.as_wire_str(),
            composer_prefix: value.composer_prefix,
            inline_field: value.inline_field,
            composer_execute: value.composer_execute,
        }
    }
}

fn command_response(result: &minidom::Element) -> Result<JsValue, JsValue> {
    let response =
        parse_extension_command_result(result).map_err(|error| js_error(error.to_string()))?;
    to_js_value(&ResultBoundary::from(response))
}

#[derive(Serialize)]
struct ResultBoundary {
    status: &'static str,
    session_id: Option<String>,
    actions: Vec<&'static str>,
    form: Option<FormBoundary>,
    notes: Vec<NoteBoundary>,
}
#[derive(Serialize)]
struct FormBoundary {
    title: Option<String>,
    instructions: Option<String>,
    fields: Vec<FieldBoundary>,
}
#[derive(Serialize)]
struct FieldBoundary {
    name: String,
    label: Option<String>,
    field_type: String,
    required: bool,
    blocked: bool,
    options: Vec<OptionBoundary>,
    values: Vec<String>,
}
#[derive(Serialize)]
struct OptionBoundary {
    label: Option<String>,
    value: String,
}
#[derive(Serialize)]
struct NoteBoundary {
    note_type: &'static str,
    value: String,
}
impl From<ExtensionCommandResult> for ResultBoundary {
    fn from(value: ExtensionCommandResult) -> Self {
        Self {
            status: match value.status {
                AdHocStatus::Executing => "executing",
                AdHocStatus::Completed => "completed",
                AdHocStatus::Canceled => "canceled",
            },
            session_id: value.session_id,
            actions: value
                .actions
                .into_iter()
                .map(AdHocAction::as_wire_str)
                .collect(),
            form: value.form.map(|form| FormBoundary {
                title: form.title,
                instructions: form.instructions,
                fields: form
                    .fields
                    .into_iter()
                    .filter(|field| !field.blocked)
                    .map(|field| FieldBoundary {
                        name: field.var,
                        label: field.label,
                        field_type: field.field_type.as_wire_str().replace('-', "_"),
                        required: field.required,
                        blocked: false,
                        options: field
                            .options
                            .into_iter()
                            .map(|option| OptionBoundary {
                                label: option.label,
                                value: option.value,
                            })
                            .collect(),
                        values: field.values,
                    })
                    .collect(),
            }),
            notes: value
                .notes
                .into_iter()
                .map(|note| NoteBoundary {
                    note_type: match note.note_type {
                        ExtensionNoteType::Info => "info",
                        ExtensionNoteType::Warn => "warn",
                        ExtensionNoteType::Error => "error",
                    },
                    value: note.value,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_boundary_rejects_full_member_jids_and_unknown_actions() {
        assert!(serde_json::from_value::<AccountCommand>(serde_json::json!({
            "method":"create_group_dm", "args":{"name":"Group", "member_jids":["a@example.org/resource"]}
        })).is_err());
        assert!(serde_json::from_value::<AccountCommand>(serde_json::json!({
            "method":"submit_extension_command_form", "args":{"service_jid":"extensions.example.org",
            "node":"test", "session_id":null,"fields":[],"action":"invented","room_jid":null}
        })).is_err());
    }

    #[test]
    fn standalone_channel_keeps_bookmark_classification_at_boundary() {
        let room: BareJid = "group@muc.example.org".parse().expect("valid room");
        let space = SpaceNode::try_new(STANDALONE_SPACE_ID).expect("valid node");
        let mut channel = standalone_channel(room, Some("Friends".into()), &space, 2);
        channel.autojoin = false;
        channel.is_group_dm = true;
        let value = serde_json::to_value(ChannelBoundary::from(&channel)).expect("serializable");
        assert_eq!(value["room_jid"], "group@muc.example.org");
        assert_eq!(value["name"], "Friends");
        assert_eq!(value["autojoin"], false);
        assert_eq!(value["is_group_dm"], true);
    }
}
