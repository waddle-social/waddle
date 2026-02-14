use std::sync::Arc;

use chrono::Utc;
use tracing::debug;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::muc::user::{MucUser, Status};
use xmpp_parsers::presence::Type as PresenceType;

use waddle_core::event::{
    Channel, ChatMessage, Event, EventPayload, EventSource, MessageType as CoreMessageType,
    MucAffiliation as CoreAffiliation, MucOccupant as CoreOccupant, MucRole as CoreRole,
};

// Re-use the embed parser from the message processor
use super::message::parse_embeds_from_payloads;

#[cfg(feature = "native")]
use waddle_core::event::EventBus;

use crate::pipeline::{ProcessorContext, ProcessorResult, StanzaProcessor};
use crate::stanza::Stanza;

pub struct MucProcessor {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl MucProcessor {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>) -> Self {
        Self { event_bus }
    }
}

impl StanzaProcessor for MucProcessor {
    fn name(&self) -> &str {
        "muc"
    }

    fn process_inbound(&self, stanza: &mut Stanza, _ctx: &ProcessorContext) -> ProcessorResult {
        match stanza {
            Stanza::Message(msg) => {
                if msg.type_ != MessageType::Groupchat {
                    return ProcessorResult::Continue;
                }

                if let Some((_, subject)) = msg.get_best_subject(vec![]) {
                    let room = msg
                        .from
                        .as_ref()
                        .map(|j| j.to_bare().to_string())
                        .unwrap_or_default();
                    debug!(room = %room, "MUC subject changed");
                    #[cfg(feature = "native")]
                    {
                        let _ = self.event_bus.publish(Event::new(
                            Channel::new("xmpp.muc.subject.changed").unwrap(),
                            EventSource::Xmpp,
                            EventPayload::MucSubjectChanged {
                                room,
                                subject: subject.clone(),
                            },
                        ));
                    }
                    return ProcessorResult::Continue;
                }

                let body = match msg.get_best_body(vec![]) {
                    Some((_, body)) => body.clone(),
                    None => return ProcessorResult::Continue,
                };

                let room = msg
                    .from
                    .as_ref()
                    .map(|j| j.to_bare().to_string())
                    .unwrap_or_default();

                let embeds = parse_embeds_from_payloads(&msg.payloads);

                let chat_message = ChatMessage {
                    id: msg.id.as_ref().map(|id| id.0.clone()).unwrap_or_default(),
                    from: msg.from.as_ref().map(|j| j.to_string()).unwrap_or_default(),
                    to: msg.to.as_ref().map(|j| j.to_string()).unwrap_or_default(),
                    body,
                    timestamp: Utc::now(),
                    message_type: CoreMessageType::Groupchat,
                    thread: msg.thread.as_ref().map(|t| t.id.clone()),
                    embeds,
                };

                debug!(room = %room, "MUC message received");
                #[cfg(feature = "native")]
                {
                    let _ = self.event_bus.publish(Event::new(
                        Channel::new("xmpp.muc.message.received").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::MucMessageReceived {
                            room,
                            message: chat_message,
                        },
                    ));
                }
            }
            Stanza::Presence(presence) => {
                let muc_user = presence
                    .payloads
                    .iter()
                    .find_map(|el| MucUser::try_from(el.clone()).ok());

                let Some(muc_user) = muc_user else {
                    return ProcessorResult::Continue;
                };

                let room = presence
                    .from
                    .as_ref()
                    .map(|j| j.to_bare().to_string())
                    .unwrap_or_default();

                let nick = presence
                    .from
                    .as_ref()
                    .and_then(|j| j.resource().map(|r| r.to_string()))
                    .unwrap_or_default();

                let is_self = muc_user.status.contains(&Status::SelfPresence);

                if presence.type_ == PresenceType::Unavailable {
                    if is_self {
                        debug!(room = %room, "left MUC room");
                        #[cfg(feature = "native")]
                        {
                            let _ = self.event_bus.publish(Event::new(
                                Channel::new("xmpp.muc.left").unwrap(),
                                EventSource::Xmpp,
                                EventPayload::MucLeft { room },
                            ));
                        }
                    } else {
                        emit_occupant_changed(
                            &room,
                            &nick,
                            &muc_user,
                            #[cfg(feature = "native")]
                            &self.event_bus,
                        );
                    }
                } else {
                    if is_self {
                        debug!(room = %room, nick = %nick, "joined MUC room");
                        #[cfg(feature = "native")]
                        {
                            let _ = self.event_bus.publish(Event::new(
                                Channel::new("xmpp.muc.joined").unwrap(),
                                EventSource::Xmpp,
                                EventPayload::MucJoined {
                                    room: room.clone(),
                                    nick: nick.clone(),
                                },
                            ));
                        }
                    }
                    emit_occupant_changed(
                        &room,
                        &nick,
                        &muc_user,
                        #[cfg(feature = "native")]
                        &self.event_bus,
                    );
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

fn emit_occupant_changed(
    room: &str,
    nick: &str,
    muc_user: &MucUser,
    #[cfg(feature = "native")] event_bus: &Arc<dyn EventBus>,
) {
    if let Some(item) = muc_user.items.first() {
        let occupant = CoreOccupant {
            nick: nick.to_string(),
            jid: item.jid.as_ref().map(|j| j.to_string()),
            affiliation: match item.affiliation {
                xmpp_parsers::muc::user::Affiliation::Owner => CoreAffiliation::Owner,
                xmpp_parsers::muc::user::Affiliation::Admin => CoreAffiliation::Admin,
                xmpp_parsers::muc::user::Affiliation::Member => CoreAffiliation::Member,
                xmpp_parsers::muc::user::Affiliation::Outcast => CoreAffiliation::Outcast,
                xmpp_parsers::muc::user::Affiliation::None => CoreAffiliation::None,
            },
            role: match item.role {
                xmpp_parsers::muc::user::Role::Moderator => CoreRole::Moderator,
                xmpp_parsers::muc::user::Role::Participant => CoreRole::Participant,
                xmpp_parsers::muc::user::Role::Visitor => CoreRole::Visitor,
                xmpp_parsers::muc::user::Role::None => CoreRole::None,
            },
        };

        debug!(room = %room, nick = %nick, "MUC occupant changed");
        #[cfg(feature = "native")]
        {
            let _ = event_bus.publish(Event::new(
                Channel::new("xmpp.muc.occupant.changed").unwrap(),
                EventSource::Xmpp,
                EventPayload::MucOccupantChanged {
                    room: room.to_string(),
                    occupant,
                },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MUC_MESSAGE_XML: &[u8] = b"<message xmlns='jabber:client' type='groupchat' \
        from='room@conference.example.com/alice' to='bob@example.com' id='muc-1'>\
        <body>Hello everyone!</body>\
    </message>";

    const MUC_SUBJECT_XML: &[u8] = b"<message xmlns='jabber:client' type='groupchat' \
        from='room@conference.example.com/alice' to='bob@example.com'>\
        <subject>New topic</subject>\
    </message>";

    const MUC_PRESENCE_XML: &[u8] = b"<presence xmlns='jabber:client' \
        from='room@conference.example.com/bob'>\
        <x xmlns='http://jabber.org/protocol/muc#user'>\
            <item affiliation='member' role='participant'/>\
            <status code='110'/>\
        </x>\
    </presence>";

    #[test]
    fn parses_muc_message() {
        let stanza = Stanza::parse(MUC_MESSAGE_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        assert_eq!(msg.type_, MessageType::Groupchat);
    }

    #[test]
    fn parses_muc_subject() {
        let stanza = Stanza::parse(MUC_SUBJECT_XML).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message");
        };
        assert!(msg.get_best_subject(vec![]).is_some());
    }

    #[test]
    fn parses_muc_presence() {
        let stanza = Stanza::parse(MUC_PRESENCE_XML).unwrap();
        let Stanza::Presence(presence) = &stanza else {
            panic!("expected presence");
        };
        let muc_user = presence
            .payloads
            .iter()
            .find_map(|el| MucUser::try_from(el.clone()).ok());
        assert!(muc_user.is_some());
        let muc_user = muc_user.unwrap();
        assert!(muc_user.status.contains(&Status::SelfPresence));
    }
}
