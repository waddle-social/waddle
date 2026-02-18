use std::sync::Arc;

use tracing::{debug, warn};
use xmpp_parsers::{iq::Iq, ns, roster::Roster};

use waddle_core::event::{
    Channel, Event, EventPayload, EventSource, RosterItem, Subscription as CoreSubscription,
};

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct RosterProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl RosterProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for RosterProcessor {
    fn name(&self) -> &str {
        "roster"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        let Stanza::Iq(iq) = stanza else {
            return ProcessorResult::Continue;
        };

        match iq.as_ref() {
            Iq::Result {
                payload: Some(payload),
                ..
            } => {
                if !payload.is("query", ns::ROSTER) {
                    return ProcessorResult::Continue;
                }
                let Ok(roster) = Roster::try_from(payload.clone()) else {
                    warn!("failed to parse roster result payload");
                    return ProcessorResult::Continue;
                };
                let items: Vec<RosterItem> = roster.items.iter().map(convert_roster_item).collect();
                debug!(count = items.len(), "roster result received");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.roster.received").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::RosterReceived { items },
                    ));
                }
            }
            Iq::Set { payload, .. } => {
                if !payload.is("query", ns::ROSTER) {
                    return ProcessorResult::Continue;
                }
                let Ok(roster) = Roster::try_from(payload.clone()) else {
                    warn!("failed to parse roster push payload");
                    return ProcessorResult::Continue;
                };
                for item in &roster.items {
                    let core_item = convert_roster_item(item);
                    if matches!(
                        item.subscription,
                        xmpp_parsers::roster::Subscription::Remove
                    ) {
                        debug!(jid = %item.jid, "roster item removed");
                        #[cfg(feature = "native")]
                        {
                            let _ = self.event_bus.publish(Event::new(
                                Channel::new("xmpp.roster.removed").unwrap(),
                                EventSource::Xmpp,
                                EventPayload::RosterRemoved {
                                    jid: item.jid.to_string(),
                                },
                            ));
                        }
                    } else {
                        debug!(jid = %item.jid, "roster item updated");
                        #[cfg(feature = "native")]
                        {
                            let _ = self.event_bus.publish(Event::new(
                                Channel::new("xmpp.roster.updated").unwrap(),
                                EventSource::Xmpp,
                                EventPayload::RosterUpdated { item: core_item },
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

fn convert_roster_item(item: &xmpp_parsers::roster::Item) -> RosterItem {
    RosterItem {
        jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: match item.subscription {
            xmpp_parsers::roster::Subscription::None => CoreSubscription::None,
            xmpp_parsers::roster::Subscription::From => CoreSubscription::From,
            xmpp_parsers::roster::Subscription::To => CoreSubscription::To,
            xmpp_parsers::roster::Subscription::Both => CoreSubscription::Both,
            xmpp_parsers::roster::Subscription::Remove => CoreSubscription::Remove,
        },
        groups: item.groups.iter().map(|g| g.0.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROSTER_RESULT_XML: &[u8] = b"<iq xmlns='jabber:client' type='result' id='roster-1'>\
        <query xmlns='jabber:iq:roster'>\
            <item jid='alice@example.com' name='Alice' subscription='both'>\
                <group>Friends</group>\
            </item>\
            <item jid='bob@example.com' subscription='to'/>\
        </query>\
    </iq>";

    const ROSTER_PUSH_XML: &[u8] = b"<iq xmlns='jabber:client' type='set' id='push-1'>\
        <query xmlns='jabber:iq:roster'>\
            <item jid='carol@example.com' name='Carol' subscription='from'/>\
        </query>\
    </iq>";

    const ROSTER_REMOVE_XML: &[u8] = b"<iq xmlns='jabber:client' type='set' id='push-2'>\
        <query xmlns='jabber:iq:roster'>\
            <item jid='dave@example.com' subscription='remove'/>\
        </query>\
    </iq>";

    #[test]
    fn converts_roster_item_correctly() {
        let item = xmpp_parsers::roster::Item {
            jid: "alice@example.com".parse().unwrap(),
            name: Some("Alice".into()),
            subscription: xmpp_parsers::roster::Subscription::Both,
            ask: xmpp_parsers::roster::Ask::None,
            groups: vec![xmpp_parsers::roster::Group("Friends".into())],
        };

        let core = convert_roster_item(&item);
        assert_eq!(core.jid, "alice@example.com");
        assert_eq!(core.name, Some("Alice".into()));
        assert!(matches!(core.subscription, CoreSubscription::Both));
        assert_eq!(core.groups, vec!["Friends"]);
    }

    #[test]
    fn roster_processor_parses_result() {
        let stanza = Stanza::parse(ROSTER_RESULT_XML).unwrap();
        assert!(matches!(stanza, Stanza::Iq(_)));
    }

    #[test]
    fn roster_processor_parses_push() {
        let stanza = Stanza::parse(ROSTER_PUSH_XML).unwrap();
        assert!(matches!(stanza, Stanza::Iq(_)));
    }

    #[test]
    fn roster_processor_parses_remove() {
        let stanza = Stanza::parse(ROSTER_REMOVE_XML).unwrap();
        assert!(matches!(stanza, Stanza::Iq(_)));
    }
}
