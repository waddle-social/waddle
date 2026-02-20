// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Stanza dispatch: parse raw XMPP elements into typed client events.
//!
//! This module replaces the `xmpp` crate's `Agent` event loop by parsing
//! `minidom::Element` stanzas directly. This gives us access to the full
//! stanza including custom payloads (e.g. `urn:waddle:github:0` embeds)
//! that the `xmpp` Agent discards.

use std::str::FromStr;

use minidom::Element;
use tracing::{debug, warn};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid::{BareJid, FullJid, Jid};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::muc::user::{MucUser, Status};
use xmpp_parsers::ns;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::roster::{Item as RosterItem, Roster};

use crate::sanitize::sanitize_for_terminal;

/// Maximum length for sanitized string fields (nicknames, body, etc.)
const MAX_NICK_LEN: usize = 256;
const MAX_BODY_LEN: usize = 65_536;

/// A raw embed payload extracted from a message stanza.
/// The CLI does NOT interpret these — they are passed to the plugin runtime.
#[derive(Debug, Clone)]
pub struct RawEmbed {
    /// XML namespace of the embed element.
    pub namespace: String,
    /// Element name.
    pub name: String,
    /// Serialized XML of the embed element (for plugin consumption).
    pub xml: String,
}

/// Events produced by stanza dispatch.
#[derive(Debug, Clone)]
pub enum StanzaEvent {
    /// Received a message in a MUC room.
    RoomMessage {
        room_jid: BareJid,
        sender_nick: String,
        body: String,
        id: Option<String>,
        embeds: Vec<RawEmbed>,
    },
    /// Received a direct chat message.
    ChatMessage {
        from: BareJid,
        body: String,
        id: Option<String>,
        embeds: Vec<RawEmbed>,
    },
    /// MUC room subject was set.
    RoomSubject {
        room_jid: BareJid,
        nick: Option<String>,
        subject: String,
    },
    /// Successfully joined a MUC room (self-presence with status 110).
    RoomJoined { room_jid: BareJid },
    /// Left a MUC room (unavailable self-presence).
    RoomLeft { room_jid: BareJid },
    /// A roster contact was received.
    RosterItem(RosterItem),
    /// Presence update from a contact.
    ContactPresence {
        jid: BareJid,
        available: bool,
        show: Option<String>,
        status: Option<String>,
    },
    /// MAM result: a forwarded historical message.
    MamMessage {
        room_jid: Option<BareJid>,
        id: Option<String>,
        from: Option<Jid>,
        body: String,
        sender_nick: Option<String>,
        embeds: Vec<RawEmbed>,
        timestamp: Option<String>,
    },
    /// MAM query completed (fin element received).
    MamFinished {
        complete: bool,
    },
    /// An IQ result we don't specifically handle.
    UnhandledIq(Iq),
}

/// Known namespaces that are part of standard XMPP and should NOT be treated as embeds.
fn is_standard_payload(ns: &str, name: &str) -> bool {
    matches!(
        (ns, name),
        (ns::DELAY, "delay")
            | ("urn:xmpp:mam:2", "result")
            | ("urn:xmpp:sid:0", _)
            | ("jabber:x:oob", _)
            | ("http://jabber.org/protocol/chatstates", _)
            | (ns::MUC_USER, _)
            | ("urn:xmpp:receipts", _)
            | ("urn:xmpp:hints", _)
            | ("urn:xmpp:message-correct:0", _)
            | ("urn:xmpp:reply:0", _)
            | ("urn:xmpp:fallback:0", _)
    )
}

/// Extract non-standard payload elements from a message as raw embeds.
fn extract_embeds(message: &Message) -> Vec<RawEmbed> {
    message
        .payloads
        .iter()
        .filter(|p| !is_standard_payload(&p.ns(), p.name()))
        .map(|p| RawEmbed {
            namespace: p.ns().to_string(),
            name: p.name().to_string(),
            xml: String::from(p),
        })
        .collect()
}

