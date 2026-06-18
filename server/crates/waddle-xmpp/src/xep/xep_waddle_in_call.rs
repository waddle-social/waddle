//! Waddle transient in-call signaling carrier (`urn:waddle:in-call:0`).

use minidom::Element;
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::{Id, Message, MessageType};

use super::xep0334::{add_hint, Hint};

pub const NS_WADDLE_IN_CALL: &str = "urn:waddle:in-call:0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InCallSessionId(SessionId);

impl InCallSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, InCallParseError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(InCallParseError::EmptySessionId);
        }
        Ok(Self(SessionId(value.to_owned())))
    }

    pub fn as_str(&self) -> &str {
        self.0 .0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InCallReactionEmoji(String);

impl InCallReactionEmoji {
    pub fn new(value: impl Into<String>) -> Result<Self, InCallParseError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(InCallParseError::EmptyReaction);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InCallReactionSignal {
    pub sid: InCallSessionId,
    pub emoji: InCallReactionEmoji,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InCallSignal {
    Reaction(InCallReactionSignal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InCallParseError {
    NotInCall,
    MissingAttribute(&'static str),
    MissingChild(&'static str),
    EmptySessionId,
    EmptyReaction,
}

pub fn build_in_call_reaction_element(
    sid: &InCallSessionId,
    emoji: &InCallReactionEmoji,
) -> Element {
    Element::builder("in-call", NS_WADDLE_IN_CALL)
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.as_str())
        .append(
            Element::builder("reaction", NS_WADDLE_IN_CALL)
                .attr(
                    minidom::rxml::xml_ncname!("emoji").to_owned(),
                    emoji.as_str(),
                )
                .build(),
        )
        .build()
}

pub fn build_in_call_reaction_message(
    to: jid::Jid,
    from: jid::Jid,
    sid: &InCallSessionId,
    emoji: &InCallReactionEmoji,
    message_type: MessageType,
) -> Message {
    let mut message = Message::new(Some(to));
    message.from = Some(from);
    message.type_ = message_type;
    message.id = Some(Id(uuid::Uuid::new_v4().to_string()));
    message
        .payloads
        .push(build_in_call_reaction_element(sid, emoji));
    add_hint(&mut message, Hint::NoStore);
    add_hint(&mut message, Hint::NoCopy);
    message
}

pub fn parse_in_call_signal(element: &Element) -> Result<InCallSignal, InCallParseError> {
    if element.name() != "in-call" || element.ns() != NS_WADDLE_IN_CALL {
        return Err(InCallParseError::NotInCall);
    }

    let sid = InCallSessionId::new(required_attr(element, "sid")?)?;
    let reaction = element
        .children()
        .find(|child| child.name() == "reaction" && child.ns() == NS_WADDLE_IN_CALL)
        .ok_or(InCallParseError::MissingChild("reaction"))?;
    let emoji = InCallReactionEmoji::new(required_attr(reaction, "emoji")?)?;

    Ok(InCallSignal::Reaction(InCallReactionSignal { sid, emoji }))
}

pub fn parse_in_call_signal_child(message: &Message) -> Option<InCallSignal> {
    message
        .payloads
        .iter()
        .find(|payload| payload.name() == "in-call" && payload.ns() == NS_WADDLE_IN_CALL)
        .and_then(|payload| parse_in_call_signal(payload).ok())
}

fn required_attr<'a>(
    element: &'a Element,
    name: &'static str,
) -> Result<&'a str, InCallParseError> {
    element
        .attr(name)
        .ok_or(InCallParseError::MissingAttribute(name))
}
