use super::{
    assemble_provider_request, clean_prompt, command_response_with_config,
    execute_provider_request_with_runtime, extension_error, format_archived_messages, manifest,
    parse_provider_answer, provider_execution_error, provider_tool_mam_query, provider_tools_json,
    select_host_tools, take_sent_room_messages, types, CleanPrompt, CredentialPreference,
    ExecutionContext, HostTool, NonEmptyString, ProviderAnswer, ProviderConfig,
    ProviderConfigError, ProviderExecutionError, ProviderExecutor, ProviderRequest, ResponseTarget,
    BASELINE_SYSTEM_PROMPT, COMMAND_NODE, DEFAULT_DEPLOYMENT, DEFAULT_PROVIDER, MAX_CONTEXT_BYTES,
    MAX_CONTEXT_LINE_BYTES, MAX_PROVIDER_TOOL_CALLS_PER_ROUND,
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
fn manifest_registers_slash_ai_as_extension_command_without_raw_http_or_privacy_leaks() {
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
            types::ExtensionCapability::HostChannelsRead,
            types::ExtensionCapability::HostSpacesRead,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::Commands,
        ]
    );
}

#[test]
fn parses_brokered_provider_configuration_and_rejects_legacy_plaintext() {
    let config = ProviderConfig::parse(
        r#"{"provider":"openrouter","deployment":"default","model":"waddle-test","system_prompt":"Use XMPP context.","context_limit":8}"#,
    )
    .expect("provider config");
    assert_eq!(config.provider.as_str(), DEFAULT_PROVIDER);
    assert_eq!(config.deployment.as_str(), DEFAULT_DEPLOYMENT);
    assert_eq!(config.model.as_str(), "waddle-test");
    assert_eq!(config.credential, CredentialPreference::InvokingUser);
    assert_eq!(config.system_prompt.unwrap().as_str(), "Use XMPP context.");
    assert_eq!(config.context_limit, 8);

    let tenant_config =
        ProviderConfig::parse(r#"{"model":"waddle-test","credential_name":"tenant-openrouter"}"#)
            .expect("tenant credential config");
    assert_eq!(
        tenant_config.credential,
        CredentialPreference::Tenant(
            NonEmptyString::try_new("tenant-openrouter").expect("credential name")
        )
    );

    assert_eq!(
        ProviderConfig::parse(r#"{"model":"waddle-test","api_key":"secret-value"}"#),
        Err(ProviderConfigError::LegacyPlaintextConfigForbidden)
    );
    assert_eq!(
        ProviderConfig::parse(
            r#"{"endpoint":"https://api.example.test/v1/chat/completions","model":"waddle-test"}"#
        ),
        Err(ProviderConfigError::LegacyPlaintextConfigForbidden)
    );
}

#[test]
fn selects_only_room_scoped_and_discovery_tools() {
    let context = execution_context("summarize this channel for the space");
    let tools = select_host_tools(&context, 5);
    let kinds: Vec<_> = tools.iter().map(|request| request.tool).collect();
    assert_eq!(kinds, vec![HostTool::QueryMam, HostTool::Members]);

    let command_context = command_execution_context("list spaces");
    let tools = select_host_tools(&command_context, 5);
    let kinds: Vec<_> = tools.iter().map(|request| request.tool).collect();
    assert_eq!(kinds, vec![HostTool::Channels, HostTool::Spaces]);

    let no_context_tools = select_host_tools(&context, 0);
    assert!(!no_context_tools
        .iter()
        .any(|request| request.tool == HostTool::QueryMam));
}

#[test]
fn assembles_brokered_provider_request_and_room_only_mam_schema() {
    let config = ProviderConfig::parse(r#"{"model":"waddle-test","system_prompt":"Be concise."}"#)
        .expect("provider config");
    let context = execution_context("summarize this thread");
    let tools = select_host_tools(&context, config.context_limit);
    let request = assemble_provider_request(&config, &context, tools);
    assert_eq!(request.provider.as_str(), "openrouter");
    assert_eq!(request.deployment.as_str(), "default");
    assert_eq!(request.model.as_str(), "waddle-test");
    assert_eq!(request.messages.len(), 3);
    assert_eq!(request.messages[0].content.as_str(), BASELINE_SYSTEM_PROMPT);
    assert_eq!(request.messages[1].content.as_str(), "Be concise.");
    assert_eq!(
        request.messages[2].content.as_str(),
        "summarize this thread"
    );
    assert!(request.messages.iter().all(|message| {
        !message
            .content
            .as_str()
            .contains("Untrusted Waddle context")
            && !message.content.as_str().contains("waddle context:")
    }));

    let tool_schemas = provider_tools_json(&request.tools);
    let mam_schema = serde_json::to_string(&tool_schemas).expect("tool json");
    assert!(mam_schema.contains("\"query_mam\""));
    assert!(!mam_schema.contains("\"conversation\""));
    assert!(!mam_schema.contains("presence"));
    assert!(!mam_schema.contains("roster"));
}

#[test]
fn provider_loop_handles_streamed_text_and_tool_calls() {
    let request = provider_request_for_loop_test();
    let mut rounds = 0usize;
    let answer = execute_provider_request_with_runtime(
        &request,
        |_messages| {
            rounds += 1;
            Ok("streamed assistant summary".to_string())
        },
        |tool_call, req| {
            panic!("unexpected tool call {tool_call:?} for {req:?}");
        },
    )
    .expect("provider answer");
    assert_eq!(rounds, 1);
    assert_eq!(answer.text.as_str(), "streamed assistant summary");
}

#[test]
fn provider_loop_rejects_unavailable_tools() {
    let request = provider_request_for_loop_test();
    let mut responses = vec![
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"list_spaces","arguments":"{}"}}]}}]}"#
            .to_string(),
        "done".to_string(),
    ]
    .into_iter();
    let mut seen_tool_error = false;

    let answer = execute_provider_request_with_runtime(
        &request,
        |messages| {
            if messages
                .iter()
                .any(|m| m.role == "tool" && m.content.contains("was not available"))
            {
                seen_tool_error = true;
            }
            Ok(responses.next().expect("provider response"))
        },
        |tool_call, req| {
            panic!("unavailable tool executed: {tool_call:?} for request {req:?}");
        },
    )
    .expect("provider answer");

    assert_eq!(answer.text.as_str(), "done");
    assert!(seen_tool_error);
}