/// Dispatch a raw stanza element into zero or more `StanzaEvent`s.
pub fn dispatch_stanza(elem: Element) -> Vec<StanzaEvent> {
    if elem.is("message", "jabber:client") {
        match Message::try_from(elem) {
            Ok(msg) => dispatch_message(msg),
            Err(e) => {
                warn!("Failed to parse message stanza: {e}");
                vec![]
            }
        }
    } else if elem.is("presence", "jabber:client") {
        match Presence::try_from(elem) {
            Ok(pres) => dispatch_presence(pres),
            Err(e) => {
                warn!("Failed to parse presence stanza: {e}");
                vec![]
            }
        }
    } else if elem.is("iq", "jabber:client") {
        match Iq::try_from(elem) {
            Ok(iq) => dispatch_iq(iq),
            Err(e) => {
                warn!("Failed to parse IQ stanza: {e}");
                vec![]
            }
        }
    } else {
        debug!("Ignoring unknown stanza: {}", elem.name());
        vec![]
    }
}

fn dispatch_message(msg: Message) -> Vec<StanzaEvent> {
    let mut events = vec![];
    let from = match msg.from.as_ref() {
        Some(f) => f.clone(),
        None => return events,
    };

    // Check for MAM result wrapper
    for payload in &msg.payloads {
        if payload.is("result", "urn:xmpp:mam:2") {
            if let Some(event) = parse_mam_result(payload) {
                events.push(event);
            }
            return events;
        }
    }

    let embeds = extract_embeds(&msg);

    match msg.type_ {
        MessageType::Groupchat => {
            // Check for subject
            if let Some((_lang, subject)) = msg.get_best_subject(vec!["en", ""]) {
                let nick = match from.clone().try_into_full() {
                    Ok(full) => Some(sanitize_for_terminal(full.resource().as_str(), Some(MAX_NICK_LEN))),
                    Err(_) => None,
                };
                events.push(StanzaEvent::RoomSubject {
                    room_jid: from.to_bare(),
                    nick,
                    subject: sanitize_for_terminal(&subject.0, Some(MAX_BODY_LEN)),
                });
            }

            // Check for body
            if let Some((_lang, body)) = msg.get_best_body(vec!["en", ""]) {
                match from.clone().try_into_full() {
                    Ok(full) => {
                        events.push(StanzaEvent::RoomMessage {
                            room_jid: from.to_bare(),
                            sender_nick: sanitize_for_terminal(
                                full.resource().as_str(),
                                Some(MAX_NICK_LEN),
                            ),
                            body: sanitize_for_terminal(&body.0, Some(MAX_BODY_LEN)),
                            id: msg.id.clone(),
                            embeds,
                        });
                    }
                    Err(_bare) => {
                        // Service message (from bare JID = room itself)
                        debug!("Service message from {}: {}", from, body.0);
                    }
                }
            }
        }
        MessageType::Chat | MessageType::Normal => {
            if let Some((_lang, body)) = msg.get_best_body(vec!["en", ""]) {
                events.push(StanzaEvent::ChatMessage {
                    from: from.to_bare(),
                    body: sanitize_for_terminal(&body.0, Some(MAX_BODY_LEN)),
                    id: msg.id.clone(),
                    embeds,
                });
            }
        }
        _ => {}
    }

    events
}

fn dispatch_presence(pres: Presence) -> Vec<StanzaEvent> {
    let mut events = vec![];
    let from = match pres.from.as_ref() {
        Some(f) => f.clone(),
        None => return events,
    };

    // Check if this is a MUC presence (has MucUser payload)
    let is_muc = pres
        .payloads
        .iter()
        .any(|p| p.is("x", ns::MUC_USER));

    if is_muc {
        // Parse MUC user payload to check for self-presence (status 110)
        let is_self = pres.payloads.iter().any(|p| {
            if !p.is("x", ns::MUC_USER) {
                return false;
            }
            MucUser::try_from(p.clone())
                .map(|mu| {
                    mu.status
                        .iter()
                        .any(|s| *s == Status::SelfPresence)
                })
                .unwrap_or(false)
        });

        match pres.type_ {
            PresenceType::None if is_self => {
                events.push(StanzaEvent::RoomJoined {
                    room_jid: from.to_bare(),
                });
            }
            PresenceType::Unavailable if is_self => {
                events.push(StanzaEvent::RoomLeft {
                    room_jid: from.to_bare(),
                });
            }
            _ => {}
        }
    } else {
        // Regular contact presence
        let available = !matches!(pres.type_, PresenceType::Unavailable);
        let show = pres.show.as_ref().map(|s| format!("{s:?}"));
        let status_text = pres
            .statuses
            .get("")
            .or_else(|| pres.statuses.values().next())
            .map(|s| sanitize_for_terminal(s, Some(MAX_BODY_LEN)));

        events.push(StanzaEvent::ContactPresence {
            jid: from.to_bare(),
            available,
            show,
            status: status_text,
        });
    }

    events
}

