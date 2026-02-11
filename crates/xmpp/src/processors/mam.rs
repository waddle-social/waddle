use std::sync::Arc;

use chrono::Utc;
use tracing::debug;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::mam;

use waddle_core::event::{
    Channel, ChatMessage, Event, EventPayload, EventSource, MessageType as CoreMessageType,
};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct MamProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl MamProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for MamProcessor {
    fn name(&self) -> &str {
        "mam"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        match stanza {
            Stanza::Message(msg) => {
                let mam_result = msg
                    .payloads
                    .iter()
                    .find_map(|el| mam::Result_::try_from(el.clone()).ok());

                let Some(result) = mam_result else {
                    return ProcessorResult::Continue;
                };

                let forwarded_msg = &result.forwarded.message;
                let timestamp = result
                    .forwarded
                    .delay
                    .as_ref()
                    .map(|d| d.stamp.0.to_utc())
                    .unwrap_or_else(Utc::now);

                let body = forwarded_msg
                    .get_best_body(vec![])
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();

                let chat_message = ChatMessage {
                    id: result.id.clone(),
                    from: forwarded_msg
                        .from
                        .as_ref()
                        .map(|j| j.to_string())
                        .unwrap_or_default(),
                    to: forwarded_msg
                        .to
                        .as_ref()
                        .map(|j| j.to_string())
                        .unwrap_or_default(),
                    body,
                    timestamp,
                    message_type: match forwarded_msg.type_ {
                        xmpp_parsers::message::MessageType::Chat => CoreMessageType::Chat,
                        xmpp_parsers::message::MessageType::Groupchat => CoreMessageType::Groupchat,
                        xmpp_parsers::message::MessageType::Normal => CoreMessageType::Normal,
                        xmpp_parsers::message::MessageType::Headline => CoreMessageType::Headline,
                        xmpp_parsers::message::MessageType::Error => CoreMessageType::Error,
                    },
                    thread: forwarded_msg.thread.as_ref().map(|t| t.id.clone()),
                };

                let query_id = result
                    .queryid
                    .as_ref()
                    .map(|q| q.0.clone())
                    .unwrap_or_default();
                debug!(query_id = %query_id, id = %result.id, "MAM result received");

                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.mam.result.received").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::MamResultReceived {
                            query_id,
                            messages: vec![chat_message],
                            complete: false,
                        },
                    ));
                }
            }
            Stanza::Iq(iq) => {
                if let Iq::Result {
                    id,
                    payload: Some(payload),
                    ..
                } = iq.as_ref()
                {
                    if let Ok(fin) = mam::Fin::try_from(payload.clone()) {
                        let last_id = fin.set.last.clone();
                        debug!(
                            complete = fin.complete,
                            last_id = ?last_id,
                            "MAM query finished"
                        );
                        #[cfg(feature = "native")]
                        {
                            let _ = self.event_bus.publish(Event::new(
                                Channel::new("xmpp.mam.fin.received").unwrap(),
                                EventSource::Xmpp,
                                EventPayload::MamFinReceived {
                                    iq_id: id.clone(),
                                    complete: fin.complete,
                                    last_id,
                                },
                            ));
                        }
                    }
                }
            }
            _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    const MAM_RESULT_XML: &[u8] = b"<message xmlns='jabber:client' to='alice@example.com'>\
        <result xmlns='urn:xmpp:mam:2' queryid='q1' id='archive-id-1'>\
            <forwarded xmlns='urn:xmpp:forward:0'>\
                <delay xmlns='urn:xmpp:delay' stamp='2023-06-15T12:00:00Z'/>\
                <message xmlns='jabber:client' type='chat' \
                    from='bob@example.com' to='alice@example.com' id='orig-1'>\
                    <body>Hello from the past</body>\
                </message>\
            </forwarded>\
        </result>\
    </message>";

    #[test]
    fn parses_mam_result() {
        let stanza = Stanza::parse(MAM_RESULT_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        let result = msg
            .payloads
            .iter()
            .find_map(|el| mam::Result_::try_from(el.clone()).ok());
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.id, "archive-id-1");
        assert_eq!(result.queryid.as_ref().map(|q| q.0.as_str()), Some("q1"));
    }
}
