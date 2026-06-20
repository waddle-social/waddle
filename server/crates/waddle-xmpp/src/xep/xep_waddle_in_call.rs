//! Waddle in-call signaling carrier (`urn:waddle:in-call:0`).
//!
//! Two distinct shapes share this namespace and the `<in-call/>` root:
//!
//! - **Message-transient signals** (reactions): carried in a `<message/>`
//!   marked `no-store`/`no-copy`, identified by a `sid` attribute and a
//!   `<reaction/>` child. Fire-and-forget; never stored.
//! - **Presence-durable state** (raised hand): carried in MUC occupant
//!   presence as a child *alongside* (never inside) the `<muji/>` element,
//!   with a `<hand-raised/>` marker child. Stored in room state, replayed to
//!   late joiners, and cleared when the occupant leaves the call.

use minidom::Element;
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::{Id, Message, MessageType};

use super::xep0334::{add_hint, Hint};

pub const NS_WADDLE_IN_CALL: &str = "urn:waddle:in-call:0";

const IN_CALL_NAME: &str = "in-call";
const HAND_RAISED_NAME: &str = "hand-raised";
const MUTED_NAME: &str = "muted";

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

/// Presence-durable in-call state for one occupant session, carried in MUC
/// presence as `<in-call xmlns='urn:waddle:in-call:0'>` alongside `<muji/>`.
///
/// Models the durable in-call sub-states an occupant can advertise: a raised
/// hand and a muted microphone. Each is an independent marker child of the same
/// `<in-call/>` root, so the typed struct can grow further sub-states
/// (recording, …) without changing the wire root. An all-`false` value
/// [`is_empty`](Self::is_empty) and clears the occupant's stored entry,
/// mirroring `<muji/>` leave semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InCallPresenceState {
    pub hand_raised: bool,
    pub muted: bool,
}

impl InCallPresenceState {
    /// True when no sub-state is advertised, i.e. the occupant carries no
    /// in-call presence state and any stored entry should be cleared.
    pub fn is_empty(&self) -> bool {
        !self.hand_raised && !self.muted
    }
}

/// Build the `<in-call xmlns='urn:waddle:in-call:0'>` presence child for the
/// given state. Each advertised sub-state emits its own marker child
/// (`<hand-raised/>`, `<muted/>`); a state with none advertised emits an empty
/// `<in-call/>` element (the canonical "no state" shape a peer may also omit
/// entirely).
pub fn build_in_call_presence_state_element(state: &InCallPresenceState) -> Element {
    let mut builder = Element::builder(IN_CALL_NAME, NS_WADDLE_IN_CALL);
    if state.hand_raised {
        builder = builder.append(Element::builder(HAND_RAISED_NAME, NS_WADDLE_IN_CALL).build());
    }
    if state.muted {
        builder = builder.append(Element::builder(MUTED_NAME, NS_WADDLE_IN_CALL).build());
    }
    builder.build()
}

/// Parse an `<in-call xmlns='urn:waddle:in-call:0'>` presence child into its
/// durable state. The presence shape carries no `sid` (the occupant presence
/// already identifies the participant and room); each marker child raises its
/// sub-state and its absence lowers it.
pub fn parse_in_call_presence_state(
    element: &Element,
) -> Result<InCallPresenceState, InCallParseError> {
    if element.name() != IN_CALL_NAME || element.ns() != NS_WADDLE_IN_CALL {
        return Err(InCallParseError::NotInCall);
    }
    let has_marker = |name: &str| {
        element
            .children()
            .any(|child| child.name() == name && child.ns() == NS_WADDLE_IN_CALL)
    };
    Ok(InCallPresenceState {
        hand_raised: has_marker(HAND_RAISED_NAME),
        muted: has_marker(MUTED_NAME),
    })
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
