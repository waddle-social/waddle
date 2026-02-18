use std::sync::Arc;

use chrono::Utc;
use tracing::debug;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::receipts;

use waddle_core::event::{
    Channel, ChatMessage, Event, EventPayload, EventSource, MessageEmbed,
    MessageType as CoreMessageType,
};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct MessageProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl MessageProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for MessageProcessor {
    fn name(&self) -> &str {
        "message"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let Stanza::Message(msg) = stanza else {
            return ProcessorResult::Continue;
        };

        if msg.type_ == MessageType::Groupchat {
            return ProcessorResult::Continue;
        }

        if let Some(received) = try_extract_receipt(msg) {
            debug!(id = %received.id, "delivery receipt received");
            #[cfg(feature = "native")]
            {
                let to = msg
                    .from
                    .as_ref()
                    .map(|j| j.to_bare().to_string())
                    .unwrap_or_default();
                let _ = self.event_bus.publish(Event::new(
                    Channel::new("xmpp.message.delivered").unwrap(),
                    EventSource::Xmpp,
                    EventPayload::MessageDelivered {
                        id: received.id,
                        to,
                    },
                ));
            }
            return ProcessorResult::Continue;
        }

        let body = match msg.get_best_body(vec![]) {
            Some((_, body)) => body.clone(),
            None => return ProcessorResult::Continue,
        };

        // Parse plugin embeds from stanza payloads
        let embeds = parse_embeds_from_payloads(&msg.payloads);

        let chat_message = ChatMessage {
            id: msg.id.as_ref().map(|id| id.0.clone()).unwrap_or_default(),
            from: msg
                .from
                .as_ref()
                .map(|j| j.to_bare().to_string())
                .unwrap_or_default(),
            to: msg
                .to
                .as_ref()
                .map(|j| j.to_bare().to_string())
                .unwrap_or_default(),
            body,
            timestamp: Utc::now(),
            message_type: match msg.type_ {
                MessageType::Chat => CoreMessageType::Chat,
                MessageType::Normal => CoreMessageType::Normal,
                MessageType::Headline => CoreMessageType::Headline,
                MessageType::Error => CoreMessageType::Error,
                MessageType::Groupchat => CoreMessageType::Groupchat,
            },
            thread: msg.thread.as_ref().map(|t| t.id.clone()),
            embeds,
        };

        debug!(
            from = %chat_message.from,
            to = %chat_message.to,
            "message received"
        );

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("xmpp.message.received").unwrap(),
                EventSource::Xmpp,
                EventPayload::MessageReceived {
                    message: chat_message,
                },
            ));
        }

        ProcessorResult::Continue
    }

    fn process_outbound(&self, _stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        ProcessorResult::Continue
    }

    fn priority(&self) -> i32 {
        10
    }
}

/// Known embed namespace for GitHub metadata.
const NS_WADDLE_GITHUB: &str = "urn:waddle:github:0";

