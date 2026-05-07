use super::{
    assemble_provider_request, clean_prompt, command_response_with_config,
    execute_provider_request_with_runtime, extension_error, format_archived_messages, manifest,
    parse_provider_answer, provider_execution_error, provider_request_headers,
    provider_request_json, provider_request_json_from_parts, provider_tool_mam_query,
    select_host_tools, types, CleanPrompt, ExecutionContext, HostTool, NonEmptyString,
    ProviderAnswer, ProviderConfig, ProviderExecutionError, ProviderExecutor, ProviderRequest,
    ProviderRole, ResponseTarget, BASELINE_SYSTEM_PROMPT, COMMAND_NODE, MAX_CONTEXT_BYTES,
    MAX_CONTEXT_LINE_BYTES, MAX_PROVIDER_TOOL_CALLS_PER_ROUND, OPENROUTER_REFERER,
    OPENROUTER_TITLE,
};

struct FakeExecutor {
    answer: Result<ProviderAnswer, ProviderExecutionError>,
}

impl ProviderExecutor for FakeExecutor {
    fn execute(&self, _request: ProviderRequest) -> Result<ProviderAnswer, ProviderExecutionError> {
        self.answer.clone()
    }
}

#[test]
fn clean_prompt_strips_ai_command_and_mentions_case_insensitively() {
    assert_eq!(clean_prompt(" /AI @WADDLE summarize "), "summarize");
    assert_eq!(clean_prompt("@wAdDlE continue"), "continue");
    assert_eq!(clean_prompt("@waddle_bot continue"), "@waddle_bot continue");
    assert_eq!(clean_prompt("@waddleBot continue"), "@waddleBot continue");
    assert_eq!(
        clean_prompt("alice@waddle.social can help"),
        "alice@waddle.social can help"
    );
    assert_eq!(clean_prompt("☃ /ai later"), "☃ /ai later");
    assert_eq!(clean_prompt("/airship @WADDLE"), "/airship @WADDLE");
    assert_eq!(
        clean_prompt("/ai what does @waddle mean?"),
        "what does @waddle mean?"
    );
}

#[test]
fn manifest_registers_slash_ai_as_extension_command() {
    let manifest = manifest();
    assert_eq!(manifest.commands.len(), 1);
    assert_eq!(manifest.commands[0].node.value, COMMAND_NODE);
    assert_eq!(manifest.commands[0].name.value, "/ai");
    assert!(matches!(
        manifest.commands[0].scope,
        types::CommandScope::Global,
    ));
    assert_eq!(
        manifest.capabilities,
        vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::HostMamRead,
            types::ExtensionCapability::HostMembersRead,
            types::ExtensionCapability::HostPresenceRead,
            types::ExtensionCapability::HostRosterRead,
            types::ExtensionCapability::HostChannelsRead,
            types::ExtensionCapability::HostSpacesRead,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::OutboundHttpRequest,
            types::ExtensionCapability::Commands,
        ]
    );
}

#[test]
fn parses_provider_configuration_contract() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value","system_prompt":"Use XMPP context.","context_limit":8}"#,
    )
    .expect("provider config");
    assert_eq!(
        config.endpoint.as_str(),
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(config.model.as_str(), "waddle-test");
    assert_eq!(config.api_key.as_str(), "secret-value");
    assert_eq!(config.system_prompt.unwrap().as_str(), "Use XMPP context.");
    assert_eq!(config.context_limit, 8);
}

#[test]
fn selects_tools_for_context_assembly() {
    let context = execution_context("summarize this channel and roster for the space");
    let tools = select_host_tools(&context, 5);
    let kinds: Vec<_> = tools.iter().map(|request| request.tool).collect();
    assert_eq!(kinds, vec![HostTool::QueryMam, HostTool::Members]);

    let command_context = command_execution_context("summarize my roster for this space");
    let tools = select_host_tools(&command_context, 5);
    let kinds: Vec<_> = tools.iter().map(|request| request.tool).collect();
    assert_eq!(
        kinds,
        vec![
            HostTool::QueryMam,
            HostTool::Channels,
            HostTool::Spaces,
            HostTool::Presence,
            HostTool::Roster,
        ]
    );

    let no_context_tools = select_host_tools(&context, 0);
    assert!(!no_context_tools
        .iter()
        .any(|request| request.tool == HostTool::QueryMam));
}

