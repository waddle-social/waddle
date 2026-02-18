use std::sync::Arc;

use tracing::debug;
use xmpp_parsers::chatstates::ChatState as XmppChatState;
use xmpp_parsers::message::MessageType;

use waddle_core::event::{Channel, ChatState as CoreChatState, Event, EventPayload, EventSource};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct ChatStateProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl ChatStateProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for ChatStateProcessor {
    fn name(&self) -> &str {
        "chat_state"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let Stanza::Message(msg) = stanza else {
            return ProcessorResult::Continue;
        };

        if msg.type_ == MessageType::Groupchat || msg.type_ == MessageType::Error {
            return ProcessorResult::Continue;
        }

        let chat_state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());

        let Some(state) = chat_state else {
            return ProcessorResult::Continue;
        };

        let from = msg.from.as_ref().map(|j| j.to_string()).unwrap_or_default();

        let core_state = match state {
            XmppChatState::Active => CoreChatState::Active,
            XmppChatState::Composing => CoreChatState::Composing,
            XmppChatState::Paused => CoreChatState::Paused,
            XmppChatState::Inactive => CoreChatState::Inactive,
            XmppChatState::Gone => CoreChatState::Gone,
        };

        debug!(from = %from, state = ?core_state, "chat state received");
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("xmpp.chatstate.received").unwrap(),
                EventSource::Xmpp,
                EventPayload::ChatStateReceived {
                    from,
                    state: core_state,
                },
            ));
        }

        ProcessorResult::Continue
    }

    fn process_outbound(&self, _stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        ProcessorResult::Continue
    }

    fn priority(&self) -> i32 {
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSING_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='alice@example.com' to='bob@example.com'>\
        <composing xmlns='http://jabber.org/protocol/chatstates'/>\
    </message>";

    const ACTIVE_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='alice@example.com' to='bob@example.com'>\
        <active xmlns='http://jabber.org/protocol/chatstates'/>\
    </message>";

    const PAUSED_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='alice@example.com' to='bob@example.com'>\
        <paused xmlns='http://jabber.org/protocol/chatstates'/>\
    </message>";

    #[test]
    fn parses_composing_state() {
        let stanza = Stanza::parse(COMPOSING_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Composing)));
    }

    #[test]
    fn parses_active_state() {
        let stanza = Stanza::parse(ACTIVE_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Active)));
    }

    #[test]
    fn parses_paused_state() {
        let stanza = Stanza::parse(PAUSED_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Paused)));
    }
}
