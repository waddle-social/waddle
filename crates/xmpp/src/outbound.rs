use std::sync::Arc;

use tracing::{debug, error, warn};
use uuid::Uuid;
use xmpp_parsers::chatstates::ChatState as XmppChatState;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid;
use xmpp_parsers::message::{Lang, Message, MessageType as XmppMessageType};
use xmpp_parsers::muc::Muc;
use xmpp_parsers::presence::{Presence, Show, Type as PresenceType};
use xmpp_parsers::roster;

use waddle_core::event::{
    ChatMessage, ChatState as CoreChatState, Event, EventPayload, EventSource,
    MessageType as CoreMessageType, PresenceShow as CorePresenceShow,
};

#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus};

use crate::pipeline::StanzaPipeline;
use crate::stanza::Stanza;

pub struct OutboundRouter {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
    pipeline: Arc<StanzaPipeline>,
}

impl OutboundRouter {
    #[cfg(feature = "native")]
    pub fn new(event_bus: Arc<dyn EventBus>, pipeline: Arc<StanzaPipeline>) -> Self {
        Self {
            event_bus,
            pipeline,
        }
    }

    #[cfg(feature = "native")]
    pub async fn run(&self) -> Result<(), OutboundRouterError> {
        let mut subscription = self
            .event_bus
            .subscribe("ui.**")
            .map_err(|e| OutboundRouterError::SubscriptionFailed(e.to_string()))?;

        loop {
            match subscription.recv().await {
                Ok(event) => {
                    if let Err(e) = self.handle_event(&event).await {
                        warn!(
                            channel = %event.channel,
                            error = %e,
                            "failed to handle outbound event"
                        );
                    }
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    debug!("event bus closed, outbound router stopping");
                    return Ok(());
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "outbound router lagged, some events dropped");
                }
                Err(e) => {
                    error!(error = %e, "outbound router subscription error");
                    return Err(OutboundRouterError::SubscriptionFailed(e.to_string()));
                }
            }
        }
    }

    #[cfg(feature = "native")]
    async fn handle_event(&self, event: &Event) -> Result<(), OutboundRouterError> {
        let stanza = match &event.payload {
            EventPayload::MessageSendRequested {
                to,
                body,
                message_type,
            } => {
                let stanza = build_message_stanza(to, body, message_type)?;
                self.emit_message_sent(event, to, body, message_type);
                Some(stanza)
            }
            EventPayload::PresenceSetRequested { show, status } => {
                let stanza = build_presence_stanza(show, status.as_deref());
                self.emit_own_presence_changed(show, status.as_deref());
                Some(stanza)
            }
            EventPayload::RosterAddRequested { jid, name, groups } => {
                Some(build_roster_add_stanza(jid, name.as_deref(), groups)?)
            }
            EventPayload::RosterRemoveRequested { jid } => Some(build_roster_remove_stanza(jid)?),
            EventPayload::SubscriptionRespondRequested { jid, accept } => {
                Some(build_subscription_response_stanza(jid, *accept)?)
            }
            EventPayload::MucJoinRequested { room, nick } => {
                Some(build_muc_join_stanza(room, nick)?)
            }
            EventPayload::MucLeaveRequested { room } => Some(build_muc_leave_stanza(room)?),
            EventPayload::MucSendRequested { room, body } => {
                Some(build_muc_message_stanza(room, body)?)
            }
            EventPayload::ChatStateSendRequested { to, state } => {
                Some(build_chat_state_stanza(to, state)?)
            }
            _ => None,
        };

        if let Some(stanza) = stanza {
            let _bytes = self
                .pipeline
                .process_outbound(stanza)
                .await
                .map_err(|e| OutboundRouterError::PipelineFailed(e.to_string()))?;
        }

        Ok(())
    }

    #[cfg(feature = "native")]
    fn emit_message_sent(
        &self,
        event: &Event,
        to: &str,
        body: &str,
        message_type: &CoreMessageType,
    ) {
        let channel = match Channel::new("xmpp.message.sent") {
            Ok(c) => c,
            Err(_) => return,
        };

        let msg = ChatMessage {
            id: Uuid::new_v4().to_string(),
            from: String::new(),
            to: to.to_string(),
            body: body.to_string(),
            timestamp: chrono::Utc::now(),
            message_type: message_type.clone(),
            thread: None,
        };

        let sent_event = if let Some(corr) = event.correlation_id {
            Event::with_correlation(
                channel,
                EventSource::Xmpp,
                EventPayload::MessageSent { message: msg },
                corr,
            )
        } else {
            Event::new(
                channel,
                EventSource::Xmpp,
                EventPayload::MessageSent { message: msg },
            )
        };

        let _ = self.event_bus.publish(sent_event);
    }

    #[cfg(feature = "native")]
    fn emit_own_presence_changed(&self, show: &CorePresenceShow, status: Option<&str>) {
        let channel = match Channel::new("xmpp.presence.own_changed") {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = self.event_bus.publish(Event::new(
            channel,
            EventSource::Xmpp,
            EventPayload::OwnPresenceChanged {
                show: show.clone(),
                status: status.map(String::from),
            },
        ));
    }
}