#[test]
fn requester_private_tool_selection_stays_command_scoped() {
    let room_context = execution_context("who is online in this room?");
    assert!(!select_host_tools(&room_context, 5)
        .iter()
        .any(|request| request.tool == HostTool::Presence));

    let requester_context = command_execution_context("what is my status?");
    assert!(select_host_tools(&requester_context, 5)
        .iter()
        .any(|request| request.tool == HostTool::Presence));

    let room_roster_context = execution_context("show my roster");
    assert!(!select_host_tools(&room_roster_context, 5)
        .iter()
        .any(|request| request.tool == HostTool::Roster));
}

#[test]
fn assembles_provider_request_with_prompt_and_tool_schemas_without_initial_context_injection() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value","system_prompt":"Be concise."}"#,
    )
    .expect("provider config");
    let context = execution_context("summarize this thread");
    let tools = select_host_tools(&context, config.context_limit);
    let request = assemble_provider_request(&config, &context, tools);
    assert_eq!(
        request.endpoint.as_str(),
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(request.model.as_str(), "waddle-test");
    assert_eq!(request.api_key.as_str(), "secret-value");
    assert_eq!(request.messages.len(), 3);
    assert_eq!(request.messages[0].content.as_str(), BASELINE_SYSTEM_PROMPT);
    assert_eq!(request.messages[1].content.as_str(), "Be concise.");
    assert_eq!(
        request.messages[2].content.as_str(),
        "summarize this thread"
    );
    assert_eq!(request.messages[2].role, ProviderRole::User);
    assert!(request.messages.iter().all(|message| {
        !message
            .content
            .as_str()
            .contains("Untrusted Waddle context")
            && !message
                .content
                .as_str()
                .contains("room members: alice, bob")
            && !message.content.as_str().contains("waddle context sources")
            && !message.content.as_str().contains("waddle context:")
    }));
    assert!(request
        .tools
        .iter()
        .any(|tool| tool.tool == HostTool::QueryMam));
}

#[test]
fn serializes_openai_compatible_provider_request() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value","system_prompt":"Be concise."}"#,
    )
    .expect("provider config");
    let context = execution_context("summarize this thread");
    let tools = select_host_tools(&context, config.context_limit);
    let request = assemble_provider_request(&config, &context, tools);
    let body = provider_request_json(&request);
    assert!(body.contains("\"model\":\"waddle-test\""));
    assert!(body.contains("\"role\":\"system\""));
    assert!(body.contains("\"role\":\"user\""));
    assert!(body.contains("summarize this thread"));
    assert!(body.contains("\"tools\""));
    assert!(body.contains("\"tool_choice\":\"auto\""));
    assert!(body.contains("\"name\":\"query_mam\""));
    assert!(body.contains("\"max_results\""));
    assert!(!body.contains("Untrusted Waddle context"));
    assert!(!body.contains("waddle context sources"));
}

#[test]
fn provider_tool_choice_is_auto_for_initial_and_followup_requests() {
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": "summarize"
    })];
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "query_mam",
            "parameters": {
                "type": "object"
            }
        }
    })];

    let initial = provider_request_json_from_parts("waddle-test", &messages, &tools, true);
    let followup = provider_request_json_from_parts("waddle-test", &messages, &tools, false);

    assert!(initial.contains("\"tool_choice\":\"auto\""));
    assert!(followup.contains("\"tool_choice\":\"auto\""));
}

