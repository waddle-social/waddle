use std::sync::Arc;

use chrono::Utc;
use tracing::debug;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::receipts;

use waddle_core::event::{
    Channel, ChatMessage, Event, EventPayload, EventSource, MessageType as CoreMessageType,
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
}