/// Parse structured embeds from unknown XMPP stanza payloads.
///
/// Currently recognises the `urn:waddle:github:0` namespace and converts
/// `<repo>`, `<issue>`, and `<pr>` elements into `MessageEmbed` values
/// that the TUI / GUI can render.
pub(crate) fn parse_embeds_from_payloads(payloads: &[xmpp_parsers::minidom::Element]) -> Vec<MessageEmbed> {
    let mut embeds = Vec::new();
    for payload in payloads {
        if payload.ns() != NS_WADDLE_GITHUB {
            continue;
        }
        let mut data = serde_json::Map::new();

        match payload.name() {
            "repo" => {
                data.insert("type".into(), "repo".into());
                if let Some(v) = payload.attr("owner") {
                    data.insert("owner".into(), v.into());
                }
                if let Some(v) = payload.attr("name") {
                    data.insert("name".into(), v.into());
                }
                if let Some(v) = payload.attr("url") {
                    data.insert("url".into(), v.into());
                }
                if let Some(el) = payload.get_child("description", NS_WADDLE_GITHUB) {
                    let text = el.text();
                    if !text.is_empty() {
                        data.insert("description".into(), text.into());
                    }
                }
                if let Some(n) = payload
                    .get_child("stars", NS_WADDLE_GITHUB)
                    .and_then(|el| el.text().parse::<u64>().ok())
                {
                    data.insert("stars".into(), n.into());
                }
                if let Some(n) = payload
                    .get_child("forks", NS_WADDLE_GITHUB)
                    .and_then(|el| el.text().parse::<u64>().ok())
                {
                    data.insert("forks".into(), n.into());
                }
                if let Some(el) = payload.get_child("license", NS_WADDLE_GITHUB) {
                    let text = el.text();
                    if !text.is_empty() {
                        data.insert("license".into(), text.into());
                    }
                }
                if let Some(el) = payload.get_child("default-branch", NS_WADDLE_GITHUB) {
                    let text = el.text();
                    if !text.is_empty() {
                        data.insert("defaultBranch".into(), text.into());
                    }
                }
                // Collect topics
                let topics: Vec<serde_json::Value> = payload
                    .children()
                    .filter(|c| c.name() == "topic" && c.ns() == NS_WADDLE_GITHUB)
                    .map(|c| c.text().into())
                    .collect();
                if !topics.is_empty() {
                    data.insert("topics".into(), topics.into());
                }
                // First language as primary
                if let Some(name) = payload
                    .children()
                    .find(|c| c.name() == "language" && c.ns() == NS_WADDLE_GITHUB)
                    .and_then(|el| el.attr("name"))
                {
                    data.insert("language".into(), name.into());
                }
            }
            "issue" => {
                data.insert("type".into(), "issue".into());
                if let Some(v) = payload.attr("repo") {
                    data.insert("repo".into(), v.into());
                }
                if let Some(v) = payload.attr("number") {
                    data.insert("number".into(), v.into());
                }
                if let Some(v) = payload.attr("state") {
                    data.insert("state".into(), v.into());
                }
                if let Some(v) = payload.attr("url") {
                    data.insert("url".into(), v.into());
                }
                if let Some(el) = payload.get_child("title", NS_WADDLE_GITHUB) {
                    data.insert("title".into(), el.text().into());
                }
                if let Some(el) = payload.get_child("author", NS_WADDLE_GITHUB) {
                    data.insert("author".into(), el.text().into());
                }
            }
            "pr" => {
                data.insert("type".into(), "pr".into());
                if let Some(v) = payload.attr("repo") {
                    data.insert("repo".into(), v.into());
                }
                if let Some(v) = payload.attr("number") {
                    data.insert("number".into(), v.into());
                }
                if let Some(v) = payload.attr("state") {
                    data.insert("state".into(), v.into());
                }
                if let Some(v) = payload.attr("url") {
                    data.insert("url".into(), v.into());
                }
                if let Some(el) = payload.get_child("title", NS_WADDLE_GITHUB) {
                    data.insert("title".into(), el.text().into());
                }
                if let Some(el) = payload.get_child("author", NS_WADDLE_GITHUB) {
                    data.insert("author".into(), el.text().into());
                }
                if let Some(v) = payload.attr("draft") {
                    data.insert("draft".into(), (v == "true").into());
                }
                if let Some(v) = payload.attr("merged") {
                    data.insert("merged".into(), (v == "true").into());
                }
            }
            _ => continue,
        }

        if !data.is_empty() {
            embeds.push(MessageEmbed {
                namespace: NS_WADDLE_GITHUB.to_string(),
                data: serde_json::Value::Object(data),
            });
        }
    }
    embeds
}