#[test]
fn provider_loop_allows_initial_answer_without_forced_tool_call() {
    let request = provider_request_for_loop_test();
    let mut bodies = Vec::new();
    let mut responses =
        vec![r#"{"choices":[{"message":{"content":"summary without tool"}}]}"#.to_string()]
            .into_iter();

    let answer = execute_provider_request_with_runtime(
        &request,
        |body| {
            bodies.push(body);
            Ok(types::HttpResponse {
                status: 200,
                body: responses.next().expect("provider response"),
            })
        },
        |tool_call, request| {
            panic!("unexpected tool call {tool_call:?} for request {request:?}");
        },
    )
    .expect("provider answer");

    assert_eq!(answer.text.as_str(), "summary without tool");
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).expect("first body");
    assert_eq!(bodies.len(), 1);
    assert_eq!(first["tool_choice"], "auto");
    assert!(first["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty()));
}

#[test]
fn provider_loop_rejects_tools_not_advertised_for_context() {
    let request = provider_request_for_loop_test();
    assert!(!request
        .tools
        .iter()
        .any(|request| request.tool == HostTool::Roster));
    let mut bodies = Vec::new();
    let mut responses = vec![
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"get_roster","arguments":"{}"}}]}}]}"#
            .to_string(),
        r#"{"choices":[{"message":{"content":"done"}}]}"#.to_string(),
    ]
    .into_iter();

    let answer = execute_provider_request_with_runtime(
        &request,
        |body| {
            bodies.push(body);
            Ok(types::HttpResponse {
                status: 200,
                body: responses.next().expect("provider response"),
            })
        },
        |tool_call, request| {
            panic!("unavailable tool executed: {tool_call:?} for request {request:?}");
        },
    )
    .expect("provider answer");

    assert_eq!(answer.text.as_str(), "done");
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("second body");
    assert!(second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool"
            && message["content"] == "Error: tool get_roster was not available for this request"));
}

#[test]
fn provider_loop_caps_tool_calls_per_round() {
    let request = provider_request_for_loop_test();
    let tool_calls = (0..(MAX_PROVIDER_TOOL_CALLS_PER_ROUND + 2))
        .map(|index| {
            serde_json::json!({
                "id": format!("call-{index}"),
                "type": "function",
                "function": {
                    "name": "query_mam",
                    "arguments": "{}"
                }
            })
        })
        .collect::<Vec<_>>();
    let mut bodies = Vec::new();
    let mut responses = vec![
        serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls
                }
            }]
        })
        .to_string(),
        r#"{"choices":[{"message":{"content":"done"}}]}"#.to_string(),
    ]
    .into_iter();
    let mut executed_tool_calls = 0usize;

    let answer = execute_provider_request_with_runtime(
        &request,
        |body| {
            bodies.push(body);
            Ok(types::HttpResponse {
                status: 200,
                body: responses.next().expect("provider response"),
            })
        },
        |_tool_call, _target| {
            executed_tool_calls += 1;
            Ok("tool context".to_string())
        },
    )
    .expect("provider answer");

    assert_eq!(answer.text.as_str(), "done");
    assert_eq!(executed_tool_calls, MAX_PROVIDER_TOOL_CALLS_PER_ROUND);
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("second body");
    let tool_messages = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), MAX_PROVIDER_TOOL_CALLS_PER_ROUND + 2);
    assert!(tool_messages
        .iter()
        .any(|message| message["content"] == "Error: provider tool-call limit exceeded"));
}

#[test]
fn provider_loop_caps_aggregate_tool_result_bytes() {
    let request = provider_request_for_loop_test();
    let tool_calls = (0..3)
        .map(|index| {
            serde_json::json!({
                "id": format!("call-{index}"),
                "type": "function",
                "function": {
                    "name": "query_mam",
                    "arguments": "{}"
                }
            })
        })
        .collect::<Vec<_>>();
    let mut bodies = Vec::new();
    let mut responses = vec![
        serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls
                }
            }]
        })
        .to_string(),
        r#"{"choices":[{"message":{"content":"done"}}]}"#.to_string(),
    ]
    .into_iter();
    let large_tool_result = "a".repeat(MAX_CONTEXT_BYTES / 2 + 64);

    execute_provider_request_with_runtime(
        &request,
        |body| {
            bodies.push(body);
            Ok(types::HttpResponse {
                status: 200,
                body: responses.next().expect("provider response"),
            })
        },
        |_tool_call, _target| Ok(large_tool_result.clone()),
    )
    .expect("provider answer");

    let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("second body");
    let tool_contents = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(tool_contents.len(), 3);
    assert!(tool_contents[1].ends_with("[truncated]"));
    assert_eq!(
        tool_contents[2],
        "Error: provider tool-result budget exceeded"
    );
}