fn build_message_stanza(
    to: &str,
    body: &str,
    message_type: &CoreMessageType,
) -> Result<Stanza, OutboundRouterError> {
    let to_jid: jid::Jid = to
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(to.to_string()))?;

    let xmpp_type = match message_type {
        CoreMessageType::Chat => XmppMessageType::Chat,
        CoreMessageType::Normal => XmppMessageType::Normal,
        CoreMessageType::Headline => XmppMessageType::Headline,
        CoreMessageType::Groupchat => XmppMessageType::Groupchat,
        CoreMessageType::Error => XmppMessageType::Error,
    };

    let mut msg = Message::new_with_type(xmpp_type, Some(to_jid));
    msg.id = Some(xmpp_parsers::message::Id(Uuid::new_v4().to_string()));
    msg.bodies.insert(Lang::new(), body.to_string());

    Ok(Stanza::Message(Box::new(msg)))
}

fn build_presence_stanza(show: &CorePresenceShow, status: Option<&str>) -> Stanza {
    let mut presence = Presence::new(PresenceType::None);

    match show {
        CorePresenceShow::Unavailable => {
            presence.type_ = PresenceType::Unavailable;
        }
        CorePresenceShow::Available => {}
        CorePresenceShow::Chat => {
            presence.show = Some(Show::Chat);
        }
        CorePresenceShow::Away => {
            presence.show = Some(Show::Away);
        }
        CorePresenceShow::Xa => {
            presence.show = Some(Show::Xa);
        }
        CorePresenceShow::Dnd => {
            presence.show = Some(Show::Dnd);
        }
    }

    if let Some(text) = status {
        presence.statuses.insert(Lang::new(), text.to_string());
    }

    Stanza::Presence(Box::new(presence))
}

fn build_roster_add_stanza(
    jid_str: &str,
    name: Option<&str>,
    groups: &[String],
) -> Result<Stanza, OutboundRouterError> {
    let contact_jid: jid::BareJid = jid_str
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(jid_str.to_string()))?;

    let item = roster::Item {
        jid: contact_jid,
        name: name.map(String::from),
        subscription: roster::Subscription::None,
        ask: roster::Ask::None,
        groups: groups.iter().map(|g| roster::Group(g.clone())).collect(),
    };

    let query = roster::Roster {
        ver: None,
        items: vec![item],
    };

    let iq = Iq::from_set(Uuid::new_v4().to_string(), query);
    Ok(Stanza::Iq(Box::new(iq)))
}

fn build_roster_remove_stanza(jid_str: &str) -> Result<Stanza, OutboundRouterError> {
    let contact_jid: jid::BareJid = jid_str
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(jid_str.to_string()))?;

    let item = roster::Item {
        jid: contact_jid,
        name: None,
        subscription: roster::Subscription::Remove,
        ask: roster::Ask::None,
        groups: vec![],
    };

    let query = roster::Roster {
        ver: None,
        items: vec![item],
    };

    let iq = Iq::from_set(Uuid::new_v4().to_string(), query);
    Ok(Stanza::Iq(Box::new(iq)))
}

fn build_subscription_response_stanza(
    jid_str: &str,
    accept: bool,
) -> Result<Stanza, OutboundRouterError> {
    let to_jid: jid::Jid = jid_str
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(jid_str.to_string()))?;

    let mut presence = Presence::new(if accept {
        PresenceType::Subscribed
    } else {
        PresenceType::Unsubscribed
    });
    presence.to = Some(to_jid);

    Ok(Stanza::Presence(Box::new(presence)))
}

fn build_muc_join_stanza(room: &str, nick: &str) -> Result<Stanza, OutboundRouterError> {
    let room_jid: jid::Jid = format!("{room}/{nick}")
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(format!("{room}/{nick}")))?;

    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(room_jid);

    let muc = Muc::new();
    let muc_element: xmpp_parsers::minidom::Element = muc.into();
    presence.payloads.push(muc_element);

    Ok(Stanza::Presence(Box::new(presence)))
}