fn try_extract_receipt(msg: &xmpp_parsers::message::Message) -> Option<receipts::Received> {
    for payload in &msg.payloads {
        if let Ok(received) = receipts::Received::try_from(payload.clone()) {
            return Some(received);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT_MESSAGE_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='alice@example.com' to='bob@example.com' id='msg-1'>\
        <body>Hello, Bob!</body>\
    </message>";

    const RECEIPT_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='bob@example.com' to='alice@example.com'>\
        <received xmlns='urn:xmpp:receipts' id='msg-1'/>\
    </message>";

    const GROUPCHAT_XML: &[u8] = b"<message xmlns='jabber:client' type='groupchat' \
        from='room@conference.example.com/nick' to='alice@example.com' id='gc-1'>\
        <body>Hello room!</body>\
    </message>";

    #[test]
    fn parses_chat_message() {
        let stanza = Stanza::parse(CHAT_MESSAGE_XML).unwrap();
        assert!(matches!(stanza, Stanza::Message(_)));
    }

    #[test]
    fn parses_receipt() {
        let stanza = Stanza::parse(RECEIPT_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let receipt = try_extract_receipt(msg);
        assert!(receipt.is_some());
        assert_eq!(receipt.unwrap().id, "msg-1");
    }

    #[test]
    fn skips_groupchat() {
        let stanza = Stanza::parse(GROUPCHAT_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        assert_eq!(msg.type_, MessageType::Groupchat);
    }

    // ── Embed parsing tests ────────────────────────────────────────

    #[test]
    fn parses_github_repo_embed() {
        let xml: &[u8] = b"<message xmlns='jabber:client' type='chat' \
            from='alice@example.com' to='bob@example.com' id='msg-e1'>\
            <body>Check https://github.com/cuenv/cuenv</body>\
            <repo xmlns='urn:waddle:github:0' url='https://github.com/cuenv/cuenv' \
                  owner='cuenv' name='cuenv'>\
                <description>Cool project</description>\
                <stars>42</stars>\
                <language name='Rust' bytes='12345'/>\
                <license>MIT</license>\
            </repo>\
        </message>";
        let stanza = Stanza::parse(xml).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let embeds = parse_embeds_from_payloads(&msg.payloads);
        assert_eq!(embeds.len(), 1);
        assert_eq!(embeds[0].namespace, "urn:waddle:github:0");
        let data = &embeds[0].data;
        assert_eq!(data["type"], "repo");
        assert_eq!(data["owner"], "cuenv");
        assert_eq!(data["name"], "cuenv");
        assert_eq!(data["description"], "Cool project");
        assert_eq!(data["stars"], 42);
        assert_eq!(data["language"], "Rust");
        assert_eq!(data["license"], "MIT");
    }

    #[test]
    fn parses_github_issue_embed() {
        let xml: &[u8] = b"<message xmlns='jabber:client' type='chat' \
            from='alice@example.com' to='bob@example.com' id='msg-e2'>\
            <body>See issue</body>\
            <issue xmlns='urn:waddle:github:0' url='https://github.com/a/b/issues/1' \
                   repo='a/b' number='1' state='open'>\
                <title>Fix bug</title>\
                <author>alice</author>\
            </issue>\
        </message>";
        let stanza = Stanza::parse(xml).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let embeds = parse_embeds_from_payloads(&msg.payloads);
        assert_eq!(embeds.len(), 1);
        let data = &embeds[0].data;
        assert_eq!(data["type"], "issue");
        assert_eq!(data["repo"], "a/b");
        assert_eq!(data["number"], "1");
        assert_eq!(data["state"], "open");
        assert_eq!(data["title"], "Fix bug");
        assert_eq!(data["author"], "alice");
    }

    #[test]
    fn parses_github_pr_embed() {
        let xml: &[u8] = b"<message xmlns='jabber:client' type='chat' \
            from='alice@example.com' to='bob@example.com' id='msg-e3'>\
            <body>Review this</body>\
            <pr xmlns='urn:waddle:github:0' url='https://github.com/a/b/pull/99' \
                repo='a/b' number='99' state='open' draft='true' merged='false'>\
                <title>Add feature</title>\
                <author>bob</author>\
            </pr>\
        </message>";
        let stanza = Stanza::parse(xml).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let embeds = parse_embeds_from_payloads(&msg.payloads);
        assert_eq!(embeds.len(), 1);
        let data = &embeds[0].data;
        assert_eq!(data["type"], "pr");
        assert_eq!(data["number"], "99");
        assert_eq!(data["draft"], true);
        assert_eq!(data["merged"], false);
    }

    #[test]
    fn no_embeds_for_plain_message() {
        let stanza = Stanza::parse(CHAT_MESSAGE_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let embeds = parse_embeds_from_payloads(&msg.payloads);
        assert!(embeds.is_empty());
    }

    #[test]
    fn ignores_unknown_namespace_payloads() {
        let xml: &[u8] = b"<message xmlns='jabber:client' type='chat' \
            from='alice@example.com' to='bob@example.com' id='msg-e4'>\
            <body>Hi</body>\
            <x xmlns='urn:other:ns' foo='bar'/>\
        </message>";
        let stanza = Stanza::parse(xml).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let embeds = parse_embeds_from_payloads(&msg.payloads);
        assert!(embeds.is_empty());
    }
}
