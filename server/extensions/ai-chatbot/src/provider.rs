#[cfg(not(test))]
use crate::bindings::waddle::extension::inference;
use crate::bindings::waddle::extension::types;
#[cfg(not(test))]
use crate::constants::MAX_PROVIDER_RESPONSE_BYTES;
use crate::constants::{
    BASELINE_SYSTEM_PROMPT, MAX_CONTEXT_BYTES, MAX_PROVIDER_TOOL_CALLS_PER_ROUND,
    MAX_PROVIDER_TOOL_RESULT_BYTES, MAX_PROVIDER_TOOL_ROUNDS,
};
use crate::error::extension_error;
#[cfg(not(test))]
use crate::model::CredentialPreference;
use crate::model::{
    ExecutionContext, HostTool, HostToolRequest, NonEmptyString, ProviderAnswer, ProviderConfig,
    ProviderExecutionError, ProviderMessage, ProviderRequest, ProviderRole,
};
use crate::text::truncate_context_line;
#[cfg(not(test))]
use crate::tools::execute_provider_tool_call_content;

#[cfg(not(test))]
pub(crate) fn execute_provider_request(
    request: ProviderRequest,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    execute_provider_request_with_runtime(
        &request,
        |messages| execute_brokered_inference_stream(&request, messages),
        execute_provider_tool_call_content,
    )
}

#[cfg(test)]
pub(crate) fn execute_provider_request(
    request: ProviderRequest,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    let _ = (&request.tool_target, &request.credential);
    Err(ProviderExecutionError::BrokerFailure(
        "brokered inference stream is unavailable in unit tests".to_string(),
    ))
}

#[cfg(not(test))]
fn execute_brokered_inference_stream(
    request: &ProviderRequest,
    messages: &[inference::InferenceMessage],
) -> Result<String, ProviderExecutionError> {
    let credential = match &request.credential {
        CredentialPreference::InvokingUser => {
            inference::CredentialRef::InvokingUser(request.provider.as_str().to_string())
        }
        CredentialPreference::Tenant(name) => {
            inference::CredentialRef::Tenant(name.as_str().to_string())
        }
    };
    let stream = inference::start_stream(&inference::InferenceRequest {
        credential,
        provider: types::ProviderId {
            value: request.provider.as_str().to_string(),
        },
        deployment: request.deployment.as_str().to_string(),
        model: types::ModelId {
            value: request.model.as_str().to_string(),
        },
        messages: messages.to_vec(),
        max_output_tokens: None,
    })
    .map_err(map_inference_error)?;

    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next_chunk().map_err(map_inference_error)? {
        if buffer.len().saturating_add(chunk.delta.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(ProviderExecutionError::InvalidResponse(
                "brokered inference stream exceeded response limit".to_string(),
            ));
        }
        buffer.extend_from_slice(&chunk.delta);
    }
    let _ = stream.terminal_usage();
    String::from_utf8(buffer).map_err(|_| {
        ProviderExecutionError::InvalidResponse(
            "brokered inference stream was not valid UTF-8".to_string(),
        )
    })
}

#[cfg(not(test))]
fn map_inference_error(err: inference::InferenceError) -> ProviderExecutionError {
    match err.code {
        inference::InferenceErrorCode::Denied => {
            ProviderExecutionError::Denied("broker denied inference request".to_string())
        }
        inference::InferenceErrorCode::Canceled => ProviderExecutionError::Canceled,
        inference::InferenceErrorCode::RateLimited => ProviderExecutionError::RateLimited,
        inference::InferenceErrorCode::InvalidRequest => {
            ProviderExecutionError::InvalidResponse("broker rejected inference request".to_string())
        }
        inference::InferenceErrorCode::TemporaryFailure => ProviderExecutionError::BrokerFailure(
            "brokered inference temporarily unavailable".to_string(),
        ),
        inference::InferenceErrorCode::ChunkLimitExceeded => {
            ProviderExecutionError::InvalidResponse("stream chunk limit exceeded".to_string())
        }
    }
}