fn dispatch_iq(iq: Iq) -> Vec<StanzaEvent> {
    use xmpp_parsers::iq::IqType;

    let mut events = vec![];

    // Extract the element from the IQ payload
    let elem = match &iq.payload {
        IqType::Result(Some(elem)) => Some(elem.clone()),
        IqType::Set(elem) => Some(elem.clone()),
        IqType::Get(elem) => Some(elem.clone()),
        _ => None,
    };

    if let Some(elem) = elem {
        // Handle roster result
        if elem.is("query", ns::ROSTER) {
            if let Ok(roster) = Roster::try_from(elem) {
                for item in roster.items {
                    events.push(StanzaEvent::RosterItem(item));
                }
                return events;
            }
        } else if elem.is("fin", "urn:xmpp:mam:2") {
            // Handle MAM fin
            let complete = elem.attr("complete").map(|v| v == "true").unwrap_or(false);
            events.push(StanzaEvent::MamFinished { complete });
            return events;
        }
    }

    events.push(StanzaEvent::UnhandledIq(iq));
    events
}

/// Parse a MAM `<result>` element containing a forwarded message.
fn parse_mam_result(result_elem: &Element) -> Option<StanzaEvent> {
    let forwarded = result_elem
        .get_child("forwarded", "urn:xmpp:forward:0")?;

    let delay_elem = forwarded.get_child("delay", ns::DELAY);
    let timestamp = delay_elem.and_then(|d| d.attr("stamp")).map(|s| s.to_string());

    let msg_elem = forwarded
        .get_child("message", "jabber:client")?;

    let message = Message::try_from(msg_elem.clone()).ok()?;
    let from = message.from.clone();

    let body = message
        .get_best_body(vec!["en", ""])
        .map(|(_, b)| sanitize_for_terminal(&b.0, Some(MAX_BODY_LEN)))?;

    let embeds = extract_embeds(&message);

    let sender_nick = from.as_ref().and_then(|f| {
        FullJid::from_str(&f.to_string())
            .ok()
            .map(|full| sanitize_for_terminal(full.resource().as_str(), Some(MAX_NICK_LEN)))
    });

    let room_jid = from.as_ref().map(|f| f.to_bare());

    Some(StanzaEvent::MamMessage {
        room_jid,
        id: message.id.clone(),
        from: from.clone(),
        body,
        sender_nick,
        embeds,
        timestamp,
    })
}

/// Build initial presence stanza for sending after connection.
pub fn build_initial_presence() -> Element {
    let pres = Presence {
        from: None,
        to: None,
        id: None,
        type_: PresenceType::None,
        show: None,
        statuses: Default::default(),
        priority: 0i8,
        payloads: vec![],
    };
    pres.into()
}

/// Build a roster query IQ stanza.
pub fn build_roster_query() -> Element {
    let roster = Roster {
        ver: None,
        items: vec![],
    };
    let iq = Iq::from_get("roster", roster);
    iq.into()
}

/// Build a MUC join presence stanza.
pub fn build_muc_join(room_jid: &BareJid, nickname: &str) -> Element {
    let full_jid = format!("{}/{}", room_jid, nickname);
    let jid = Jid::from_str(&full_jid).expect("valid room occupant JID");

    let muc_elem = Element::builder("x", ns::MUC).build();

    let pres = Presence {
        from: None,
        to: Some(jid),
        id: None,
        type_: PresenceType::None,
        show: None,
        statuses: Default::default(),
        priority: 0i8,
        payloads: vec![muc_elem],
    };

    pres.into()
}

