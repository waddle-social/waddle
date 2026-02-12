use std::sync::Arc;

#[cfg(feature = "native")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "native")]
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use uuid::Uuid;
use xmpp_parsers::chatstates::ChatState as XmppChatState;
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid;
use xmpp_parsers::mam;
use xmpp_parsers::message::{Lang, Message, MessageType as XmppMessageType};
use xmpp_parsers::muc::Muc;
use xmpp_parsers::presence::{Presence, Show, Type as PresenceType};
use xmpp_parsers::roster;
use xmpp_parsers::rsm;

use waddle_core::event::{
    ChatMessage, ChatState as CoreChatState, Event, EventPayload, EventSource,
    MessageType as CoreMessageType, PresenceShow as CorePresenceShow,
};

#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus};

use crate::pipeline::StanzaPipeline;
use crate::stanza::Stanza;

#[cfg(feature = "native")]
pub type StanzaSender = mpsc::Sender<Vec<u8>>;

#[cfg(feature = "native")]
pub type StanzaReceiver = mpsc::Receiver<Vec<u8>>;

#[cfg(feature = "native")]
pub fn stanza_channel(buffer: usize) -> (StanzaSender, StanzaReceiver) {
    mpsc::channel(buffer)
}

#[cfg(feature = "native")]
const OFFLINE_DRAIN_SOURCE: &str = "offline";

pub struct OutboundRouter {
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
    pipeline: Arc<StanzaPipeline>,
    #[cfg(feature = "native")]
    wire_sender: StanzaSender,
    #[cfg(feature = "native")]
    is_online: AtomicBool,
}

impl OutboundRouter {
    #[cfg(feature = "native")]
    pub fn new(
        event_bus: Arc<dyn EventBus>,
        pipeline: Arc<StanzaPipeline>,
        wire_sender: StanzaSender,
    ) -> Self {
        Self {
            event_bus,
            pipeline,
            wire_sender,
            is_online: AtomicBool::new(false),
        }
    }

