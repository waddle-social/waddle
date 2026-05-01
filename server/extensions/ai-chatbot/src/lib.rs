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
            types::ExtensionEvent::Command(_) | types::ExtensionEvent::Launch(_) => {
                vec![types::ExtensionEffect::Noop]
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
    if !explicit_mention && !slash_trigger {
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
        payloads: vec![payload_rule(
            types::PayloadSurface::MessageEnrichment,
            "assistant-answer",
        )],
        capabilities: vec![
            types::ExtensionCapability::MessageObserve,
            types::ExtensionCapability::BotGroupchatSend,
        ],
        commands: vec![],
        pubsub_nodes: vec![],
        artifact: None,
    }
}

fn answer_body(prompt: &str) -> String {
    let prompt = clean_prompt(prompt);
    format!(
        "AI provider unavailable. Configure WADDLE_AI_PROVIDER=openai, OPENAI_API_KEY, and WADDLE_AI_MODEL on the server to answer: {prompt}"
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

fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
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
    fn reports_provider_unavailable_without_fake_local_answer() {
        let answer = answer_body("/ai summarize the release notes");
        assert!(answer.contains("AI provider unavailable"));
        assert!(answer.contains("WADDLE_AI_MODEL"));
        assert!(!answer.contains("simple arithmetic"));
        assert!(!answer.contains("letter-count"));
    }

    #[test]
    fn allows_threaded_followups_with_slash_ai() {
        let hook = message_hook("/ai continue", Some("thread-root"), Some("parent-msg"));
        assert!(message_hook_response(hook).is_some());
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