#[test]
fn slash_ai_search_stays_provider_prompt_with_message_search_tool() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value"}"#,
    )
    .expect("provider config");
    let context = execution_context(&clean_prompt("/ai search release notes"));
    let tools = select_host_tools(&context, config.context_limit);
    let request = assemble_provider_request(&config, &context, tools);

    assert_eq!(
        request.messages.last().unwrap().content.as_str(),
        "search release notes"
    );
    assert!(request
        .tools
        .iter()
        .any(|tool| tool.tool == HostTool::QueryMam));
}

#[test]
fn provider_message_tools_build_xep_mam_queries() {
    let mut target = execution_context("find the deploy note")
        .response_target
        .expect("room target");
    target.focus_thread = true;
    target.thread_id = Some(types::ThreadId {
        value: "thread-root".to_string(),
    });

    let request = ProviderRequest {
        endpoint: NonEmptyString::new("https://api.example.test/v1/chat/completions")
            .expect("endpoint"),
        model: NonEmptyString::new("waddle-test").expect("model"),
        api_key: NonEmptyString::new("secret").expect("api key"),
        context_limit: 7,
        messages: vec![],
        tools: vec![],
        tool_target: Some(target.clone()),
        requester: None,
    };
    let search = provider_tool_mam_query(
        &request,
        &serde_json::json!({ "text": "deploy note", "max_results": 50 }),
    )
    .expect("search query");
    match search.target {
        types::MamTarget::Room(room) => assert_eq!(room.value, "chat@muc.example.com"),
        other => panic!("unexpected MAM target: {other:?}"),
    }
    assert_eq!(search.text.unwrap().value, "deploy note");
    assert_eq!(search.thread_id.unwrap().value, "thread-root");
    assert_eq!(search.max_results, 7);

    let recent = provider_tool_mam_query(&request, &serde_json::json!({})).expect("recent query");
    assert!(recent.text.is_none());
    assert_eq!(recent.thread_id.unwrap().value, "thread-root");
    assert_eq!(recent.max_results, 7);

    let cross_room = provider_tool_mam_query(
        &request,
        &serde_json::json!({
            "target": { "kind": "room", "jid": "other@muc.example.com" }
        }),
    );
    assert_eq!(
        cross_room.expect_err("cross-room query rejected"),
        "query_mam room invocations cannot target another room"
    );

    let dm = provider_tool_mam_query(
        &request,
        &serde_json::json!({
            "target": { "kind": "conversation", "jid": "bob@example.com" }
        }),
    );
    assert_eq!(
        dm.expect_err("room hook cannot query DM"),
        "query_mam room invocations cannot target a direct conversation"
    );

    let mut disabled_request = request.clone();
    disabled_request.context_limit = 0;
    let disabled = provider_tool_mam_query(&disabled_request, &serde_json::json!({}));
    assert_eq!(
        disabled.expect_err("context disabled"),
        "query_mam is disabled by context_limit"
    );
}

#[test]
fn provider_tool_results_are_bounded_before_next_provider_request() {
    let target = execution_context("summarize")
        .response_target
        .expect("room target");
    let long_body = "a".repeat(MAX_CONTEXT_LINE_BYTES * 2);
    let result = format_archived_messages(
        vec![types::ArchivedMessage {
            stanza_id: types::StanzaId {
                value: "msg-1".to_string(),
            },
            from_jid: types::Jid {
                value: "alice@example.com".to_string(),
            },
            to_jid: types::Jid {
                value: "chat@muc.example.com".to_string(),
            },
            sent_at: types::Timestamp {
                value: "2026-05-02T12:00:00Z".to_string(),
            },
            body: Some(types::DisplayText { value: long_body }),
            thread_id: None,
            reply_to: None,
        }],
        Some(&target),
    );

    assert!(result.len() <= MAX_CONTEXT_LINE_BYTES);
    assert!(result.ends_with("[truncated]"));
}

