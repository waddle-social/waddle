#[cfg(not(test))]
use crate::bindings::waddle::extension::host_tools;
use crate::bindings::waddle::extension::types;
use crate::constants::{
    MAX_CONTEXT_BYTES, MAX_CONTEXT_ITEMS_PER_SOURCE, MAX_CONTEXT_LIMIT, MAX_CONTEXT_LINE_BYTES,
};
use crate::model::{HostTool, HostToolRequest, NonEmptyString, ProviderRequest, ResponseTarget};
use crate::text::truncate_context_line;

pub(crate) fn select_host_tools(
    context: &crate::model::ExecutionContext,
    context_limit: u32,
) -> Vec<HostToolRequest> {
    let mut tools = Vec::new();
    if context_limit > 0 {
        tools.push(host_tool(HostTool::QueryMam));
    }
    if context.response_target.is_some() {
        tools.push(host_tool(HostTool::Members));
    }
    if context.response_target.is_none() {
        tools.push(host_tool(HostTool::Channels));
        tools.push(host_tool(HostTool::Spaces));
        tools.push(host_tool(HostTool::Presence));
        tools.push(host_tool(HostTool::Roster));
    }
    tools
}

fn host_tool(tool: HostTool) -> HostToolRequest {
    HostToolRequest { tool }
}

