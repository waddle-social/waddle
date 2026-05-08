#[cfg(not(test))]
use crate::bindings::waddle::extension::runtime;
use crate::bindings::waddle::extension::types;
#[cfg(not(test))]
use crate::constants::MAX_PROVIDER_REQUEST_BYTES;
use crate::constants::{
    BASELINE_SYSTEM_PROMPT, MAX_PROVIDER_ERROR_BODY_BYTES, MAX_PROVIDER_TOOL_CALLS_PER_ROUND,
    MAX_PROVIDER_TOOL_RESULT_BYTES, MAX_PROVIDER_TOOL_ROUNDS, OPENROUTER_ORIGIN,
    OPENROUTER_REFERER, OPENROUTER_TITLE,
};
use crate::error::extension_error;
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
        |body| execute_provider_http_request(&request, body),
        execute_provider_tool_call_content,
    )
}

#[cfg(test)]
pub(crate) fn execute_provider_request(
    request: ProviderRequest,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    let _ = &request.tool_target;
    let _ = &request.requester;
    Err(ProviderExecutionError::Http(
        "runtime HTTP is unavailable in unit tests".to_string(),
    ))
}

pub(crate) fn execute_provider_request_with_runtime(
    request: &ProviderRequest,
    mut execute_http: impl FnMut(String) -> Result<types::HttpResponse, ProviderExecutionError>,
    mut execute_tool: impl FnMut(&serde_json::Value, &ProviderRequest) -> Result<String, String>,
) -> Result<ProviderAnswer, ProviderExecutionError> {
    let mut messages = provider_messages_json(&request.messages);
    let tools = provider_tools_json(&request.tools);
    let mut tool_result_bytes = 0usize;
    for round in 0..MAX_PROVIDER_TOOL_ROUNDS {
        let body =
            provider_request_json_from_parts(request.model.as_str(), &messages, &tools, round == 0);
        let response = execute_http(body)?;
        let document = serde_json::from_str::<serde_json::Value>(&response.body)
            .map_err(|error| ProviderExecutionError::InvalidResponse(error.to_string()))?;
        let Some(message) = document.pointer("/choices/0/message") else {
            return Err(ProviderExecutionError::InvalidResponse(
                "provider response did not include a message".to_string(),
            ));
        };
        if let Some(tool_calls) = message
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
            .filter(|calls| !calls.is_empty())
        {
            messages.push(message.clone());
            for (index, tool_call) in tool_calls.iter().enumerate() {
                let id = provider_tool_call_id(tool_call);
                let content = if index >= MAX_PROVIDER_TOOL_CALLS_PER_ROUND {
                    "Error: provider tool-call limit exceeded".to_string()
                } else {
                    match provider_tool_call_name(tool_call)
                        .ok_or_else(|| "unsupported tool ".to_string())
                        .and_then(|name| provider_tool_available(request, name))
                        .and_then(|()| execute_tool(tool_call, request))
                    {
                        Ok(content) => content,
                        Err(error) => format!("Error: {error}"),
                    }
                };
                messages.push(provider_tool_message(
                    id,
                    bound_provider_tool_result(content, &mut tool_result_bytes),
                ));
            }
            continue;
        }
        return parse_provider_answer_from_document(&document);
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

fn provider_tool_message(id: &str, content: String) -> serde_json::Value {
    serde_json::json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content,
    })
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
pub(crate) fn provider_request_json(request: &ProviderRequest) -> String {
    let messages = provider_messages_json(&request.messages);
    let tools = provider_tools_json(&request.tools);
    provider_request_json_from_parts(request.model.as_str(), &messages, &tools, true)
}

fn provider_messages_json(messages: &[ProviderMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|message| {
            serde_json::json!({
                "role": message.role.as_str(),
                "content": message.content.as_str(),
            })
        })
        .collect()
}

pub(crate) fn provider_request_json_from_parts(
    model: &str,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    _initial_request: bool,
) -> String {
    let mut request = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": 0.2,
    });
    if !tools.is_empty() {
        request["tools"] = serde_json::Value::Array(tools.to_vec());
        request["tool_choice"] = serde_json::json!("auto");
        request["parallel_tool_calls"] = serde_json::json!(false);
    }
    request.to_string()
}