#[test]
fn provider_loop_caps_tool_calls_per_round_and_aggregate_bytes() {
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
        "done".to_string(),
    ]
    .into_iter();
    let large_tool_result = "a".repeat(MAX_CONTEXT_BYTES / 2 + 64);
    let mut executed = 0usize;
    let mut tool_outputs = Vec::new();

    execute_provider_request_with_runtime(
        &request,
        |messages| {
            tool_outputs = messages
                .iter()
                .filter(|m| m.role == "tool")
                .map(|m| m.content.clone())
                .collect();
            Ok(responses.next().expect("provider response"))
        },
        |_tool_call, _req| {
            executed += 1;
            Ok(large_tool_result.clone())
        },
    )
    .expect("provider answer");

    assert_eq!(executed, MAX_PROVIDER_TOOL_CALLS_PER_ROUND);
    assert_eq!(tool_outputs.len(), MAX_PROVIDER_TOOL_CALLS_PER_ROUND + 2);
    assert!(tool_outputs
        .iter()
        .any(|out| out.contains("budget exceeded")));
    assert!(tool_outputs
        .iter()
        .any(|out| out.contains("limit exceeded")));
}

#[test]
fn slash_ai_search_stays_provider_prompt_with_message_search_tool() {
    let config = ProviderConfig::parse(r#"{"model":"waddle-test"}"#).expect("provider config");
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
fn provider_message_tools_enforce_source_room_and_reject_conversation() {
    let mut target = execution_context("find the deploy note")
        .response_target
        .expect("room target");
    target.focus_thread = true;
    target.thread_id = Some(types::ThreadId {
        value: "thread-root".to_string(),
    });

    let request = ProviderRequest {
        provider: NonEmptyString::try_new("openrouter").expect("provider"),
        deployment: NonEmptyString::try_new("default").expect("deployment"),
        model: NonEmptyString::try_new("waddle-test").expect("model"),
        credential: CredentialPreference::InvokingUser,
        context_limit: 7,
        messages: vec![],
        tools: vec![],
        tool_target: Some(target.clone()),
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
        "query_mam does not permit direct conversation archive queries"
    );

    let mut roomless = request.clone();
    roomless.tool_target = None;
    assert!(provider_tool_mam_query(&roomless, &serde_json::json!({})).is_err());

    let mut disabled_request = request;
    disabled_request.context_limit = 0;
    assert_eq!(
        provider_tool_mam_query(&disabled_request, &serde_json::json!({}))
            .expect_err("context disabled"),
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
fn parses_openai_compatible_provider_answer() {
    let answer =
        parse_provider_answer(r#"{"choices":[{"message":{"content":"extension-owned answer"}}]}"#)
            .expect("provider answer");
    assert_eq!(answer.text.as_str(), "extension-owned answer");
}

#[test]
fn maps_provider_denial_and_failure_without_echoing_raw_bodies() {
    let denied =
        provider_execution_error(ProviderExecutionError::Denied("policy denied".to_string()));
    assert_eq!(denied.code, types::ExtensionErrorCode::Denied);

    let failure = provider_execution_error(ProviderExecutionError::BrokerFailure(
        "brokered inference temporarily unavailable".to_string(),
    ));
    assert_eq!(failure.code, types::ExtensionErrorCode::TemporaryFailure);
    assert!(!failure.message.value.contains("Bearer"));
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
            "ai-chatbot provider configuration is invalid: expected JSON config",
        )),
    )
    .expect_err("missing provider config fails command");
    assert_eq!(error.code, types::ExtensionErrorCode::InvalidRequest);
    assert!(error.message.value.contains("provider configuration"));
}

#[test]
fn command_initial_execute_returns_prompt_form_defaulting_to_channel() {
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
    assert_eq!(form.fields[1].name.value, "output");
    assert_eq!(form.fields[1].values[0].value, "channel");
}

#[test]
fn command_rejects_private_output_mode_inside_room() {
    let mut command = command_invocation("summarize release blockers");
    command.room = Some(types::RoomJid {
        value: "chat@muc.example.com".to_string(),
    });
    command.fields.push(types::FormFieldValue {
        name: types::UiActionId {
            value: "output".to_string(),
        },
        values: vec![types::DataFormValue {
            value: "private".to_string(),
        }],
    });
    let executor = success_executor("unused");
    let error = command_response_with_config(command, &executor, test_config())
        .expect_err("private output mode inside room must be denied");
    assert_eq!(error.code, types::ExtensionErrorCode::Denied);
}

#[test]
fn channel_public_command_posts_answer_to_active_room() {
    let _ = take_sent_room_messages();
    let mut command = command_invocation("summarize release blockers");
    command.room = Some(types::RoomJid {
        value: "chat@muc.example.com".to_string(),
    });
    command.fields.push(types::FormFieldValue {
        name: types::UiActionId {
            value: "output".to_string(),
        },
        values: vec![types::DataFormValue {
            value: "channel".to_string(),
        }],
    });
    let executor = success_executor("there are no blockers");

    let effect = command_response_with_config(command, &executor, test_config())
        .expect("command succeeds")
        .expect("visible command result");

    let types::ExtensionEffect::EnrichMessage(envelope) = effect else {
        panic!("expected visible posted confirmation");
    };
    let block = &envelope.enrichments[0].ui[0].blocks[0];
    let types::UiBlock::Text(text) = block else {
        panic!("expected text block");
    };
    assert_eq!(text.text.value, "AI answer posted to channel.");
    let sent = take_sent_room_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body.value, "there are no blockers");
}

#[test]
fn channel_public_command_requires_active_room() {
    let mut command = command_invocation("summarize release blockers");
    command.fields.push(types::FormFieldValue {
        name: types::UiActionId {
            value: "output".to_string(),
        },
        values: vec![types::DataFormValue {
            value: "channel".to_string(),
        }],
    });
    let executor = success_executor("unused");

    let error = command_response_with_config(command, &executor, test_config())
        .expect_err("channel output without room fails");

    assert_eq!(error.code, types::ExtensionErrorCode::InvalidRequest);
    assert_eq!(
        error.message.value,
        "posting an AI answer to a channel requires an active channel"
    );
}

fn success_executor(answer: &str) -> FakeExecutor {
    FakeExecutor {
        answer: Ok(ProviderAnswer {
            text: NonEmptyString::try_new(answer).expect("answer"),
        }),
    }
}

fn test_config() -> Result<ProviderConfig, types::ExtensionError> {
    ProviderConfig::parse(r#"{"model":"waddle-test"}"#).map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("config error: {error}"),
        )
    })
}

fn provider_request_for_loop_test() -> ProviderRequest {
    let config = ProviderConfig::parse(r#"{"model":"waddle-test"}"#).expect("provider config");
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
        prompt: CleanPrompt::try_new(prompt.to_string()).expect("prompt"),
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
        prompt: CleanPrompt::try_new(prompt.to_string()).expect("prompt"),
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
