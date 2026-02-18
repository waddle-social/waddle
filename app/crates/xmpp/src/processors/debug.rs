use std::sync::Arc;

use tracing::debug;

use waddle_core::event::{Channel, Event, EventPayload, EventSource};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct DebugProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl DebugProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for DebugProcessor {
    fn name(&self) -> &str {
        "debug"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let xml = stanza_to_string(stanza);
        debug!(
            direction = "inbound",
            stanza_type = stanza.name(),
            "raw stanza"
        );
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("xmpp.debug.stanza.received").unwrap(),
                EventSource::Xmpp,
                EventPayload::RawStanzaReceived { stanza: xml },
            ));
        }
        ProcessorResult::Continue
    }

    fn process_outbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let xml = stanza_to_string(stanza);
        debug!(
            direction = "outbound",
            stanza_type = stanza.name(),
            "raw stanza"
        );
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("xmpp.debug.stanza.sent").unwrap(),
                EventSource::Xmpp,
                EventPayload::RawStanzaSent { stanza: xml },
            ));
        }
        ProcessorResult::Continue
    }

    fn priority(&self) -> i32 {
        100
    }
}

fn stanza_to_string(stanza: &Stanza) -> String {
    stanza
        .to_bytes()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|_| format!("<{} [serialization failed]/>", stanza.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE_XML: &[u8] = b"<message xmlns='jabber:client' type='chat' \
        from='alice@example.com' to='bob@example.com'>\
        <body>test</body>\
    </message>";

    #[test]
    fn stanza_to_string_works() {
        let stanza = Stanza::parse(MESSAGE_XML).unwrap();
        let xml = stanza_to_string(&stanza);
        assert!(xml.contains("message"));
        assert!(xml.contains("test"));
    }

    #[test]
    fn debug_processor_has_priority_100() {
        #[cfg(feature = "native")]
        {
            use waddle_core::event::BroadcastEventBus;
            let bus = Arc::new(BroadcastEventBus::default());
            let processor = DebugProcessor::new(bus);
            assert_eq!(processor.priority(), 100);
        }
    }
}
