mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
        },
    });
}

use bindings::exports;
use bindings::waddle::extension::types;

struct AiChatbot;

bindings::export!(AiChatbot with_types_in bindings);

const PLUGIN_ID: &str = "ai-chatbot";
const PLUGIN_NAME: &str = "AI Chatbot";
const PLUGIN_NS: &str = "urn:waddle:ai-chatbot:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:ai-chatbot";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for AiChatbot {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for AiChatbot {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) => {
                message_hook_response(hook).into_iter().collect()
            }
            types::ExtensionEvent::Command(command) => {
                let prompt = command
                    .fields
                    .first()
                    .and_then(|field| field.values.first())
                    .map(|value| value.value.as_str())
                    .unwrap_or("Ask me from chat with /ask.");
                vec![visible_message(answer(prompt, command.waddle_id, None))]
            }
            types::ExtensionEvent::Launch(launch) => {
                let prompt = field_value(&launch.fields, "payload#assistant-followup#question")
                    .or_else(|| field_value(&launch.fields, "payload#assistant-followup"))
                    .or_else(|| field_value(&launch.fields, "payload#question"))
                    .unwrap_or("Continue the previous answer.");
                vec![visible_message(answer(
                    prompt,
                    launch.context.waddle_id,
                    launch.context.source_stanza_id,
                ))]
            }
        };
        Ok(types::ExtensionResponse { effects })
    }
}

fn message_hook_response(hook: types::MessageHook) -> Option<types::ExtensionEffect> {
    let body = hook.body.value;
    let explicit_mention = contains_waddle_mention(&body);
    let slash_trigger = starts_with_ai_command(&body);
    let types::MessageContext {
        room,
        sender,
        thread_id,
        stanza_id,
        reply_to,
        ..
    } = hook.context;
    let in_thread = thread_id.is_some();
    let is_reply = reply_to.is_some();
    if !explicit_mention && (!slash_trigger || in_thread) {
        return None;
    }
    if is_reply && !in_thread {
        return None;
    }

    let room = room?;
    let root_thread_id = thread_id.or_else(|| {
        stanza_id
            .clone()
            .map(|id| types::ThreadId { value: id.value })
    });
    let reply_to = stanza_id
        .map(|id| types::ReplyTarget { id, to: sender })
        .or(reply_to);
    Some(types::ExtensionEffect::BotGroupchatResponse(
        types::BotGroupchatResponse {
            body: display(&answer_body(&body)),
            room,
            thread_id: root_thread_id,
            reply_to,
        },
    ))
}

fn contains_waddle_mention(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower
        .match_indices("@waddle")
        .any(|(start, mention)| is_word_boundary(lower.as_bytes().get(start + mention.len())))
}

fn starts_with_ai_command(body: &str) -> bool {
    let trimmed = body.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("/ai") && is_command_boundary(lower.as_bytes().get(3))
}

fn is_command_boundary(next: Option<&u8>) -> bool {
    matches!(next, None | Some(b' ' | b'\t' | b'\r' | b'\n'))
}