#[test]
fn focused_thread_tool_results_include_root_stanza() {
    let mut target = execution_context("summarize")
        .response_target
        .expect("room target");
    target.focus_thread = true;
    target.thread_id = Some(types::ThreadId {
        value: "thread-root".to_string(),
    });
    let result = format_archived_messages(
        vec![
            archived_message("thread-root", None, None, "root body"),
            archived_message("reply-1", Some("thread-root"), None, "thread reply"),
            archived_message("other", None, None, "outside thread"),
        ],
        Some(&target),
    );

    assert!(result.contains("root body"));
    assert!(result.contains("thread reply"));
    assert!(!result.contains("outside thread"));
}

#[test]
fn adds_openrouter_headers_for_openrouter_endpoint() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://openrouter.ai/api/v1/chat/completions","model":"openrouter/auto","api_key":"secret-value"}"#,
    )
    .expect("provider config");
    let request = assemble_provider_request(&config, &execution_context("answer"), vec![]);
    let headers = provider_request_headers(&request);
    assert!(headers
        .iter()
        .any(|header| header.name == "authorization" && header.value == "Bearer secret-value"));
    assert!(headers
        .iter()
        .any(|header| header.name == "accept" && header.value == "application/json"));
    assert!(headers
        .iter()
        .any(|header| header.name == "http-referer" && header.value == OPENROUTER_REFERER));
    assert!(headers
        .iter()
        .any(|header| header.name == "x-openrouter-title" && header.value == OPENROUTER_TITLE));
}

#[test]
fn provider_config_trims_secret_file_newline() {
    let config = ProviderConfig::parse(
        "{\"endpoint\":\"https://openrouter.ai/api/v1/chat/completions\",\"model\":\"openrouter/auto\",\"api_key\":\"secret-value\\n\"}",
    )
    .expect("provider config");
    assert_eq!(config.api_key.as_str(), "secret-value");
}

#[test]
fn parses_openai_compatible_provider_answer() {
    let answer =
        parse_provider_answer(r#"{"choices":[{"message":{"content":"extension-owned answer"}}]}"#)
            .expect("provider answer");
    assert_eq!(answer.text.as_str(), "extension-owned answer");
}

#[test]
fn maps_provider_http_status_to_temporary_failure() {
    let error = provider_execution_error(ProviderExecutionError::HttpStatus {
        status: 429,
        body: r#"{"error":{"message":"rate limited"}}"#.to_string(),
    });
    assert_eq!(error.code, types::ExtensionErrorCode::TemporaryFailure);
    assert!(error.message.value.contains("HTTP 429"));
    assert!(error.message.value.contains("rate limited"));
}

#[test]
fn command_missing_provider_config_returns_clear_error_not_room_reply() {
    let command = command_invocation("summarize");
    let executor = success_executor("unused");
    let error = command_response_with_config(
        command,
        &executor,
        Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "ai-chatbot provider configuration is invalid: expected JSON config with endpoint, model, and api_key",
        )),
    )
    .expect_err("missing provider config fails command");
    assert_eq!(error.code, types::ExtensionErrorCode::InvalidRequest);
    assert!(error.message.value.contains("provider configuration"));
}

#[test]
fn command_uses_prompt_field_and_reports_provider_transport_errors() {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value"}"#,
    )
    .map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("config error: {error}"),
        )
    });
    let command = command_invocation("summarize");
    let executor = FakeExecutor {
        answer: Err(ProviderExecutionError::Http(
            "provider transport failed".to_string(),
        )),
    };
    let error = command_response_with_config(command, &executor, config)
        .expect_err("provider transport error fails command");
    assert_eq!(error.code, types::ExtensionErrorCode::TemporaryFailure);
    assert!(error.message.value.contains("provider transport failed"));
}