fn build_muc_leave_stanza(room: &str) -> Result<Stanza, OutboundRouterError> {
    let room_jid: jid::Jid = room
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(room.to_string()))?;

    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.to = Some(room_jid);

    Ok(Stanza::Presence(Box::new(presence)))
}

fn build_muc_message_stanza(room: &str, body: &str) -> Result<Stanza, OutboundRouterError> {
    let room_jid: jid::Jid = room
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(room.to_string()))?;

    let mut msg = Message::new_with_type(XmppMessageType::Groupchat, Some(room_jid));
    msg.id = Some(xmpp_parsers::message::Id(Uuid::new_v4().to_string()));
    msg.bodies.insert(Lang::new(), body.to_string());

    Ok(Stanza::Message(Box::new(msg)))
}

fn build_chat_state_stanza(to: &str, state: &CoreChatState) -> Result<Stanza, OutboundRouterError> {
    let to_jid: jid::Jid = to
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(to.to_string()))?;

    let xmpp_state = match state {
        CoreChatState::Active => XmppChatState::Active,
        CoreChatState::Composing => XmppChatState::Composing,
        CoreChatState::Paused => XmppChatState::Paused,
        CoreChatState::Inactive => XmppChatState::Inactive,
        CoreChatState::Gone => XmppChatState::Gone,
    };

    let mut msg = Message::new(Some(to_jid));
    msg.type_ = XmppMessageType::Chat;
    let state_element: xmpp_parsers::minidom::Element = xmpp_state.into();
    msg.payloads.push(state_element);

    Ok(Stanza::Message(Box::new(msg)))
}

#[derive(Debug, thiserror::Error)]
pub enum OutboundRouterError {
    #[error("failed to subscribe to events: {0}")]
    SubscriptionFailed(String),

    #[error("invalid JID: {0}")]
    InvalidJid(String),