#[cfg(not(test))]
pub(crate) fn execute_provider_tool_call_content(
    tool_call: &serde_json::Value,
    request: &ProviderRequest,
) -> Result<String, String> {
    let args = tool_call
        .pointer("/function/arguments")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("{}");
    let args = serde_json::from_str::<serde_json::Value>(args)
        .map_err(|error| format!("invalid tool arguments: {error}"))?;
    let name = tool_call
        .pointer("/function/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    crate::provider::provider_tool_available(request, name)?;
    match name {
        "query_mam" => query_mam_tool(request, &args),
        "list_room_members" => list_room_members_tool(request, &args),
        "get_presence" => get_presence_tool(request, &args),
        "get_roster" => get_roster_tool(request, &args),
        "list_channels" => list_channels_tool(),
        "list_spaces" => list_spaces_tool(),
        _ => Err(format!("unsupported tool {name}")),
    }
}

#[cfg(not(test))]
fn query_mam_tool(request: &ProviderRequest, args: &serde_json::Value) -> Result<String, String> {
    let query = provider_tool_mam_query(request, args)?;
    let target = request.tool_target.as_ref();
    let response = host_tools::query_mam(&query).map_err(|error| error.message.value)?;
    Ok(format_archived_messages(response.messages, target))
}

#[cfg(not(test))]
fn list_room_members_tool(
    request: &ProviderRequest,
    args: &serde_json::Value,
) -> Result<String, String> {
    let requested_room = optional_non_empty_arg(args, "room");
    let room = if let Some(target) = request.tool_target.as_ref() {
        if let Some(requested_room) = requested_room.as_ref() {
            if requested_room.as_str() != target.room.value.as_str() {
                return Err(
                    "list_room_members room invocations cannot target another room".to_string(),
                );
            }
        }
        target.room.clone()
    } else {
        requested_room
            .map(|room| types::RoomJid {
                value: room.as_str().to_string(),
            })
            .ok_or_else(|| {
                "list_room_members requires a room outside a room invocation".to_string()
            })?
    };
    let response = host_tools::list_room_members(&types::ListRoomMembersRequest { room })
        .map_err(|error| error.message.value)?;
    let members = response
        .members
        .into_iter()
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
        .map(|member| {
            serde_json::json!({
                "room": member.room.value,
                "jid": member.jid.value,
                "nick": member.nick.map(|nick| nick.value),
                "role": format!("{:?}", member.role),
                "affiliation": format!("{:?}", member.affiliation),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "members": members }).to_string())
}

#[cfg(not(test))]
fn get_presence_tool(
    request: &ProviderRequest,
    args: &serde_json::Value,
) -> Result<String, String> {
    let subject = optional_non_empty_arg(args, "subject")
        .map(|subject| types::BareJid {
            value: subject.as_str().to_string(),
        })
        .or_else(|| request.requester.clone())
        .ok_or_else(|| "get_presence requires a requester or subject".to_string())?;
    let response = host_tools::get_presence(&types::GetPresenceRequest { subject })
        .map_err(|error| error.message.value)?;
    let resources = response
        .resources
        .into_iter()
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
        .map(|presence| {
            serde_json::json!({
                "jid": presence.jid.value,
                "availability": format!("{:?}", presence.availability),
                "show": presence.show.map(|show| format!("{show:?}")),
                "status": presence.status.map(|status| status.value),
                "priority": presence.priority,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "resources": resources }).to_string())
}

#[cfg(not(test))]
fn get_roster_tool(request: &ProviderRequest, args: &serde_json::Value) -> Result<String, String> {
    let owner = optional_non_empty_arg(args, "owner")
        .map(|owner| types::BareJid {
            value: owner.as_str().to_string(),
        })
        .or_else(|| request.requester.clone())
        .ok_or_else(|| "get_roster requires a requester or owner".to_string())?;
    let response = host_tools::get_roster(&types::GetRosterRequest { owner })
        .map_err(|error| error.message.value)?;
    let entries = response
        .entries
        .into_iter()
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
        .map(|entry| {
            serde_json::json!({
                "jid": entry.jid.value,
                "name": entry.name.map(|name| name.value),
                "subscription": format!("{:?}", entry.subscription),
                "ask": entry.ask.map(|ask| format!("{ask:?}")),
                "groups": entry.groups.into_iter().map(|group| group.value).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "entries": entries }).to_string())
}

#[cfg(not(test))]
fn list_channels_tool() -> Result<String, String> {
    let response = host_tools::list_channels(&types::ListChannelsRequest { reserved: None })
        .map_err(|error| error.message.value)?;
    let channels = response
        .channels
        .into_iter()
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
        .map(|channel| {
            serde_json::json!({
                "room": channel.room.value,
                "name": channel.name.map(|name| name.value),
                "description": channel.description.map(|description| description.value),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "channels": channels }).to_string())
}

#[cfg(not(test))]
fn list_spaces_tool() -> Result<String, String> {
    let response = host_tools::list_spaces(&types::ListSpacesRequest { reserved: None })
        .map_err(|error| error.message.value)?;
    let spaces = response
        .spaces
        .into_iter()
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
        .map(|space| {
            serde_json::json!({
                "service": space.service.value,
                "node": space.node.value,
                "name": space.name.map(|name| name.value),
                "description": space.description.map(|description| description.value),
                "channels": space.channels.into_iter().map(|room| room.value).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "spaces": spaces }).to_string())
}

pub(crate) fn provider_tool_mam_query(
    request: &ProviderRequest,
    args: &serde_json::Value,
) -> Result<types::MamQuery, String> {
    let target_arg = args.get("target").and_then(serde_json::Value::as_object);
    let target = match target_arg {
        Some(target) => {
            let kind = target
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let jid = target
                .get("jid")
                .and_then(serde_json::Value::as_str)
                .and_then(NonEmptyString::new);
            if let Some(current_target) = request.tool_target.as_ref() {
                match (kind, jid) {
                    ("room", Some(jid)) if jid.as_str() == current_target.room.value.as_str() => {
                        types::MamTarget::Room(current_target.room.clone())
                    }
                    ("room", None) => types::MamTarget::Room(current_target.room.clone()),
                    ("room", Some(_)) => {
                        return Err(
                            "query_mam room invocations cannot target another room".to_string()
                        )
                    }
                    ("conversation", _) => {
                        return Err(
                            "query_mam room invocations cannot target a direct conversation"
                                .to_string(),
                        )
                    }
                    _ => {
                        return Err("query_mam target.kind must be room or conversation".to_string())
                    }
                }
            } else {
                match (kind, jid) {
                    ("room", Some(jid)) => types::MamTarget::Room(types::RoomJid {
                        value: jid.as_str().to_string(),
                    }),
                    ("conversation", Some(jid)) => types::MamTarget::Conversation(types::BareJid {
                        value: jid.as_str().to_string(),
                    }),
                    ("room", None) => {
                        return Err("query_mam target.jid is required outside a room".to_string())
                    }
                    ("conversation", None) => {
                        return Err("query_mam conversation target requires target.jid".to_string())
                    }
                    _ => {
                        return Err("query_mam target.kind must be room or conversation".to_string())
                    }
                }
            }
        }
        None => request
            .tool_target
            .as_ref()
            .map(|target| types::MamTarget::Room(target.room.clone()))
            .ok_or_else(|| "query_mam requires a target outside a room invocation".to_string())?,
    };
    if request.context_limit == 0 {
        return Err("query_mam is disabled by context_limit".to_string());
    }
    let tool_limit = request.context_limit.min(MAX_CONTEXT_LIMIT);
    let max_results = args
        .get("max_results")
        .or_else(|| args.get("max"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(tool_limit)
        .min(tool_limit);
    let requested_thread =
        optional_non_empty_arg(args, "thread_id").map(|thread_id| types::ThreadId {
            value: thread_id.as_str().to_string(),
        });
    let default_thread = request.tool_target.as_ref().and_then(|target| {
        target
            .focus_thread
            .then(|| target.thread_id.clone())
            .flatten()
    });
    Ok(types::MamQuery {
        target,
        start: optional_non_empty_arg(args, "start").map(|value| types::Timestamp {
            value: value.as_str().to_string(),
        }),
        end: optional_non_empty_arg(args, "end").map(|value| types::Timestamp {
            value: value.as_str().to_string(),
        }),
        thread_id: requested_thread.or(default_thread),
        sender: optional_non_empty_arg(args, "sender").map(|value| types::BareJid {
            value: value.as_str().to_string(),
        }),
        text: optional_non_empty_arg(args, "text")
            .or_else(|| optional_non_empty_arg(args, "query"))
            .map(|value| types::DisplayText {
                value: value.as_str().to_string(),
            }),
        max_results,
    })
}

fn optional_non_empty_arg(args: &serde_json::Value, key: &str) -> Option<NonEmptyString> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(NonEmptyString::new)
}

pub(crate) fn format_archived_messages(
    messages: Vec<types::ArchivedMessage>,
    target: Option<&ResponseTarget>,
) -> String {
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    for message in messages
        .into_iter()
        .filter(|message| {
            target
                .map(|target| archived_message_matches_target(message, target))
                .unwrap_or(true)
        })
        .take(MAX_CONTEXT_ITEMS_PER_SOURCE)
    {
        let Some(body) = message.body else {
            continue;
        };
        let line = truncate_context_line(
            &format!(
                "{} from {}: {}",
                message.sent_at.value, message.from_jid.value, body.value
            ),
            MAX_CONTEXT_LINE_BYTES,
        );
        let additional = line.len() + usize::from(!lines.is_empty());
        if bytes.saturating_add(additional) > MAX_CONTEXT_BYTES {
            break;
        }
        bytes += additional;
        lines.push(line);
    }
    if lines.is_empty() {
        "No messages found.".to_string()
    } else {
        lines.join("\n")
    }
}

fn archived_message_matches_target(
    message: &types::ArchivedMessage,
    target: &ResponseTarget,
) -> bool {
    if !target.focus_thread {
        return true;
    }
    let Some(thread_id) = target.thread_id.as_ref() else {
        return true;
    };
    message.stanza_id.value.eq(thread_id.value.as_str())
        || message
            .thread_id
            .as_ref()
            .is_some_and(|message_thread| message_thread.value == thread_id.value)
        || message
            .reply_to
            .as_ref()
            .is_some_and(|reply| reply.id.value == thread_id.value)
}