    #[cfg(feature = "native")]
    pub async fn run(&self) -> Result<(), OutboundRouterError> {
        let mut subscription = self
            .event_bus
            .subscribe("{ui,system}.**")
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
        match &event.payload {
            EventPayload::ConnectionEstablished { .. } | EventPayload::ComingOnline => {
                self.is_online.store(true, Ordering::Relaxed);
                return Ok(());
            }
            EventPayload::ConnectionLost { .. }
            | EventPayload::ConnectionReconnecting { .. }
            | EventPayload::GoingOffline => {
                self.is_online.store(false, Ordering::Relaxed);
                return Ok(());
            }
            _ => {}
        }

        let can_send_while_offline = matches!(event.source, EventSource::System(ref source) if source == OFFLINE_DRAIN_SOURCE);
        if !self.is_online.load(Ordering::Relaxed) && !can_send_while_offline {
            return Ok(());
        }

        let mut message_sent = None;
        let mut own_presence_changed = None;

        let stanza = match &event.payload {
            EventPayload::MessageSendRequested {
                to,
                body,
                message_type,
            } => {
                let message_id = event
                    .correlation_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let stanza =
                    build_message_stanza(to, body, message_type, Some(message_id.as_str()))?;
                message_sent = Some((message_id, to.clone(), body.clone(), message_type.clone()));
                Some(stanza)
            }
            EventPayload::PresenceSetRequested { show, status } => {
                let stanza = build_presence_stanza(show, status.as_deref());
                own_presence_changed = Some((show.clone(), status.clone()));
                Some(stanza)
            }
            EventPayload::RosterAddRequested { jid, name, groups } => {
                Some(build_roster_add_stanza(jid, name.as_deref(), groups)?)
            }
            EventPayload::RosterUpdateRequested { jid, name, groups } => {
                Some(build_roster_add_stanza(jid, name.as_deref(), groups)?)
            }
            EventPayload::RosterRemoveRequested { jid } => Some(build_roster_remove_stanza(jid)?),
            EventPayload::RosterFetchRequested => Some(build_roster_get_stanza()),
            EventPayload::SubscriptionRespondRequested { jid, accept } => {
                Some(build_subscription_response_stanza(jid, *accept)?)
            }
            EventPayload::SubscriptionSendRequested { jid, subscribe } => {
                Some(build_subscription_send_stanza(jid, *subscribe)?)
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
            EventPayload::MamQueryRequested {
                query_id,
                with_jid,
                after,
                before,
                max,
            } => Some(build_mam_query_stanza(
                query_id, with_jid, after, before, *max,
            )),
            _ => None,
        };

        if let Some(stanza) = stanza {
            let bytes = self
                .pipeline
                .process_outbound(stanza)
                .await
                .map_err(|e| OutboundRouterError::PipelineFailed(e.to_string()))?;

            self.wire_sender
                .send(bytes)
                .await
                .map_err(|_| OutboundRouterError::WireSendFailed)?;

            if let Some((message_id, to, body, message_type)) = message_sent {
                self.emit_message_sent(event, &message_id, &to, &body, &message_type);
            }

            if let Some((show, status)) = own_presence_changed {
                self.emit_own_presence_changed(&show, status.as_deref());
            }
        }

        Ok(())
    }

    #[cfg(feature = "native")]
    fn emit_message_sent(
        &self,
        event: &Event,
        message_id: &str,
        to: &str,
        body: &str,
        message_type: &CoreMessageType,
    ) {
        let channel = match Channel::new("xmpp.message.sent") {
            Ok(c) => c,
            Err(_) => return,
        };

        let bare_to = to
            .parse::<jid::Jid>()
            .map(|j| j.to_bare().to_string())
            .unwrap_or_else(|_| to.to_string());

        let msg = ChatMessage {
            id: message_id.to_string(),
            from: String::new(),
            to: bare_to,
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
    message_id: Option<&str>,
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
    msg.id = Some(xmpp_parsers::message::Id(
        message_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    ));
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

fn build_roster_get_stanza() -> Stanza {
    let query = roster::Roster {
        ver: None,
        items: vec![],
    };
    let iq = Iq::from_get(Uuid::new_v4().to_string(), query);
    Stanza::Iq(Box::new(iq))
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

fn build_subscription_send_stanza(
    jid_str: &str,
    subscribe: bool,
) -> Result<Stanza, OutboundRouterError> {
    let to_jid: jid::Jid = jid_str
        .parse()
        .map_err(|_| OutboundRouterError::InvalidJid(jid_str.to_string()))?;

    let mut presence = Presence::new(if subscribe {
        PresenceType::Subscribe
    } else {
        PresenceType::Unsubscribe
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

fn build_mam_query_stanza(
    query_id: &str,
    with_jid: &Option<String>,
    after: &Option<String>,
    before: &Option<String>,
    max: u32,
) -> Stanza {
    let set = rsm::SetQuery {
        max: Some(max as usize),
        after: after.clone(),
        before: before.clone(),
        index: None,
    };

    let form = with_jid.as_ref().map(|jid| {
        DataForm::new(
            DataFormType::Submit,
            "urn:xmpp:mam:2",
            vec![Field::text_single("with", jid)],
        )
    });

    let query = mam::Query {
        queryid: Some(mam::QueryId(query_id.to_string())),
        node: None,
        form,
        set: Some(set),
        flip_page: false,
    };

    let iq = Iq::from_set(query_id.to_string(), query);
    Stanza::Iq(Box::new(iq))
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

    #[error("wire send failed: transport channel closed")]
    WireSendFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_chat_message_stanza() {
        let stanza =
            build_message_stanza("bob@example.com", "Hello!", &CoreMessageType::Chat, None)
                .unwrap();
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
            None,
        )
        .unwrap();
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.type_, XmppMessageType::Groupchat);
    }

    #[test]
    fn rejects_invalid_jid_in_message() {
        let result = build_message_stanza("not a jid!!!", "body", &CoreMessageType::Chat, None);
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
    fn builds_subscription_subscribe() {
        let stanza = build_subscription_send_stanza("carol@example.com", true).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Subscribe);
        assert_eq!(
            p.to.as_ref().map(|j| j.to_string()),
            Some("carol@example.com".to_string())
        );
    }

    #[test]
    fn builds_subscription_unsubscribe() {
        let stanza = build_subscription_send_stanza("carol@example.com", false).unwrap();
        let Stanza::Presence(p) = &stanza else {
            panic!("expected presence stanza");
        };
        assert_eq!(p.type_, PresenceType::Unsubscribe);
        assert_eq!(
            p.to.as_ref().map(|j| j.to_string()),
            Some("carol@example.com".to_string())
        );
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
    fn builds_mam_query_stanza_with_query_id_and_jid_filter() {
        let stanza = build_mam_query_stanza(
            "query-123",
            &Some("bob@example.com".to_string()),
            &Some("after-1".to_string()),
            &Some("before-1".to_string()),
            25,
        );
        let Stanza::Iq(iq) = &stanza else {
            panic!("expected iq stanza");
        };

        let (id, payload) = match iq.as_ref() {
            Iq::Set { id, payload, .. } => (id, payload),
            _ => panic!("expected IQ set"),
        };

        assert_eq!(id, "query-123");

        let query = mam::Query::try_from(payload.clone()).expect("payload should be MAM query");
        assert_eq!(
            query.queryid.as_ref().map(|query_id| query_id.0.as_str()),
            Some("query-123")
        );

        let set = query.set.expect("MAM query should have RSM set");
        assert_eq!(set.max, Some(25));
        assert_eq!(set.after.as_deref(), Some("after-1"));
        assert_eq!(set.before.as_deref(), Some("before-1"));

        let form = query.form.expect("MAM query should include form filter");
        let with_field = form
            .fields
            .iter()
            .find(|field| field.var.as_deref() == Some("with"))
            .expect("MAM query should include `with` field");
        assert_eq!(
            with_field.values.first().map(String::as_str),
            Some("bob@example.com")
        );
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
            build_message_stanza("bob@example.com", "test", &CoreMessageType::Chat, None).unwrap(),
            build_presence_stanza(&CorePresenceShow::Available, None),
            build_presence_stanza(&CorePresenceShow::Away, Some("brb")),
            build_presence_stanza(&CorePresenceShow::Unavailable, None),
            build_roster_add_stanza("alice@example.com", Some("Alice"), &[]).unwrap(),
            build_roster_remove_stanza("alice@example.com").unwrap(),
            build_subscription_response_stanza("carol@example.com", true).unwrap(),
            build_subscription_response_stanza("carol@example.com", false).unwrap(),
            build_subscription_send_stanza("carol@example.com", true).unwrap(),
            build_subscription_send_stanza("carol@example.com", false).unwrap(),
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

#[cfg(all(test, feature = "native"))]
mod integration_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::timeout;
    use waddle_core::event::{
        BroadcastEventBus, Channel, ChatState as CoreChatState, Event, EventBus, EventPayload,
        EventSource, MessageType as CoreMessageType, PresenceShow as CorePresenceShow, UiTarget,
    };

    use super::*;
    use crate::pipeline::StanzaPipeline;

    fn make_router() -> (OutboundRouter, StanzaReceiver, Arc<dyn EventBus>) {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(64));
        let pipeline = Arc::new(StanzaPipeline::new());
        let (tx, rx) = stanza_channel(64);
        let router = OutboundRouter::new(event_bus.clone(), pipeline, tx);
        (router, rx, event_bus)
    }

    fn publish_ui_event(event_bus: &Arc<dyn EventBus>, channel: &str, payload: EventPayload) {
        let channel = Channel::new(channel).expect("valid channel");
        let event = Event::new(channel, EventSource::Ui(UiTarget::Tui), payload);
        event_bus.publish(event).expect("publish should succeed");
    }

    async fn publish_connection_established(event_bus: &Arc<dyn EventBus>) {
        let event = Event::new(
            Channel::new("system.connection.established").expect("valid channel"),
            EventSource::Xmpp,
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        event_bus
            .publish(event)
            .expect("publish connection established should succeed");
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    fn connection_established_event() -> Event {
        Event::new(
            Channel::new("system.connection.established").expect("valid channel"),
            EventSource::Xmpp,
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        )
    }

    async fn yield_to_router() {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn message_send_reaches_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.message.send",
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "Hello Bob!".to_string(),
                message_type: CoreMessageType::Chat,
            },
        );

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "message");
        let Stanza::Message(msg) = &stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.bodies.get("").map(String::as_str), Some("Hello Bob!"));

        _handle.abort();
    }

    #[tokio::test]
    async fn presence_set_reaches_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.presence.set",
            EventPayload::PresenceSetRequested {
                show: CorePresenceShow::Away,
                status: Some("brb".to_string()),
            },
        );

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "presence");

        _handle.abort();
    }

    #[tokio::test]
    async fn roster_add_reaches_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.roster.add",
            EventPayload::RosterAddRequested {
                jid: "alice@example.com".to_string(),
                name: Some("Alice".to_string()),
                groups: vec!["Friends".to_string()],
            },
        );

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "iq");

