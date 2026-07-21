//! MUC Message Types
//!
//! Types for handling MUC groupchat message routing and broadcasting.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tracing::debug;
use xmpp_parsers::message::{Message, MessageType};

use super::presence::NS_MUC_USER;
use super::room::RoomConfig;
use super::SubjectState;
use crate::xep::xep0085::{self, ChatStateCarrier};
use crate::xep::xep0203;
use crate::xep::xep0421::{generate_occupant_id, set_occupant_id_on_message, OccupantIdSecret};
use crate::XmppError;

/// Represents a parsed MUC message ready for routing.
#[derive(Debug, Clone)]
pub struct MucMessage {
    /// The room this message is destined for (bare JID)
    pub room_jid: BareJid,
    /// The sender's full JID (user@domain/resource)
    pub sender_jid: FullJid,
    /// The original message
    pub message: Message,
}

impl MucMessage {
    /// Create a MUC message from an XMPP message.
    ///
    /// Validates that the message is a groupchat type destined for a MUC room.
    pub fn from_message(msg: Message, sender_jid: FullJid) -> Result<Self, XmppError> {
        // Validate message type is groupchat
        if msg.type_ != MessageType::Groupchat {
            return Err(XmppError::bad_request(Some(
                "Expected groupchat message type".to_string(),
            )));
        }

        // Extract the room JID from the 'to' attribute
        let room_jid = msg
            .to
            .as_ref()
            .ok_or_else(|| {
                XmppError::bad_request(Some("Message missing 'to' attribute".to_string()))
            })?
            .clone();

        // Convert to bare JID (strip resource if present)
        let room_bare_jid = match room_jid.try_into_full() {
            Ok(full) => full.to_bare(),
            Err(bare) => bare,
        };

        debug!(
            room = %room_bare_jid,
            sender = %sender_jid,
            "Parsed MUC message"
        );

        Ok(Self {
            room_jid: room_bare_jid,
            sender_jid,
            message: msg,
        })
    }

    /// Check if this message has a body (text content).
    pub fn has_body(&self) -> bool {
        !self.message.bodies.is_empty()
    }

    /// Get the message body text (first body if multiple languages).
    pub fn body_text(&self) -> Option<&str> {
        self.message.bodies.iter().next().map(|b| b.1.as_str())
    }

    /// Get the message ID.
    pub fn id(&self) -> Option<&str> {
        self.message.id.as_ref().map(|id| id.0.as_str())
    }

    /// Check if this message has a subject element.
    ///
    /// Per XEP-0045, subject changes are messages with a <subject/>
    /// element (and typically no <body/>).
    pub fn has_subject(&self) -> bool {
        !self.message.subjects.is_empty()
    }

    /// Get the subject text (first subject if multiple languages).
    ///
    /// Returns the subject text, or None if no subject is present.
    pub fn subject_text(&self) -> Option<&str> {
        self.message.subjects.iter().next().map(|s| s.1.as_str())
    }

    /// Check if this message is a subject-only message (no body, has subject).
    ///
    /// Per XEP-0045 Section 8.1, subject changes are sent as groupchat
    /// messages with a <subject/> element but no <body/> element.
    pub fn is_subject_change(&self) -> bool {
        self.has_subject() && !self.has_body()
    }

    /// Check if this message carries a chat state notification (XEP-0085).
    pub fn has_chat_state(&self) -> bool {
        self.message.has_chat_state()
    }

    /// Extract the chat state from this MUC message (XEP-0085).
    pub fn chat_state(&self) -> Option<xep0085::ChatState> {
        self.message.chat_state()
    }

    /// Returns `true` if this is a standalone chat state notification (no body).
    ///
    /// These should be broadcast to occupants but not archived.
    pub fn is_chat_state_only(&self) -> bool {
        self.message.is_standalone_chat_state()
    }
}

/// An outbound MUC message to send to an occupant.
#[derive(Debug, Clone)]
pub struct OutboundMucMessage {
    /// The recipient's full JID
    pub to: FullJid,
    /// The message to send
    pub message: Message,
}

impl OutboundMucMessage {
    /// Create a new outbound message.
    pub fn new(to: FullJid, message: Message) -> Self {
        Self { to, message }
    }
}

/// XEP-0045 §10.2.1 configuration-change status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucConfigStatusCode {
    /// 104: non-privacy-related room configuration change.
    NonPrivacyConfigurationChange,
    /// 170: room logging is now enabled.
    LoggingEnabled,
    /// 171: room logging is now disabled.
    LoggingDisabled,
}

impl MucConfigStatusCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonPrivacyConfigurationChange => "104",
            Self::LoggingEnabled => "170",
            Self::LoggingDisabled => "171",
        }
    }
}

/// Diff two room configs into XEP-0045 §10.2.1 notification codes.
pub fn config_change_status_codes(
    previous: &RoomConfig,
    next: &RoomConfig,
) -> Vec<MucConfigStatusCode> {
    let mut codes = Vec::new();

    if previous.enable_logging != next.enable_logging {
        codes.push(if next.enable_logging {
            MucConfigStatusCode::LoggingEnabled
        } else {
            MucConfigStatusCode::LoggingDisabled
        });
    }

    if has_non_privacy_config_change(previous, next) {
        codes.push(MucConfigStatusCode::NonPrivacyConfigurationChange);
    }

    codes
}

fn has_non_privacy_config_change(previous: &RoomConfig, next: &RoomConfig) -> bool {
    previous.name != next.name
        || previous.description != next.description
        || previous.persistent != next.persistent
        || previous.members_only != next.members_only
        || previous.public_room != next.public_room
        || previous.moderated != next.moderated
        || previous.max_occupants != next.max_occupants
        || previous.occupants_may_change_subject != next.occupants_may_change_subject
        || previous.forum != next.forum
        || previous.group_dm != next.group_dm
        || previous.pin_permission != next.pin_permission
        || previous.federation_policy != next.federation_policy
}