/// Build a MUC leave presence stanza (unavailable presence to room).
pub fn build_muc_leave(room_jid: &BareJid, nickname: &str) -> Element {
    let full_jid = format!("{}/{}", room_jid, nickname);
    let jid = Jid::from_str(&full_jid).expect("valid room occupant JID");

    let pres = Presence {
        from: None,
        to: Some(jid),
        id: None,
        type_: PresenceType::Unavailable,
        show: None,
        statuses: Default::default(),
        priority: 0i8,
        payloads: vec![],
    };
    pres.into()
}

/// Build a MAM query for the last N messages.
///
/// For MUC MAM (XEP-0313 §5.4), set `to` to the room JID.
/// For user archive, pass `None`.
pub fn build_mam_query(query_id: &str, to: Option<&BareJid>, max_results: u32) -> Element {
    let set = Element::builder("set", "http://jabber.org/protocol/rsm")
        .append(
            Element::builder("max", "http://jabber.org/protocol/rsm")
                .append(max_results.to_string())
                .build(),
        )
        .append(
            Element::builder("before", "http://jabber.org/protocol/rsm").build(),
        )
        .build();

    let query = Element::builder("query", "urn:xmpp:mam:2")
        .attr("queryid", query_id)
        .append(set)
        .build();

    // Build IQ set stanza manually since Element doesn't impl IqSetPayload
    let mut builder = Element::builder("iq", "jabber:client")
        .attr("type", "set")
        .attr("id", query_id);
    if let Some(jid) = to {
        builder = builder.attr("to", jid.to_string());
    }
    builder.append(query).build()
}

/// Build a chat message stanza.
pub fn build_chat_message(to: &BareJid, body: &str) -> Element {
    let mut msg = Message::new(Some(Jid::from(to.clone())));
    msg.type_ = MessageType::Chat;
    msg.bodies
        .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
    msg.into()
}

