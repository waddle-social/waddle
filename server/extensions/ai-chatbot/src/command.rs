#[cfg(not(test))]
use crate::bindings::waddle::extension::host_tools;
use crate::bindings::waddle::extension::types;
#[cfg(test)]
use std::sync::OnceLock;

use crate::constants::COMMAND_NODE;
use crate::error::extension_error;
#[cfg(not(test))]
use crate::error::extension_error_from_host_tool;
use crate::model::{
    CleanPrompt, ExecutionContext, ProviderAnswer, ProviderConfig, ProviderExecutor, ResponseTarget,
};
use crate::prompt::clean_prompt;
use crate::provider::{assemble_provider_request, provider_execution_error};
use crate::tools::select_host_tools;
use crate::ui::{display, payload_namespace, plugin_id, timestamp};

pub(crate) fn handle_event_with_executor(
    event: types::ExtensionEvent,
    executor: &dyn ProviderExecutor,
) -> Result<types::ExtensionResponse, types::ExtensionError> {
    let effects = match event {
        types::ExtensionEvent::MessageHook(_) => vec![],
        types::ExtensionEvent::Command(command) => {
            return command_response(command, executor).map(|effect| types::ExtensionResponse {
                effects: effect.into_iter().collect(),
            });
        }
        types::ExtensionEvent::Launch(_) => vec![],
        types::ExtensionEvent::ProviderWebhook(_) => vec![],
    };
    Ok(types::ExtensionResponse { effects })
}

fn command_response(
    command: types::CommandInvocation,
    executor: &dyn ProviderExecutor,
) -> Result<Option<types::ExtensionEffect>, types::ExtensionError> {
    command_response_with_config(command, executor, crate::provider_config())
}

pub(crate) fn command_response_with_config(
    command: types::CommandInvocation,
    executor: &dyn ProviderExecutor,
    config: Result<ProviderConfig, types::ExtensionError>,
) -> Result<Option<types::ExtensionEffect>, types::ExtensionError> {
    if command.command_node.value != COMMAND_NODE {
        return Err(extension_error(
            types::ExtensionErrorCode::UnsupportedEvent,
            "unsupported ai-chatbot command",
        ));
    }
    if matches!(command.action, Some(types::CommandAction::Cancel)) {
        return Ok(None);
    }
    if command_prompt(&command).is_none()
        && !matches!(
            command.action,
            Some(types::CommandAction::Complete) | Some(types::CommandAction::Next)
        )
    {
        return Ok(Some(types::ExtensionEffect::CommandForm(
            prompt_command_form(),
        )));
    }
    let prompt = command_prompt(&command).ok_or_else(|| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "the /ai command requires a prompt field",
        )
    })?;
    let response_target = command_response_target(&command)?;
    let context = ExecutionContext::command(command.requester, prompt, response_target);
    execute_for_context_with_config(context, executor, config).map(Some)
}

fn prompt_command_form() -> types::DataForm {
    types::DataForm {
        form_type: types::DataFormType::Form,
        title: Some(display("Ask AI")),
        instructions: vec![display("Enter a prompt for the AI extension.")],
        fields: vec![
            types::DataFormField {
                name: types::UiActionId {
                    value: "prompt".to_string(),
                },
                field_type: types::FormFieldType::TextMulti,
                label: Some(display("Prompt")),
                required: true,
                values: vec![],
                options: vec![],
            },
            types::DataFormField {
                name: types::UiActionId {
                    value: "output".to_string(),
                },
                field_type: types::FormFieldType::ListSingle,
                label: Some(display("Output")),
                required: true,
                values: vec![types::DataFormValue {
                    value: "private".to_string(),
                }],
                options: vec![
                    types::FormFieldOption {
                        label: Some(display("Private")),
                        value: types::DataFormValue {
                            value: "private".to_string(),
                        },
                    },
                    types::FormFieldOption {
                        label: Some(display("Post to channel")),
                        value: types::DataFormValue {
                            value: "channel".to_string(),
                        },
                    },
                ],
            },
        ],
    }
}