/// Build the per-occupant config-change notification message.
pub fn build_config_change_message(
    room_jid: &BareJid,
    to_jid: &FullJid,
    status_codes: &[MucConfigStatusCode],
) -> Message {
    let mut x = Element::builder("x", NS_MUC_USER);
    for code in status_codes {
        x = x.append(
            Element::builder("status", NS_MUC_USER)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), code.as_str())
                .build(),
        );
    }

    let mut message = Message::new(Some(Jid::from(to_jid.clone())));
    message.from = Some(Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.payloads.push(x.build());
    message
}

/// Result of routing a message through a MUC room.
#[derive(Debug)]
pub struct MessageRouteResult {
    /// Messages to send to occupants (including sender echo per XEP-0045)
    pub outbound_messages: Vec<OutboundMucMessage>,
    /// Whether the message was successfully routed
    pub success: bool,
    /// Error message if routing failed
    pub error: Option<String>,
}

impl MessageRouteResult {
    /// Create a successful route result.
    pub fn success(outbound_messages: Vec<OutboundMucMessage>) -> Self {
        Self {
            outbound_messages,
            success: true,
            error: None,
        }
    }

    /// Create a failed route result.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            outbound_messages: Vec::new(),
            success: false,
            error: Some(error.into()),
        }
    }
}

/// Check if a message is a MUC groupchat message.
pub fn is_muc_groupchat(msg: &Message) -> bool {
    msg.type_ == MessageType::Groupchat
}

/// Check if a JID appears to be a MUC room JID.
///
/// This is a heuristic check based on the domain containing "muc." or "conference.".
/// For accurate checks, resolve the JID against the live `RoomRegistryActor`.
pub fn looks_like_muc_jid(jid: &BareJid) -> bool {
    let domain = jid.domain().as_str();
    domain.starts_with("muc.") || domain.starts_with("conference.")
}

/// Create a groupchat message for broadcasting.
///
/// Sets up the message with appropriate attributes for MUC broadcast:
/// - Type set to groupchat
/// - From set to the room JID with sender's nick
/// - Original message ID preserved
pub fn create_broadcast_message(
    original: &Message,
    from_room_jid: FullJid,
    to_occupant: FullJid,
) -> Message {
    let mut broadcast = original.clone();
    broadcast.type_ = MessageType::Groupchat;
    broadcast.from = Some(Jid::from(from_room_jid));
    broadcast.to = Some(Jid::from(to_occupant));
    broadcast
}

/// Build the historical room-subject message delivered on join, per
/// XEP-0045 §7.2.15.
///
/// `state == None` ("never set"): bare-from groupchat message with an
/// empty `<subject/>` element, no `<delay/>`, no `<occupant-id/>`.
/// XEP-0421 §3 nominally requires occupant-id on every emitted
/// message, but the derivation is keyed on the setter's real bare JID
/// (XEP-0421 §3) — for a never-set room there is no setter input and
/// the two MUSTs cannot both be satisfied. Established servers
/// (Prosody, ejabberd, MongooseIM, Openfire) resolve this by
/// satisfying §7.2.15 and omitting occupant-id; we mirror that.
///
/// `state == Some(SubjectState{..})` (set or explicitly cleared): nick-form
/// `from='room/setter_nick'` (per the §7.2.15 example), one
/// `<subject xml:lang='...'>{text}</subject>` per entry in `texts`
/// (every value empty represents the "cleared" variant — wire-
/// distinguishable from never-set per §7.2.15's
/// SHOULD-include-`<delay/>`-on-cleared rule), an XEP-0203
/// `<delay from='room' stamp='set_at'/>` (delay's `from` MUST be the
/// room itself per §7.2.15), and an XEP-0421 `<occupant-id/>` derived
/// from `setter`'s bare JID.
///
/// `setter_nick` is used solely for rendering `from`; the XEP-0421
/// stable identifier is derived from `setter` (bare JID) so it
/// survives nick changes per XEP-0421 §3's stability requirement.
pub fn build_subject_message(
    room_jid: &BareJid,
    to: &FullJid,
    state: Option<&SubjectState>,
    secret: &OccupantIdSecret,
) -> Message {
    let mut msg = Message::new(Some(Jid::from(to.clone())));
    msg.type_ = MessageType::Groupchat;

    match state {
        None => {
            msg.from = Some(Jid::from(room_jid.clone()));
            msg.subjects
                .insert(xmpp_parsers::message::Lang::new(), String::new());
        }
        Some(state) => {
            let from = room_jid
                .clone()
                .with_resource_str(&state.setter_nick)
                .map(Jid::from)
                .unwrap_or_else(|_| Jid::from(room_jid.clone()));
            msg.from = Some(from);
            // Replay every persisted language variant so the
            // join-time stanza matches the live broadcast 1:1. If the
            // persisted map is somehow empty (defensive — the handler
            // captures whatever the originating message had, which
            // §8.1 requires to be at least one `<subject/>`), fall
            // back to a single empty default-language entry so we
            // still satisfy §7.2.15's "MUST return an empty
            // <subject/>" rule.
            if state.texts.is_empty() {
                msg.subjects
                    .insert(xmpp_parsers::message::Lang::new(), String::new());
            } else {
                state.texts.apply_to_message(&mut msg);
            }
            xep0203::add_delay_stamp(&mut msg, state.set_at, &room_jid.to_string());
            let occupant_id = generate_occupant_id(&state.setter, room_jid, secret);
            set_occupant_id_on_message(&mut msg, &occupant_id);
        }
    }

    msg
}

#[cfg(test)]
mod tests;