#[cfg(not(test))]
type StreamMessage = inference::InferenceMessage;

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct StreamMessage {
    pub(crate) role: String,
    pub(crate) content: String,
}

pub(crate) fn execute_provider_request_with_runtime(
    request: &ProviderRequest,
    mut run_stream: impl FnMut(&[StreamMessage]) -> Result<String, ProviderExecutionError>,
    mut execute_tool: impl FnMut(&serde_json::Value, &ProviderRequest) -> Result<String, String>,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    let mut messages = request
        .messages
        .iter()
        .map(|message| StreamMessage {
            role: message.role.as_str().to_string(),
            content: message.content.as_str().to_string(),
        })
        .collect::<Vec<_>>();
    let mut tool_result_bytes = 0usize;
    for _round in 0..MAX_PROVIDER_TOOL_ROUNDS {
        let raw_output = run_stream(&messages)?;
        if let Ok(document) = serde_json::from_str::<serde_json::Value>(&raw_output) {
            if let Some(message) = document.pointer("/choices/0/message") {
                if let Some(tool_calls) = message
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                    .filter(|calls| !calls.is_empty())
                {
                    for (index, tool_call) in tool_calls.iter().enumerate() {
                        let id = provider_tool_call_id(tool_call);
                        let bounded = if index >= MAX_PROVIDER_TOOL_CALLS_PER_ROUND {
                            "Error: provider tool-call limit exceeded".to_string()
                        } else {
                            let content = match provider_tool_call_name(tool_call)
                                .ok_or_else(|| "unsupported tool ".to_string())
                                .and_then(|name| provider_tool_available(request, name))
                                .and_then(|()| execute_tool(tool_call, request))
                            {
                                Ok(content) => content,
                                Err(error) => format!("Error: {error}"),
                            };
                            bound_provider_tool_result(content, &mut tool_result_bytes)
                        };
                        messages.push(StreamMessage {
                            role: "tool".to_string(),
                            content: serde_json::json!({
                                "tool_call_id": id,
                                "content": bounded,
                            })
                            .to_string(),
                        });
                    }
                    continue;
                }
                return parse_provider_answer_from_document(&document);
            }
        }
        let text = NonEmptyString::try_new(raw_output).ok_or_else(|| {
            ProviderExecutionError::InvalidResponse("provider answer was empty".to_string())
        })?;
        return Ok(ProviderAnswer { text });
    }
    Err(ProviderExecutionError::InvalidResponse(
        "provider exceeded tool-call round limit".to_string(),
    ))
}

fn provider_tool_call_name(tool_call: &serde_json::Value) -> Option<&str> {
    tool_call
        .pointer("/function/name")
        .and_then(serde_json::Value::as_str)
}

pub(crate) fn provider_tool_available(request: &ProviderRequest, name: &str) -> Result<(), String> {
    let Some(tool) = HostTool::from_provider_name(name) else {
        return Err(format!("unsupported tool {name}"));
    };
    if request.tools.iter().any(|request| request.tool == tool) {
        Ok(())
    } else {
        Err(format!("tool {name} was not available for this request"))
    }
}

fn provider_tool_call_id(tool_call: &serde_json::Value) -> &str {
    tool_call
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("missing-tool-call-id")
}

fn bound_provider_tool_result(content: String, tool_result_bytes: &mut usize) -> String {
    if *tool_result_bytes >= MAX_PROVIDER_TOOL_RESULT_BYTES {
        return "Error: provider tool-result budget exceeded".to_string();
    }
    let remaining = MAX_PROVIDER_TOOL_RESULT_BYTES - *tool_result_bytes;
    let bounded = truncate_context_line(&content, remaining);
    *tool_result_bytes += bounded.len();
    bounded
}

