#[cfg(not(test))]
use crate::bindings::waddle::extension::host_tools;
use crate::bindings::waddle::extension::types;
use crate::constants::{COMMAND_NODE, COMMAND_RESULT_ID, COMMAND_RESULT_TEXT};
use crate::quotes::{quote_body, quote_catalog, quote_markup, select_quote_with_rng};
use crate::ui::{display, payload_namespace, plugin_id, timestamp};

#[cfg(test)]
use std::sync::OnceLock;

pub(crate) fn handle_command(
    command: types::CommandInvocation,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    handle_command_with_rng(command, random_u64)
}

pub(crate) fn handle_command_with_rng(
    command: types::CommandInvocation,
    mut next_u64: impl FnMut() -> u64,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if command.command_node.value != COMMAND_NODE {
        return Ok(vec![]);
    }
    if matches!(command.action, Some(types::CommandAction::Cancel)) {
        return Ok(vec![]);
    }
    let Some(room) = command.room else {
        return Ok(vec![types::ExtensionEffect::HostWarning(display(
            "Stargate quotes require an active channel.",
        ))]);
    };
    let quotes = quote_catalog()?;
    let quote = select_quote_with_rng(&quotes, &mut next_u64)?;
    send_room_message(&room, display(&quote_body(quote)), quote_markup(quote))?;
    Ok(vec![posted_result_effect()])
}

#[cfg(not(test))]
fn random_u64() -> u64 {
    crate::bindings::wasi::random::random::get_random_u64()
}

#[cfg(test)]
fn random_u64() -> u64 {
    0
}

fn posted_result_effect() -> types::ExtensionEffect {
    types::ExtensionEffect::EnrichMessage(types::ExtensionEnvelope {
        version: 1,
        enrichments: vec![types::MessageEnrichment {
            id: types::EnrichmentId {
                value: COMMAND_RESULT_ID.to_string(),
            },
            plugin: plugin_id(),
            capability: types::ExtensionCapability::MessageEnrich,
            payload_namespace: payload_namespace(),
            created_at: timestamp(),
            source: None,
            ui: vec![types::UiView {
                id: types::UiViewId {
                    value: COMMAND_RESULT_ID.to_string(),
                },
                title: Some(display("Stargate Quotes")),
                blocks: vec![types::UiBlock::Text(types::TextBlock {
                    text: display(COMMAND_RESULT_TEXT),
                    style: types::TextStyle::Body,
                })],
            }],
            payloads: vec![],
            launches: vec![],
        }],
    })
}

fn room_message_request(
    room: &types::RoomJid,
    body: types::DisplayText,
    markup: Vec<types::MessageMarkupSpan>,
) -> types::SendMessageRequest {
    types::SendMessageRequest {
        target: types::MessageTarget::Muc(room.clone()),
        body,
        thread_id: None,
        reply_to: None,
        markup,
        extensions: None,
    }
}

#[cfg(not(test))]
fn send_room_message(
    room: &types::RoomJid,
    body: types::DisplayText,
    markup: Vec<types::MessageMarkupSpan>,
) -> Result<(), types::ExtensionError> {
    host_tools::send_message(&room_message_request(room, body, markup))
        .map(|_| ())
        .map_err(extension_error_from_host_tool)
}

#[cfg(test)]
fn send_room_message(
    room: &types::RoomJid,
    body: types::DisplayText,
    markup: Vec<types::MessageMarkupSpan>,
) -> Result<(), types::ExtensionError> {
    sent_room_messages()
        .lock()
        .expect("sent room messages lock")
        .push(room_message_request(room, body, markup));
    Ok(())
}

#[cfg(test)]
fn sent_room_messages() -> &'static std::sync::Mutex<Vec<types::SendMessageRequest>> {
    static SENT_ROOM_MESSAGES: OnceLock<std::sync::Mutex<Vec<types::SendMessageRequest>>> =
        OnceLock::new();
    SENT_ROOM_MESSAGES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg(test)]
pub(crate) fn take_sent_room_messages() -> Vec<types::SendMessageRequest> {
    std::mem::take(
        &mut *sent_room_messages()
            .lock()
            .expect("sent room messages lock"),
    )
}

#[cfg(not(test))]
fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::Denied,
        types::HostToolErrorCode::InvalidRequest
        | types::HostToolErrorCode::NotFound
        | types::HostToolErrorCode::Unsupported => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    types::ExtensionError {
        code,
        message: error.message,
    }
}