fn execute_for_context_with_config(
    context: ExecutionContext,
    executor: &dyn ProviderExecutor,
    config: Result<ProviderConfig, types::ExtensionError>,
) -> Result<types::ExtensionEffect, types::ExtensionError> {
    let config = config?;
    let tools = select_host_tools(&context, config.context_limit);
    let provider_request = assemble_provider_request(&config, &context, tools);
    let answer = executor
        .execute(provider_request)
        .map_err(provider_execution_error)?;
    response_effect(context.response_target, answer)
}

fn command_prompt(command: &types::CommandInvocation) -> Option<CleanPrompt> {
    command
        .fields
        .iter()
        .find(|field| field.name.value == "prompt")
        .and_then(|field| field.values.first())
        .and_then(|value| CleanPrompt::new(clean_prompt(&value.value)))
}

fn command_response_target(
    command: &types::CommandInvocation,
) -> Result<Option<ResponseTarget>, types::ExtensionError> {
    let output = command
        .fields
        .iter()
        .find(|field| field.name.value == "output")
        .and_then(|field| field.values.first())
        .map(|value| value.value.as_str())
        .unwrap_or("private");
    match output {
        "private" => Ok(None),
        "channel" => {
            let Some(room) = command.room.clone() else {
                return Err(extension_error(
                    types::ExtensionErrorCode::InvalidRequest,
                    "posting an AI answer to a channel requires an active channel",
                ));
            };
            Ok(Some(ResponseTarget {
                room,
                thread_id: None,
                reply_to: None,
                focus_thread: false,
            }))
        }
        _ => Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "unsupported AI output target",
        )),
    }
}

fn response_effect(
    target: Option<ResponseTarget>,
    answer: ProviderAnswer,
) -> Result<types::ExtensionEffect, types::ExtensionError> {
    let Some(target) = target else {
        return Ok(command_answer_effect(answer));
    };
    send_room_message(&target, display(answer.text.as_str()))?;
    Ok(channel_posted_effect())
}

fn room_message_request(
    target: &ResponseTarget,
    body: types::DisplayText,
) -> types::SendMessageRequest {
    types::SendMessageRequest {
        target: types::MessageTarget::Muc(target.room.clone()),
        body,
        thread_id: target.thread_id.clone(),
        reply_to: target.reply_to.clone(),
        extensions: None,
    }
}

#[cfg(not(test))]
fn send_room_message(
    target: &ResponseTarget,
    body: types::DisplayText,
) -> Result<(), types::ExtensionError> {
    host_tools::send_message(&room_message_request(target, body))
        .map(|_| ())
        .map_err(extension_error_from_host_tool)
}

#[cfg(test)]
fn send_room_message(
    target: &ResponseTarget,
    body: types::DisplayText,
) -> Result<(), types::ExtensionError> {
    sent_room_messages()
        .lock()
        .expect("sent room messages lock")
        .push(room_message_request(target, body));
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

fn command_answer_effect(answer: ProviderAnswer) -> types::ExtensionEffect {
    command_text_effect("ai-command-answer", "AI answer", answer.text.as_str())
}

fn channel_posted_effect() -> types::ExtensionEffect {
    command_text_effect(
        "ai-command-posted",
        "AI answer posted",
        "AI answer posted to channel.",
    )
}

fn command_text_effect(id: &str, title: &str, text: &str) -> types::ExtensionEffect {
    types::ExtensionEffect::EnrichMessage(types::ExtensionEnvelope {
        version: 1,
        enrichments: vec![types::MessageEnrichment {
            id: types::EnrichmentId {
                value: id.to_string(),
            },
            plugin: plugin_id(),
            capability: types::ExtensionCapability::MessageEnrich,
            payload_namespace: payload_namespace(),
            created_at: timestamp(),
            source: None,
            ui: vec![types::UiView {
                id: types::UiViewId {
                    value: id.to_string(),
                },
                title: Some(display(title)),
                blocks: vec![types::UiBlock::Text(types::TextBlock {
                    text: display(text),
                    style: types::TextStyle::Body,
                })],
            }],
            payloads: vec![],
            launches: vec![],
        }],
    })
}