fn provider_tools_json(tools: &[HostToolRequest]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| match tool.tool {
            HostTool::QueryMam => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "query_mam",
                    "description": "Query XMPP Message Archive Management for room history or a direct conversation. Use only when the prompt needs archived Waddle messages.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "target": {
                                "type": "object",
                                "description": "Omit to use the current room when available.",
                                "properties": {
                                    "kind": { "type": "string", "enum": ["room", "conversation"] },
                                    "jid": { "type": "string", "description": "Room JID for kind=room, or peer bare JID for kind=conversation" }
                                },
                                "required": ["kind"]
                            },
                            "start": { "type": "string", "description": "Optional ISO-8601 start timestamp" },
                            "end": { "type": "string", "description": "Optional ISO-8601 end timestamp" },
                            "thread_id": { "type": "string" },
                            "sender": { "type": "string", "description": "Optional bare JID sender filter" },
                            "text": {
                                "type": "string",
                                "description": "Optional full-text search terms"
                            },
                            "max_results": {
                                "type": "integer",
                                "description": "Maximum results to return, capped by Waddle"
                            }
                        }
                    }
                }
            }),
            HostTool::Members => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "list_room_members",
                    "description": "List visible members/occupants for a MUC room.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "room": { "type": "string", "description": "Room JID; omitted means the current room" }
                        }
                    }
                }
            }),
            HostTool::Presence => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "get_presence",
                    "description": "Get the requester's presence resources. Available only during /ai command invocations.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "subject": { "type": "string", "description": "Bare JID; omitted means the requester" }
                        }
                    }
                }
            }),
            HostTool::Roster => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "get_roster",
                    "description": "Get the requester's XMPP roster. Available only during /ai command invocations.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Bare JID; omitted means the requester" }
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

#[cfg(not(test))]
fn execute_provider_http_request(
    request: &ProviderRequest,
    body: String,
) -> Result<types::HttpResponse, ProviderExecutionError> {
    if body.len() > MAX_PROVIDER_REQUEST_BYTES {
        return Err(ProviderExecutionError::Http(
            "provider request body exceeded extension limit".to_string(),
        ));
    }
    let response = runtime::http_request(&types::OutgoingHttpRequest {
        method: types::HttpMethod::Post,
        url: types::Url {
            value: request.endpoint.as_str().to_string(),
        },
        headers: provider_request_headers(request),
        body: Some(body),
    })
    .map_err(|error| ProviderExecutionError::Http(error.message.value))?;

    if !(200..300).contains(&response.status) {
        return Err(ProviderExecutionError::HttpStatus {
            status: response.status,
            body: response.body,
        });
    }
    Ok(response)
}

pub(crate) fn provider_request_headers(request: &ProviderRequest) -> Vec<types::HttpHeader> {
    let mut headers = vec![
        types::HttpHeader {
            name: "authorization".to_string(),
            value: format!("Bearer {}", request.api_key.as_str()),
        },
        types::HttpHeader {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        },
        types::HttpHeader {
            name: "accept".to_string(),
            value: "application/json".to_string(),
        },
    ];
    if request.endpoint.as_str().starts_with(OPENROUTER_ORIGIN) {
        headers.push(types::HttpHeader {
            name: "http-referer".to_string(),
            value: OPENROUTER_REFERER.to_string(),
        });
        headers.push(types::HttpHeader {
            name: "x-title".to_string(),
            value: OPENROUTER_TITLE.to_string(),
        });
        headers.push(types::HttpHeader {
            name: "x-openrouter-title".to_string(),
            value: OPENROUTER_TITLE.to_string(),
        });
    }
    headers
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
    let text = NonEmptyString::new(content).ok_or_else(|| {
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
        content: NonEmptyString::new(BASELINE_SYSTEM_PROMPT).expect("baseline prompt is non-empty"),
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
    ProviderRequest {
        endpoint: config.endpoint.clone(),
        model: config.model.clone(),
        api_key: config.api_key.clone(),
        context_limit: config.context_limit,
        messages,
        tools,
        tool_target: context.response_target.clone(),
        requester: context.requester.clone(),
    }
}

pub(crate) fn provider_execution_error(error: ProviderExecutionError) -> types::ExtensionError {
    match error {
        ProviderExecutionError::Http(error) => {
            extension_error(types::ExtensionErrorCode::TemporaryFailure, &error)
        }
        ProviderExecutionError::HttpStatus { status, body } => extension_error(
            types::ExtensionErrorCode::TemporaryFailure,
            &provider_status_error_message(status, &body),
        ),
        ProviderExecutionError::InvalidResponse(error) => {
            extension_error(types::ExtensionErrorCode::TemporaryFailure, &error)
        }
    }
}

fn provider_status_error_message(status: u16, body: &str) -> String {
    let body = provider_error_body_summary(body);
    if body.is_empty() {
        return format!("AI provider returned HTTP {status}");
    }
    format!("AI provider returned HTTP {status}: {body}")
}

fn provider_error_body_summary(body: &str) -> String {
    let summary = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|document| {
            document
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    document
                        .pointer("/error")
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| document.get("message").and_then(serde_json::Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    summary
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .take(MAX_PROVIDER_ERROR_BODY_BYTES)
        .collect::<String>()
        .trim()
        .to_string()
}