#[cfg(test)]
pub(crate) fn provider_tools_json(tools: &[HostToolRequest]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| match tool.tool {
            HostTool::QueryMam => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "query_mam",
                    "description": "Query XMPP Message Archive Management for current room history.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "target": {
                                "type": "object",
                                "description": "Omit to use the current room.",
                                "properties": {
                                    "kind": { "type": "string", "enum": ["room"] },
                                    "jid": { "type": "string", "description": "Current room JID" }
                                },
                                "required": ["kind"]
                            },
                            "start": { "type": "string" },
                            "end": { "type": "string" },
                            "thread_id": { "type": "string" },
                            "sender": { "type": "string" },
                            "text": { "type": "string" },
                            "max_results": { "type": "integer" }
                        }
                    }
                }
            }),
            HostTool::Members => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "list_room_members",
                    "description": "List visible members/occupants for the active MUC room.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "room": { "type": "string" }
                        }
                    }
                }
            }),
            HostTool::Channels => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "list_channels",
                    "description": "List channels visible to the requester.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }),
            HostTool::Spaces => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "list_spaces",
                    "description": "List spaces visible to the requester.",
                    "parameters": { "type": "object", "properties": {} }
                }
            }),
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn parse_provider_answer(input: &str) -> Result<ProviderAnswer, ProviderExecutionError> {
    let document = serde_json::from_str::<serde_json::Value>(input)
        .map_err(|error| ProviderExecutionError::InvalidResponse(error.to_string()))?;
    parse_provider_answer_from_document(&document)
}

fn parse_provider_answer_from_document(
    document: &serde_json::Value,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    let content = document
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .or_else(|| document.get("text").and_then(serde_json::Value::as_str))
        .ok_or_else(|| {
            ProviderExecutionError::InvalidResponse(
                "provider response did not include answer text".to_string(),
            )
        })?;
    let text = NonEmptyString::try_new(content).ok_or_else(|| {
        ProviderExecutionError::InvalidResponse("provider answer was empty".to_string())
    })?;
    Ok(ProviderAnswer { text })
}

pub(crate) fn assemble_provider_request(
    config: &ProviderConfig,
    context: &ExecutionContext,
    tools: Vec<HostToolRequest>,
) -> ProviderRequest {
    let mut messages = Vec::new();
    messages.push(ProviderMessage {
        role: ProviderRole::System,
        content: NonEmptyString::try_new(BASELINE_SYSTEM_PROMPT)
            .expect("baseline prompt is non-empty"),
    });
    if let Some(system_prompt) = &config.system_prompt {
        messages.push(ProviderMessage {
            role: ProviderRole::System,
            content: system_prompt.clone(),
        });
    }
    messages.push(ProviderMessage {
        role: ProviderRole::User,
        content: context.prompt.0.clone(),
    });
    let _ = MAX_CONTEXT_BYTES;
    ProviderRequest {
        provider: config.provider.clone(),
        deployment: config.deployment.clone(),
        model: config.model.clone(),
        credential: config.credential.clone(),
        context_limit: config.context_limit,
        messages,
        tools,
        tool_target: context.response_target.clone(),
    }
}

pub(crate) fn provider_execution_error(error: ProviderExecutionError) -> types::ExtensionError {
    match error {
        ProviderExecutionError::Denied(msg) => {
            extension_error(types::ExtensionErrorCode::Denied, &msg)
        }
        #[cfg(not(test))]
        ProviderExecutionError::Canceled => {
            extension_error(types::ExtensionErrorCode::Denied, "inference canceled")
        }
        #[cfg(not(test))]
        ProviderExecutionError::RateLimited => extension_error(
            types::ExtensionErrorCode::TemporaryFailure,
            "inference rate limited",
        ),
        ProviderExecutionError::BrokerFailure(error)
        | ProviderExecutionError::InvalidResponse(error) => {
            extension_error(types::ExtensionErrorCode::TemporaryFailure, &error)
        }
    }
}