#[test]
fn command_initial_execute_returns_prompt_form() {
    let mut command = command_invocation("summarize");
    command.fields.clear();
    let executor = success_executor("answer");
    let effect = command_response_with_config(command, &executor, test_config())
        .expect("initial command execute succeeds")
        .expect("prompt form effect");
    let types::ExtensionEffect::CommandForm(form) = effect else {
        panic!("expected command prompt form");
    };
    assert_eq!(form.form_type, types::DataFormType::Form);
    assert_eq!(form.fields[0].name.value, "prompt");
    assert!(form.fields[0].required);
}

#[test]
fn command_success_returns_visible_result_enrichment() {
    let command = command_invocation("summarize");
    let executor = success_executor("answer");
    let effect = command_response_with_config(command, &executor, test_config())
        .expect("command succeeds")
        .expect("visible command result");
    let types::ExtensionEffect::EnrichMessage(envelope) = effect else {
        panic!("expected command result enrichment");
    };
    let block = &envelope.enrichments[0].ui[0].blocks[0];
    let types::UiBlock::Text(text) = block else {
        panic!("expected text block");
    };
    assert_eq!(text.text.value, "answer");
}

fn success_executor(answer: &str) -> FakeExecutor {
    FakeExecutor {
        answer: Ok(ProviderAnswer {
            text: NonEmptyString::new(answer).expect("answer"),
        }),
    }
}

fn test_config() -> Result<ProviderConfig, types::ExtensionError> {
    ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value"}"#,
    )
    .map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("config error: {error}"),
        )
    })
}

fn provider_request_for_loop_test() -> ProviderRequest {
    let config = ProviderConfig::parse(
        r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test","api_key":"secret-value"}"#,
    )
    .expect("provider config");
    let context = execution_context("summarize this thread");
    let tools = select_host_tools(&context, config.context_limit);
    assemble_provider_request(&config, &context, tools)
}

fn archived_message(
    stanza_id: &str,
    thread_id: Option<&str>,
    reply_to: Option<&str>,
    body: &str,
) -> types::ArchivedMessage {
    types::ArchivedMessage {
        stanza_id: types::StanzaId {
            value: stanza_id.to_string(),
        },
        from_jid: types::Jid {
            value: "alice@example.com".to_string(),
        },
        to_jid: types::Jid {
            value: "chat@muc.example.com".to_string(),
        },
        sent_at: types::Timestamp {
            value: "2026-05-02T12:00:00Z".to_string(),
        },
        body: Some(types::DisplayText {
            value: body.to_string(),
        }),
        thread_id: thread_id.map(|value| types::ThreadId {
            value: value.to_string(),
        }),
        reply_to: reply_to.map(|value| types::ReplyTarget {
            id: types::StanzaId {
                value: value.to_string(),
            },
            to: None,
        }),
    }
}

fn execution_context(prompt: &str) -> ExecutionContext {
    ExecutionContext {
        requester: Some(types::BareJid {
            value: "alice@example.com".to_string(),
        }),
        prompt: CleanPrompt::new(prompt.to_string()).expect("prompt"),
        response_target: Some(ResponseTarget {
            room: types::RoomJid {
                value: "chat@muc.example.com".to_string(),
            },
            thread_id: None,
            reply_to: None,
            focus_thread: false,
        }),
    }
}

fn command_execution_context(prompt: &str) -> ExecutionContext {
    ExecutionContext {
        requester: Some(types::BareJid {
            value: "alice@example.com".to_string(),
        }),
        prompt: CleanPrompt::new(prompt.to_string()).expect("prompt"),
        response_target: None,
    }
}

fn command_invocation(prompt: &str) -> types::CommandInvocation {
    types::CommandInvocation {
        waddle_id: types::WaddleId {
            value: "space".to_string(),
        },
        room: None,
        requester: types::FullJid {
            value: "alice@example.com/work".to_string(),
        },
        command_node: types::CommandNode {
            value: COMMAND_NODE.to_string(),
        },
        session_id: None,
        action: Some(types::CommandAction::Execute),
        form: None,
        fields: vec![types::FormFieldValue {
            name: types::UiActionId {
                value: "prompt".to_string(),
            },
            values: vec![types::DataFormValue {
                value: prompt.to_string(),
            }],
        }],
    }
}