/// Build a groupchat message stanza.
pub fn build_room_message(room_jid: &BareJid, body: &str) -> Element {
    let mut msg = Message::new(Some(Jid::from(room_jid.clone())));
    msg.type_ = MessageType::Groupchat;
    msg.bodies
        .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
    msg.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_groupchat_message(from: &str, body: &str) -> Element {
        let xml = format!(
            r#"<message xmlns='jabber:client' type='groupchat' from='{}'>
                <body>{}</body>
            </message>"#,
            from, body
        );
        xml.parse().unwrap()
    }

    fn make_chat_message(from: &str, body: &str) -> Element {
        let xml = format!(
            r#"<message xmlns='jabber:client' type='chat' from='{}'>
                <body>{}</body>
            </message>"#,
            from, body
        );
        xml.parse().unwrap()
    }

    #[test]
    fn dispatch_groupchat_message() {
        let elem = make_groupchat_message("room@muc.example.com/alice", "Hello!");
        let events = dispatch_stanza(elem);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StanzaEvent::RoomMessage {
                room_jid,
                sender_nick,
                body,
                ..
            } => {
                assert_eq!(room_jid.to_string(), "room@muc.example.com");
                assert_eq!(sender_nick, "alice");
                assert_eq!(body, "Hello!");
            }
            other => panic!("Expected RoomMessage, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_chat_message() {
        let elem = make_chat_message("bob@example.com", "Hi there");
        let events = dispatch_stanza(elem);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StanzaEvent::ChatMessage { from, body, .. } => {
                assert_eq!(from.to_string(), "bob@example.com");
                assert_eq!(body, "Hi there");
            }
            other => panic!("Expected ChatMessage, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_message_with_custom_payload() {
        let xml = r#"<message xmlns='jabber:client' type='chat' from='bob@example.com'>
            <body>Check this out</body>
            <repo xmlns='urn:waddle:github:0' url='https://github.com/a/b' owner='a' name='b'/>
        </message>"#;
        let elem: Element = xml.parse().unwrap();
        let events = dispatch_stanza(elem);

        assert_eq!(events.len(), 1);
        match &events[0] {
            StanzaEvent::ChatMessage { embeds, body, .. } => {
                assert_eq!(body, "Check this out");
                assert_eq!(embeds.len(), 1);
                assert_eq!(embeds[0].namespace, "urn:waddle:github:0");
                assert_eq!(embeds[0].name, "repo");
                assert!(embeds[0].xml.contains("owner"));
            }
            other => panic!("Expected ChatMessage, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_message_filters_standard_payloads() {
        let xml = r#"<message xmlns='jabber:client' type='chat' from='bob@example.com'>
            <body>Hello</body>
            <delay xmlns='urn:xmpp:delay' stamp='2025-01-01T00:00:00Z'/>
            <active xmlns='http://jabber.org/protocol/chatstates'/>
        </message>"#;
        let elem: Element = xml.parse().unwrap();
        let events = dispatch_stanza(elem);

        match &events[0] {
            StanzaEvent::ChatMessage { embeds, .. } => {
                assert!(embeds.is_empty(), "Standard payloads should not be embeds");
            }
            other => panic!("Expected ChatMessage, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_sanitizes_body() {
        // Use a body with bidi override chars (valid XML, unlike ANSI escapes)
        let xml = "<message xmlns='jabber:client' type='chat' from='bob@example.com'>\
            <body>hello\u{202E}evil\u{202C}world</body>\
            </message>";
        let elem: Element = xml.parse().unwrap();
        let events = dispatch_stanza(elem);
        match &events[0] {
            StanzaEvent::ChatMessage { body, .. } => {
                assert_eq!(body, "helloevilworld"); // bidi chars stripped
            }
            other => panic!("Expected ChatMessage, got {:?}", other),
        }
    }

    #[test]
    fn build_muc_join_valid() {
        let room = BareJid::from_str("room@muc.example.com").unwrap();
        let elem = build_muc_join(&room, "mynick");
        assert_eq!(elem.name(), "presence");
        assert_eq!(
            elem.attr("to").unwrap(),
            "room@muc.example.com/mynick"
        );
        // Should have <x xmlns='...muc'> child
        assert!(elem.get_child("x", ns::MUC).is_some());
    }

    #[test]
    fn build_muc_leave_valid() {
        let room = BareJid::from_str("room@muc.example.com").unwrap();
        let elem = build_muc_leave(&room, "mynick");
        assert_eq!(elem.name(), "presence");
        assert_eq!(elem.attr("type").unwrap(), "unavailable");
    }

    #[test]
    fn build_chat_message_valid() {
        let to = BareJid::from_str("bob@example.com").unwrap();
        let elem = build_chat_message(&to, "Hello!");
        assert_eq!(elem.name(), "message");
        assert_eq!(elem.attr("type").unwrap(), "chat");
    }

    #[test]
    fn build_room_message_valid() {
        let to = BareJid::from_str("room@muc.example.com").unwrap();
        let elem = build_room_message(&to, "Hello room!");
        assert_eq!(elem.name(), "message");
        assert_eq!(elem.attr("type").unwrap(), "groupchat");
    }

    #[test]
    fn build_mam_query_valid() {
        let room = BareJid::from_str("room@muc.example.com").unwrap();
        let elem = build_mam_query("q1", Some(&room), 50);
        assert_eq!(elem.name(), "iq");
        assert_eq!(elem.attr("to").unwrap(), "room@muc.example.com");
        assert_eq!(elem.attr("type").unwrap(), "set");
        let query = elem.get_child("query", "urn:xmpp:mam:2").unwrap();
        assert_eq!(query.attr("queryid").unwrap(), "q1");
    }

    #[test]
    fn build_mam_query_no_to() {
        let elem = build_mam_query("q2", None, 25);
        assert_eq!(elem.name(), "iq");
        assert!(elem.attr("to").is_none());
    }

    #[test]
    fn build_roster_query_valid() {
        let elem = build_roster_query();
        assert_eq!(elem.name(), "iq");
        assert!(elem.get_child("query", ns::ROSTER).is_some());
    }
}