fn is_word_boundary(next: Option<&u8>) -> bool {
    !matches!(next, Some(b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![
            payload_rule(types::PayloadSurface::MessageEnrichment, "assistant-answer"),
            payload_rule(types::PayloadSurface::LaunchPayload, "assistant-followup"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::MessageObserve,
            types::ExtensionCapability::BotGroupchatSend,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Ask AI Chatbot"),
            command_descriptor(INVOKE_NODE, "Run AI Chatbot action"),
        ],
        pubsub_nodes: vec![],
        artifact: None,
    }
}

struct VisibleMessage {
    ui: Vec<types::UiView>,
    payloads: Vec<types::ExtensionPayload>,
    launches: Vec<types::LaunchDescriptor>,
}

fn visible_message(message: VisibleMessage) -> types::ExtensionEffect {
    types::ExtensionEffect::EnrichMessage(types::ExtensionEnvelope {
        version: 1,
        enrichments: vec![types::MessageEnrichment {
            id: types::EnrichmentId {
                value: "assistant-message".to_string(),
            },
            plugin: plugin_id(),
            capability: types::ExtensionCapability::MessageEnrich,
            payload_namespace: payload_namespace(),
            created_at: types::Timestamp {
                value: "2026-04-27T00:00:00Z".to_string(),
            },
            source: None,
            ui: message.ui,
            payloads: message.payloads,
            launches: message.launches,
        }],
    })
}

fn answer(
    prompt: &str,
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> VisibleMessage {
    let answer = answer_body(prompt);
    VisibleMessage {
        ui: vec![types::UiView {
            id: types::UiViewId {
                value: "ai-answer".to_string(),
            },
            title: Some(display(PLUGIN_NAME)),
            blocks: vec![
                types::UiBlock::Text(types::TextBlock {
                    text: display(&answer),
                    style: types::TextStyle::Body,
                }),
                types::UiBlock::Text(types::TextBlock {
                    text: display(prompt),
                    style: types::TextStyle::Muted,
                }),
                types::UiBlock::Action(types::ActionBlock {
                    launch_id: launch_id("ask-followup"),
                    label: display("Ask follow-up"),
                }),
            ],
        }],
        payloads: vec![payload(
            "assistant-answer",
            vec![
                ("run-id", "run-ai-chatbot-1".to_string()),
                ("profile", "default".to_string()),
                ("context-source", "mam".to_string()),
            ],
            "Assistant response generated",
        )],
        launches: vec![types::LaunchDescriptor {
            id: launch_id("ask-followup"),
            plugin: plugin_id(),
            action: types::ActionId {
                value: "ask-followup".to_string(),
            },
            command_node: types::CommandNode {
                value: INVOKE_NODE.to_string(),
            },
            label: display("Ask follow-up"),
            context: types::LaunchContext {
                waddle_id,
                source_stanza_id,
            },
            payloads: vec![payload(
                "assistant-followup",
                vec![("question", prompt.to_string())],
                prompt,
            )],
            fallback: None,
            expires_at: None,
        }],
    }
}

fn answer_body(prompt: &str) -> String {
    let prompt = clean_prompt(prompt);
    if let Some(answer) = answer_letter_count(&prompt) {
        return answer;
    }
    if let Some(answer) = answer_arithmetic(&prompt) {
        return answer;
    }
    format!(
        "I received: {prompt}\n\nI can answer simple arithmetic and letter-count questions here. A real AI provider is not wired into this extension yet."
    )
}

fn clean_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    let without_command = if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("/ai") {
        &trimmed[3..]
    } else {
        trimmed
    };
    without_command
        .replace("@waddle", "")
        .replace("@Waddle", "")
        .trim()
        .to_string()
}

fn answer_letter_count(prompt: &str) -> Option<String> {
    let lower = prompt.to_ascii_lowercase();
    if !lower.contains("how many") || !lower.contains(" in ") {
        return None;
    }
    let needle = extract_quoted_letter(prompt).or_else(|| extract_letter_before_in(prompt))?;
    let word = prompt
        .rsplit_once(" in ")
        .map(|(_, word)| word)
        .unwrap_or(prompt)
        .trim_matches(|ch: char| !ch.is_alphanumeric());
    if word.is_empty() {
        return None;
    }
    let count = word
        .chars()
        .filter(|ch| ch.eq_ignore_ascii_case(&needle))
        .count();
    Some(format!(
        "\"{word}\" contains {count} '{}'{}.",
        needle,
        if count == 1 { "" } else { "s" }
    ))
}

fn extract_quoted_letter(prompt: &str) -> Option<char> {
    let mut chars = prompt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' || ch == '"' || ch == '`' {
            let letter = chars.next()?;
            let closing = chars.next()?;
            if closing == ch && letter.is_alphabetic() {
                return Some(letter);
            }
        }
    }
    None
}

fn extract_letter_before_in(prompt: &str) -> Option<char> {
    let before_in = prompt.rsplit_once(" in ")?.0;
    before_in.split_whitespace().rev().find_map(|token| {
        let token = token
            .trim_matches(|ch: char| !ch.is_alphabetic())
            .trim_end_matches("'s");
        (token.chars().count() == 1)
            .then(|| token.chars().next())
            .flatten()
    })
}

fn answer_arithmetic(prompt: &str) -> Option<String> {
    let expression = prompt.trim().trim_end_matches('?').trim();
    let expression = if expression.len() >= 8 && expression[..8].eq_ignore_ascii_case("what is ") {
        &expression[8..]
    } else {
        expression
    }
    .trim();
    let mut parts = expression.split_whitespace();
    let left = parts.next()?.parse::<i64>().ok()?;
    let op = parts.next()?;
    let right = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let value = match op {
        "+" => left.checked_add(right)?,
        "-" => left.checked_sub(right)?,
        "*" | "x" | "X" => left.checked_mul(right)?,
        "/" => {
            if right == 0 {
                return Some("Division by zero is undefined.".to_string());
            }
            if left % right == 0 {
                left / right
            } else {
                return Some(format!("{left} / {right} = {}", left as f64 / right as f64));
            }
        }
        "%" => {
            if right == 0 {
                return Some("Modulo by zero is undefined.".to_string());
            }
            left % right
        }
        _ => return None,
    };
    Some(format!("{left} {op} {right} = {value}."))
}

