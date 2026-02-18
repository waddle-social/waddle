use std::sync::Arc;

use tracing::debug;
use xmpp_parsers::muc::Muc;
use xmpp_parsers::presence::{Presence, Priority, Show, Type as PresenceType};

use waddle_core::event::{
    Channel, Event, EventPayload, EventSource, PresenceShow as CorePresenceShow,
};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct PresenceProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl PresenceProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for PresenceProcessor {
    fn name(&self) -> &str {
        "presence"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let Stanza::Presence(presence) = stanza else {
            return ProcessorResult::Continue;
        };

        if is_muc_presence(presence) {
            return ProcessorResult::Continue;
        }

        match presence.type_ {
            PresenceType::Subscribe => {
                let from = presence
                    .from
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_default();
                debug!(from = %from, "subscription request received");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.subscription.request").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::SubscriptionRequest { from },
                    ));
                }
            }
            PresenceType::Subscribed => {
                let jid = presence
                    .from
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_default();
                debug!(jid = %jid, "subscription approved");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.subscription.approved").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::SubscriptionApproved { jid },
                    ));
                }
            }
            PresenceType::Unsubscribe | PresenceType::Unsubscribed => {
                let jid = presence
                    .from
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_default();
                debug!(jid = %jid, "subscription revoked");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.subscription.revoked").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::SubscriptionRevoked { jid },
                    ));
                }
            }
            PresenceType::None | PresenceType::Unavailable => {
                let jid = presence
                    .from
                    .as_ref()
                    .map(|j| j.to_string())
                    .unwrap_or_default();
                let show = convert_show(presence);
                let status = presence.statuses.get("").cloned();
                let priority = extract_priority(&presence.priority);
                debug!(jid = %jid, ?show, priority, "presence changed");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.presence.changed").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::PresenceChanged {
                            jid,
                            show,
                            status,
                            priority,
                        },
                    ));
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

fn is_muc_presence(presence: &Presence) -> bool {
    presence
        .payloads
        .iter()
        .any(|el| Muc::try_from(el.clone()).is_ok())
        || presence
            .payloads
            .iter()
            .any(|el| xmpp_parsers::muc::user::MucUser::try_from(el.clone()).is_ok())
}

fn convert_show(presence: &Presence) -> CorePresenceShow {
    if presence.type_ == PresenceType::Unavailable {
        return CorePresenceShow::Unavailable;
    }
    match presence.show {
        None => CorePresenceShow::Available,
        Some(Show::Chat) => CorePresenceShow::Chat,
        Some(Show::Away) => CorePresenceShow::Away,
        Some(Show::Xa) => CorePresenceShow::Xa,
        Some(Show::Dnd) => CorePresenceShow::Dnd,
    }
}

fn extract_priority(priority: &Priority) -> i8 {
    let el: xmpp_parsers::minidom::Element = priority.into();
    el.text().parse::<i8>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVAILABLE_XML: &[u8] =
        b"<presence xmlns='jabber:client' from='alice@example.com/mobile'/>";

    const AWAY_XML: &[u8] = b"<presence xmlns='jabber:client' from='alice@example.com/mobile'>\
        <show>away</show>\
        <status>Be right back</status>\
    </presence>";

    const UNAVAILABLE_XML: &[u8] = b"<presence xmlns='jabber:client' \
        from='alice@example.com/mobile' type='unavailable'/>";

    const SUBSCRIBE_XML: &[u8] = b"<presence xmlns='jabber:client' \
        from='carol@example.com' type='subscribe'/>";

    #[test]
    fn converts_available_presence() {
        let stanza = Stanza::parse(AVAILABLE_XML).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence");
        };
        assert!(matches!(convert_show(p), CorePresenceShow::Available));
    }

    #[test]
    fn converts_away_presence() {
        let stanza = Stanza::parse(AWAY_XML).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence");
        };
        assert!(matches!(convert_show(p), CorePresenceShow::Away));
        assert_eq!(
            p.statuses.get("").map(String::as_str),
            Some("Be right back")
        );
    }

    #[test]
    fn converts_unavailable_presence() {
        let stanza = Stanza::parse(UNAVAILABLE_XML).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence");
        };
        assert!(matches!(convert_show(p), CorePresenceShow::Unavailable));
    }

    #[test]
    fn parses_subscribe_request() {
        let stanza = Stanza::parse(SUBSCRIBE_XML).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence");
        };
        assert_eq!(p.type_, PresenceType::Subscribe);
    }
}