    #[error("outbound pipeline failed: {0}")]
    PipelineFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_chat_message_stanza() {
        let stanza =
            build_message_stanza("bob@example.com", "Hello!", &CoreMessageType::Chat).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.type_, XmppMessageType::Chat);
        assert_eq!(
            msg.to.as_ref().map(|j| j.to_string()),
            Some("bob@example.com".to_string())
        );
        assert_eq!(msg.bodies.get("").map(String::as_str), Some("Hello!"));
        assert!(msg.id.is_some());
    }

    #[test]
    fn builds_groupchat_message_stanza() {
        let stanza = build_message_stanza(
            "room@conference.example.com",
            "Hi room!",
            &CoreMessageType::Groupchat,
        )
        .unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.type_, XmppMessageType::Groupchat);
    }

    #[test]
    fn rejects_invalid_jid_in_message() {
        let result = build_message_stanza("not a jid!!!", "body", &CoreMessageType::Chat);
        assert!(result.is_err());
    }

    #[test]
    fn builds_available_presence() {
        let stanza = build_presence_stanza(&CorePresenceShow::Available, None);
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::None);
        assert!(p.show.is_none());
    }

    #[test]
    fn builds_away_presence_with_status() {
        let stanza = build_presence_stanza(&CorePresenceShow::Away, Some("brb"));
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.show, Some(Show::Away));
        assert_eq!(p.statuses.get("").map(String::as_str), Some("brb"));
    }

    #[test]
    fn builds_unavailable_presence() {
        let stanza = build_presence_stanza(&CorePresenceShow::Unavailable, None);
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Unavailable);
    }

    #[test]
    fn builds_dnd_presence() {
        let stanza = build_presence_stanza(&CorePresenceShow::Dnd, Some("busy"));
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.show, Some(Show::Dnd));
        assert_eq!(p.statuses.get("").map(String::as_str), Some("busy"));
    }

    #[test]
    fn builds_roster_add_stanza_test() {
        let stanza =
            build_roster_add_stanza("alice@example.com", Some("Alice"), &["Friends".to_string()])
                .unwrap();
        let Stanza::Iq(iq) = &stanza else {
            panic!("expected iq stanza");
        };
        match iq.as_ref() {
            Iq::Set { payload, .. } => {
                let query = roster::Roster::try_from(payload.clone()).unwrap();
                assert_eq!(query.items.len(), 1);
                assert_eq!(query.items[0].jid.to_string(), "alice@example.com");
                assert_eq!(query.items[0].name, Some("Alice".to_string()));
                assert_eq!(query.items[0].groups.len(), 1);
            }
            _ => panic!("expected IQ set"),
        }
    }

    #[test]
    fn builds_roster_remove_stanza_test() {
        let stanza = build_roster_remove_stanza("alice@example.com").unwrap();
        let Stanza::Iq(iq) = &stanza else {
            panic!("expected iq stanza");
        };
        match iq.as_ref() {
            Iq::Set { payload, .. } => {
                let query = roster::Roster::try_from(payload.clone()).unwrap();
                assert_eq!(query.items[0].subscription, roster::Subscription::Remove);
            }
            _ => panic!("expected IQ set"),
        }
    }

    #[test]
    fn builds_subscription_accept() {
        let stanza = build_subscription_response_stanza("carol@example.com", true).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Subscribed);
        assert_eq!(
            p.to.as_ref().map(|j| j.to_string()),
            Some("carol@example.com".to_string())
        );
    }

    #[test]
    fn builds_subscription_reject() {
        let stanza = build_subscription_response_stanza("carol@example.com", false).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Unsubscribed);
    }

    #[test]
    fn builds_muc_join_stanza_test() {
        let stanza = build_muc_join_stanza("room@conference.example.com", "mynick").unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(
            p.to.as_ref().map(|j| j.to_string()),
            Some("room@conference.example.com/mynick".to_string())
        );
        let has_muc = p
            .payloads
            .iter()
            .any(|el| Muc::try_from(el.clone()).is_ok());
        assert!(has_muc, "MUC join presence should contain <x/> element");
    }

    #[test]
    fn builds_muc_leave_stanza_test() {
        let stanza = build_muc_leave_stanza("room@conference.example.com").unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Unavailable);
        assert_eq!(
            p.to.as_ref().map(|j| j.to_string()),
            Some("room@conference.example.com".to_string())
        );
    }

    #[test]
    fn builds_muc_message_stanza_test() {
        let stanza =
            build_muc_message_stanza("room@conference.example.com", "Hello room!").unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.type_, XmppMessageType::Groupchat);
        assert_eq!(
            msg.to.as_ref().map(|j| j.to_string()),
            Some("room@conference.example.com".to_string())
        );
        assert_eq!(msg.bodies.get("").map(String::as_str), Some("Hello room!"));
    }

    #[test]
    fn builds_chat_state_composing() {
        let stanza = build_chat_state_stanza("bob@example.com", &CoreChatState::Composing).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.type_, XmppMessageType::Chat);
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Composing)));
    }

    #[test]
    fn builds_chat_state_active() {
        let stanza = build_chat_state_stanza("bob@example.com", &CoreChatState::Active).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Active)));
    }

    #[test]
    fn builds_chat_state_gone() {
        let stanza = build_chat_state_stanza("bob@example.com", &CoreChatState::Gone).unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        let state = msg
            .payloads
            .iter()
            .find_map(|el| XmppChatState::try_from(el.clone()).ok());
        assert!(matches!(state, Some(XmppChatState::Gone)));
    }

    #[test]
    fn all_stanzas_serialize_to_valid_xml() {
        let stanzas = vec![
            build_message_stanza("bob@example.com", "test", &CoreMessageType::Chat).unwrap(),
            build_presence_stanza(&CorePresenceShow::Available, None),
            build_presence_stanza(&CorePresenceShow::Away, Some("brb")),
            build_presence_stanza(&CorePresenceShow::Unavailable, None),
            build_roster_add_stanza("alice@example.com", Some("Alice"), &[]).unwrap(),
            build_roster_remove_stanza("alice@example.com").unwrap(),
            build_subscription_response_stanza("carol@example.com", true).unwrap(),
            build_subscription_response_stanza("carol@example.com", false).unwrap(),
            build_muc_join_stanza("room@conference.example.com", "nick").unwrap(),
            build_muc_leave_stanza("room@conference.example.com").unwrap(),
            build_muc_message_stanza("room@conference.example.com", "hi").unwrap(),
            build_chat_state_stanza("bob@example.com", &CoreChatState::Composing).unwrap(),
        ];

        for stanza in stanzas {
            let bytes = stanza.to_bytes().expect("stanza should serialize");
            let reparsed = Stanza::parse(&bytes).expect("serialized stanza should reparse");
            assert_eq!(reparsed.name(), stanza.name());
        }
    }
}