fn payload(root: &str, attrs: Vec<(&str, String)>, text: &str) -> types::ExtensionPayload {
    let namespace = payload_namespace();
    types::ExtensionPayload {
        namespace: namespace.clone(),
        root: types::PayloadRoot {
            namespace: namespace.clone(),
            local_name: root.to_string(),
        },
        tokens: vec![
            types::XmlToken::StartElement(types::XmlElement {
                namespace,
                local_name: root.to_string(),
                attributes: attrs
                    .into_iter()
                    .map(|(name, value)| types::XmlAttribute {
                        namespace: None,
                        local_name: name.to_string(),
                        value,
                    })
                    .collect(),
            }),
            types::XmlToken::Text(text.to_string()),
            types::XmlToken::EndElement,
        ],
    }
}

fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

fn command_descriptor(node: &str, name: &str) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
    }
}

fn field_value<'a>(fields: &'a [types::FormFieldValue], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.as_str())
}

fn launch_id(value: &str) -> types::LaunchId {
    types::LaunchId {
        value: value.to_string(),
    }
}

fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}
fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        answer_body, contains_waddle_mention, message_hook_response, starts_with_ai_command, types,
    };

    #[test]
    fn detects_ai_root_command_case_insensitively_with_boundary() {
        assert!(starts_with_ai_command("/ai summarize"));
        assert!(starts_with_ai_command("  /AI"));
        assert!(starts_with_ai_command("/Ai\tthread"));
        assert!(!starts_with_ai_command("prefix /ai"));
        assert!(!starts_with_ai_command("/airship"));
    }

    #[test]
    fn detects_waddle_mention_case_insensitively_with_boundary() {
        assert!(contains_waddle_mention("@waddle summarize"));
        assert!(contains_waddle_mention("can @Waddle help?"));
        assert!(contains_waddle_mention("@WADDLE"));
        assert!(!contains_waddle_mention("@waddled"));
        assert!(!contains_waddle_mention("@waddle_bot"));
    }

    #[test]
    fn ignores_root_feed_replies_even_when_they_mention_ai() {
        let hook = message_hook("/ai summarize this reply", None, Some("parent-msg"));
        assert!(message_hook_response(hook).is_none());
    }

    #[test]
    fn allows_threaded_followups_that_mention_waddle() {
        let hook = message_hook("@Waddle continue", Some("thread-root"), Some("parent-msg"));
        assert!(message_hook_response(hook).is_some());
    }

    #[test]
    fn answers_basic_arithmetic_without_canned_text() {
        assert_eq!(answer_body("/ai what is 10 % 5?"), "10 % 5 = 0.");
        assert_eq!(answer_body("/ai what is 5 * 5?"), "5 * 5 = 25.");
    }

    #[test]
    fn answers_letter_count_questions_without_canned_text() {
        assert_eq!(
            answer_body("/ai How many r's in Strawberry?"),
            "\"Strawberry\" contains 3 'r's."
        );
    }

    #[test]
    fn falls_back_honestly_when_no_local_answer_exists() {
        let answer = answer_body("/ai summarize the release notes");
        assert!(answer.contains("A real AI provider is not wired"));
        assert!(!answer.contains("I can help summarize the recent thread"));
    }

    fn message_hook(
        body: &str,
        thread_id: Option<&str>,
        reply_to: Option<&str>,
    ) -> types::MessageHook {
        types::MessageHook {
            context: types::MessageContext {
                waddle_id: types::WaddleId {
                    value: "space".to_string(),
                },
                stanza_id: Some(types::StanzaId {
                    value: "source-msg".to_string(),
                }),
                room: Some(types::RoomJid {
                    value: "chat@muc.example.com".to_string(),
                }),
                sender: Some(types::FullJid {
                    value: "alice@example.com/web".to_string(),
                }),
                thread_id: thread_id.map(|value| types::ThreadId {
                    value: value.to_string(),
                }),
                reply_to: reply_to.map(|id| types::ReplyTarget {
                    id: types::StanzaId {
                        value: id.to_string(),
                    },
                    to: None,
                }),
            },
            body: types::DisplayText {
                value: body.to_string(),
            },
            links: vec![],
        }
    }
}