        _handle.abort();
    }

    #[tokio::test]
    async fn muc_join_reaches_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.muc.join",
            EventPayload::MucJoinRequested {
                room: "room@conference.example.com".to_string(),
                nick: "mynick".to_string(),
            },
        );

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "presence");

        _handle.abort();
    }

    #[tokio::test]
    async fn chat_state_reaches_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.chatstate.send",
            EventPayload::ChatStateSendRequested {
                to: "bob@example.com".to_string(),
                state: CoreChatState::Composing,
            },
        );

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "message");

        _handle.abort();
    }

    #[tokio::test]
    async fn message_sent_event_emitted_on_outbound_message() {
        let (router, mut rx, event_bus) = make_router();

        let mut sent_sub = event_bus
            .subscribe("xmpp.message.sent")
            .expect("subscribe should succeed");

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.message.send",
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "Test".to_string(),
                message_type: CoreMessageType::Chat,
            },
        );

        // Drain the wire bytes
        let _ = timeout(Duration::from_millis(200), rx.recv()).await;

        let event = timeout(Duration::from_millis(200), sent_sub.recv())
            .await
            .expect("timed out waiting for message.sent event")
            .expect("should receive event");

        assert_eq!(event.channel.as_str(), "xmpp.message.sent");
        assert!(matches!(event.payload, EventPayload::MessageSent { .. }));

        _handle.abort();
    }

    #[tokio::test]
    async fn message_send_uses_correlation_id_for_message_ids() {
        let (router, mut rx, event_bus) = make_router();

        let mut sent_sub = event_bus
            .subscribe("xmpp.message.sent")
            .expect("subscribe should succeed");

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        let correlation_id = Uuid::new_v4();
        let event = Event::with_correlation(
            Channel::new("ui.message.send").expect("valid channel"),
            EventSource::Ui(UiTarget::Tui),
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "Correlated".to_string(),
                message_type: CoreMessageType::Chat,
            },
            correlation_id,
        );
        event_bus.publish(event).expect("publish should succeed");

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        let Stanza::Message(msg) = stanza else {
            panic!("expected message stanza");
        };
        assert_eq!(msg.id.map(|id| id.0), Some(correlation_id.to_string()));

        let sent_event = timeout(Duration::from_millis(200), sent_sub.recv())
            .await
            .expect("timed out waiting for message.sent event")
            .expect("should receive event");

        assert_eq!(sent_event.correlation_id, Some(correlation_id));
        assert!(matches!(
            sent_event.payload,
            EventPayload::MessageSent {
                message: ChatMessage { ref id, .. },
            } if id == &correlation_id.to_string()
        ));

        _handle.abort();
    }

    #[tokio::test]
    async fn non_command_ui_events_do_not_produce_wire_bytes() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        publish_ui_event(
            &event_bus,
            "ui.theme.changed",
            EventPayload::ThemeChanged {
                theme_id: "dark".to_string(),
            },
        );

        let result = timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "non-command UI event should not produce wire bytes"
        );

        _handle.abort();
    }

    #[tokio::test]
    async fn ui_commands_are_ignored_while_offline() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;

        publish_ui_event(
            &event_bus,
            "ui.message.send",
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "offline".to_string(),
                message_type: CoreMessageType::Chat,
            },
        );

        let result = timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "offline UI command should not reach the wire"
        );

        _handle.abort();
    }

    #[tokio::test]
    async fn offline_replay_source_bypasses_offline_gate() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;

        event_bus
            .publish(Event::with_correlation(
                Channel::new("ui.message.send").expect("valid channel"),
                EventSource::System("offline".to_string()),
                EventPayload::MessageSendRequested {
                    to: "bob@example.com".to_string(),
                    body: "replay".to_string(),
                    message_type: CoreMessageType::Chat,
                },
                Uuid::new_v4(),
            ))
            .expect("publish should succeed");

        let bytes = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("timed out waiting for replayed wire bytes")
            .expect("channel should not be closed");

        let stanza = Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
        assert_eq!(stanza.name(), "message");

        _handle.abort();
    }

    #[tokio::test]
    async fn closed_wire_channel_returns_error() {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(64));
        let pipeline = Arc::new(StanzaPipeline::new());
        let (tx, rx) = stanza_channel(1);
        drop(rx);

        let router = OutboundRouter::new(event_bus.clone(), pipeline, tx);
        router
            .handle_event(&connection_established_event())
            .await
            .expect("connection established event should succeed");

        let event = Event::new(
            Channel::new("ui.message.send").unwrap(),
            EventSource::Ui(UiTarget::Tui),
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "test".to_string(),
                message_type: CoreMessageType::Chat,
            },
        );

        let result = router.handle_event(&event).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            OutboundRouterError::WireSendFailed
        ));
    }

    #[tokio::test]
    async fn closed_wire_channel_does_not_emit_message_sent_event() {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(64));
        let mut sent_sub = event_bus
            .subscribe("xmpp.message.sent")
            .expect("subscribe should succeed");
        let pipeline = Arc::new(StanzaPipeline::new());
        let (tx, rx) = stanza_channel(1);
        drop(rx);

        let router = OutboundRouter::new(event_bus.clone(), pipeline, tx);
        router
            .handle_event(&connection_established_event())
            .await
            .expect("connection established event should succeed");

        let event = Event::new(
            Channel::new("ui.message.send").expect("valid channel"),
            EventSource::Ui(UiTarget::Tui),
            EventPayload::MessageSendRequested {
                to: "bob@example.com".to_string(),
                body: "test".to_string(),
                message_type: CoreMessageType::Chat,
            },
        );

        let result = router.handle_event(&event).await;
        assert!(matches!(result, Err(OutboundRouterError::WireSendFailed)));

        let received = timeout(Duration::from_millis(100), sent_sub.recv()).await;
        assert!(
            received.is_err(),
            "message.sent should not be emitted when wire send fails"
        );
    }

    #[tokio::test]
    async fn closed_wire_channel_does_not_emit_own_presence_changed_event() {
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::new(64));
        let mut presence_sub = event_bus
            .subscribe("xmpp.presence.own_changed")
            .expect("subscribe should succeed");
        let pipeline = Arc::new(StanzaPipeline::new());
        let (tx, rx) = stanza_channel(1);
        drop(rx);

        let router = OutboundRouter::new(event_bus.clone(), pipeline, tx);
        router
            .handle_event(&connection_established_event())
            .await
            .expect("connection established event should succeed");

        let event = Event::new(
            Channel::new("ui.presence.set").expect("valid channel"),
            EventSource::Ui(UiTarget::Tui),
            EventPayload::PresenceSetRequested {
                show: CorePresenceShow::Away,
                status: Some("brb".to_string()),
            },
        );

        let result = router.handle_event(&event).await;
        assert!(matches!(result, Err(OutboundRouterError::WireSendFailed)));

        let received = timeout(Duration::from_millis(100), presence_sub.recv()).await;
        assert!(
            received.is_err(),
            "presence.own_changed should not be emitted when wire send fails"
        );
    }

    #[tokio::test]
    async fn all_command_types_reach_wire() {
        let (router, mut rx, event_bus) = make_router();

        let _handle = tokio::spawn(async move { router.run().await });
        yield_to_router().await;
        publish_connection_established(&event_bus).await;

        let commands: Vec<(&str, EventPayload)> = vec![
            (
                "ui.message.send",
                EventPayload::MessageSendRequested {
                    to: "bob@example.com".to_string(),
                    body: "hi".to_string(),
                    message_type: CoreMessageType::Chat,
                },
            ),
            (
                "ui.presence.set",
                EventPayload::PresenceSetRequested {
                    show: CorePresenceShow::Dnd,
                    status: None,
                },
            ),
            (
                "ui.roster.add",
                EventPayload::RosterAddRequested {
                    jid: "alice@example.com".to_string(),
                    name: None,
                    groups: vec![],
                },
            ),
            (
                "ui.roster.remove",
                EventPayload::RosterRemoveRequested {
                    jid: "alice@example.com".to_string(),
                },
            ),
            (
                "ui.subscription.respond",
                EventPayload::SubscriptionRespondRequested {
                    jid: "carol@example.com".to_string(),
                    accept: true,
                },
            ),
            (
                "ui.subscription.send",
                EventPayload::SubscriptionSendRequested {
                    jid: "carol@example.com".to_string(),
                    subscribe: true,
                },
            ),
            (
                "ui.muc.join",
                EventPayload::MucJoinRequested {
                    room: "room@conference.example.com".to_string(),
                    nick: "nick".to_string(),
                },
            ),
            (
                "ui.muc.leave",
                EventPayload::MucLeaveRequested {
                    room: "room@conference.example.com".to_string(),
                },
            ),
            (
                "ui.muc.send",
                EventPayload::MucSendRequested {
                    room: "room@conference.example.com".to_string(),
                    body: "hi room".to_string(),
                },
            ),
            (
                "ui.chatstate.send",
                EventPayload::ChatStateSendRequested {
                    to: "bob@example.com".to_string(),
                    state: CoreChatState::Active,
                },
            ),
            (
                "ui.mam.query",
                EventPayload::MamQueryRequested {
                    query_id: "mam-q1".to_string(),
                    with_jid: Some("bob@example.com".to_string()),
                    after: Some("a1".to_string()),
                    before: None,
                    max: 25,
                },
            ),
        ];

        let expected_count = commands.len();

        for (channel, payload) in commands {
            publish_ui_event(&event_bus, channel, payload);
        }

        let mut received = 0;
        for _ in 0..expected_count {
            let result = timeout(Duration::from_millis(500), rx.recv()).await;
            if let Ok(Some(bytes)) = result {
                Stanza::parse(&bytes).expect("wire bytes should parse as stanza");
                received += 1;
            }
        }

        assert_eq!(
            received, expected_count,
            "all command types should produce wire bytes"
        );

        _handle.abort();
    }
}
