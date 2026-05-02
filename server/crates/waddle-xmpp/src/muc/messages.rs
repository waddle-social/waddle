//! MUC Message Types
//!
//! Types for handling MUC groupchat message routing and broadcasting.

use jid::{BareJid, FullJid, Jid};
use tracing::debug;
use xmpp_parsers::message::{Message, MessageType, Subject};

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
        self.message.bodies.iter().next().map(|b| b.1 .0.as_str())
    }

    /// Get the message ID.
    pub fn id(&self) -> Option<&str> {
        self.message.id.as_deref()
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
        self.message.subjects.iter().next().map(|s| s.1 .0.as_str())
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
/// For accurate checks, use MucRoomRegistry::is_muc_jid().
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
/// `from='room/setter_nick'` (per the §7.2.15 example), `<subject>{text}</subject>`
/// (empty when `text == ""` — a wire-distinguishable "cleared" marker
/// per §7.2.15's SHOULD-include-`<delay/>`-on-cleared rule), an
/// XEP-0203 `<delay from='room' stamp='set_at'/>` (delay's `from`
/// MUST be the room itself per §7.2.15), and an XEP-0421
/// `<occupant-id/>` derived from `setter`'s bare JID.
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
            msg.subjects.insert(String::new(), Subject(String::new()));
        }
        Some(state) => {
            let from = room_jid
                .clone()
                .with_resource_str(&state.setter_nick)
                .map(Jid::from)
                .unwrap_or_else(|_| Jid::from(room_jid.clone()));
            msg.from = Some(from);
            msg.subjects
                .insert(String::new(), Subject(state.text.clone()));
            xep0203::add_delay_stamp(&mut msg, state.set_at, &room_jid.to_string());
            let occupant_id = generate_occupant_id(&state.setter, room_jid, secret);
            set_occupant_id_on_message(&mut msg, &occupant_id);
        }
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Body;

    fn make_groupchat_message(to: &str, body: &str) -> Message {
        let bare_jid: BareJid = to.parse().unwrap();
        let mut msg = Message::new(Some(Jid::from(bare_jid)));
        msg.type_ = MessageType::Groupchat;
        msg.id = Some("msg-1".to_string());
        msg.bodies.insert(String::new(), Body(body.to_string()));
        msg
    }

    #[test]
    fn test_muc_message_from_groupchat() {
        let msg = make_groupchat_message("room@muc.example.com", "Hello!");
        let sender: FullJid = "user@example.com/resource".parse().unwrap();

        let muc_msg = MucMessage::from_message(msg, sender.clone()).unwrap();

        assert_eq!(muc_msg.room_jid.to_string(), "room@muc.example.com");
        assert_eq!(muc_msg.sender_jid, sender);
        assert!(muc_msg.has_body());
        assert_eq!(muc_msg.body_text(), Some("Hello!"));
    }

    #[test]
    fn test_muc_message_rejects_non_groupchat() {
        let mut msg = make_groupchat_message("room@muc.example.com", "Hello!");
        msg.type_ = MessageType::Chat; // Wrong type!

        let sender: FullJid = "user@example.com/resource".parse().unwrap();
        let result = MucMessage::from_message(msg, sender);

        assert!(result.is_err());
    }

    #[test]
    fn test_muc_message_rejects_missing_to() {
        let mut msg = Message::new(None::<Jid>);
        msg.type_ = MessageType::Groupchat;

        let sender: FullJid = "user@example.com/resource".parse().unwrap();
        let result = MucMessage::from_message(msg, sender);

        assert!(result.is_err());
    }

    #[test]
    fn test_is_muc_groupchat() {
        let groupchat = make_groupchat_message("room@muc.example.com", "Hello!");
        assert!(is_muc_groupchat(&groupchat));

        let bare_jid: BareJid = "user@example.com".parse().unwrap();
        let mut chat = Message::new(Some(Jid::from(bare_jid)));
        chat.type_ = MessageType::Chat;
        assert!(!is_muc_groupchat(&chat));
    }

    #[test]
    fn test_looks_like_muc_jid() {
        let muc_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let conf_jid: BareJid = "room@conference.example.com".parse().unwrap();
        let user_jid: BareJid = "user@example.com".parse().unwrap();

        assert!(looks_like_muc_jid(&muc_jid));
        assert!(looks_like_muc_jid(&conf_jid));
        assert!(!looks_like_muc_jid(&user_jid));
    }

    #[test]
    fn test_create_broadcast_message() {
        let original = make_groupchat_message("room@muc.example.com", "Hello!");
        let from: FullJid = "room@muc.example.com/sender_nick".parse().unwrap();
        let to: FullJid = "user@example.com/resource".parse().unwrap();

        let broadcast = create_broadcast_message(&original, from.clone(), to.clone());

        assert_eq!(broadcast.type_, MessageType::Groupchat);
        assert_eq!(broadcast.from, Some(Jid::from(from)));
        assert_eq!(broadcast.to, Some(Jid::from(to)));
        assert_eq!(broadcast.id, Some("msg-1".to_string()));
    }

    #[test]
    fn test_message_route_result() {
        let success = MessageRouteResult::success(vec![]);
        assert!(success.success);
        assert!(success.error.is_none());

        let failure = MessageRouteResult::failure("Room not found");
        assert!(!failure.success);
        assert_eq!(failure.error, Some("Room not found".to_string()));
    }

    // ── XEP-0045 §7.2.15 join-time subject emission ─────────────────────

    use crate::xep::xep0203::{extract_delay_from_message, has_delay};
    use crate::xep::xep0421::{extract_occupant_id_from_message, generate_occupant_id};
    use chrono::TimeZone;

    fn test_room() -> BareJid {
        "team@muc.example.com".parse().expect("valid bare jid")
    }
    fn test_recipient() -> FullJid {
        "joiner@example.com/web".parse().expect("valid full jid")
    }
    fn test_secret() -> OccupantIdSecret {
        OccupantIdSecret::for_testing(b"subject-builder-test-secret".to_vec())
    }
    fn sample_state(text: &str) -> SubjectState {
        SubjectState {
            text: text.to_string(),
            setter: "alice@example.com".parse().expect("valid bare jid"),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn build_subject_message_set_state_produces_section_7_2_15_shape() {
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();
        let state = sample_state("Fire Burn and Cauldron Bubble!");

        let msg = build_subject_message(&room, &to, Some(&state), &secret);

        assert_eq!(msg.type_, MessageType::Groupchat);
        assert_eq!(
            msg.from.as_ref().map(|j| j.to_string()),
            Some("team@muc.example.com/alice-nick".to_string())
        );
        assert_eq!(msg.to.as_ref().map(|j| j.to_string()), Some(to.to_string()));
        assert_eq!(msg.subjects.len(), 1, "exactly one <subject/> element");
        assert_eq!(
            msg.subjects.iter().next().map(|s| s.1 .0.as_str()),
            Some("Fire Burn and Cauldron Bubble!")
        );
        assert!(msg.bodies.is_empty(), "subject message has no <body/>");
        assert!(has_delay(&msg), "<delay/> SHOULD be present (§7.2.15)");
        assert!(
            extract_occupant_id_from_message(&msg).is_some(),
            "XEP-0421 occupant-id MUST be stamped"
        );
    }

    #[test]
    fn build_subject_message_cleared_state_emits_empty_subject_with_delay() {
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();
        let state = sample_state("");

        let msg = build_subject_message(&room, &to, Some(&state), &secret);

        assert_eq!(
            msg.subjects.iter().next().map(|s| s.1 .0.as_str()),
            Some(""),
            "explicitly cleared subject is empty <subject/>"
        );
        assert!(
            has_delay(&msg),
            "<delay/> SHOULD be included for actively-cleared subjects (§7.2.15)"
        );
        assert!(
            extract_occupant_id_from_message(&msg).is_some(),
            "occupant-id stamped because we know the user who cleared it"
        );
    }

    #[test]
    fn build_subject_message_never_set_emits_empty_subject_without_delay() {
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();

        let msg = build_subject_message(&room, &to, None, &secret);

        assert_eq!(msg.type_, MessageType::Groupchat);
        assert_eq!(
            msg.from.as_ref().map(|j| j.to_string()),
            Some("team@muc.example.com".to_string()),
            "never-set rooms emit bare-from (§7.2.15 allows this; no setter exists)"
        );
        assert_eq!(
            msg.subjects.iter().next().map(|s| s.1 .0.as_str()),
            Some(""),
            "MUST return an empty <subject/> (§7.2.15)"
        );
        assert!(
            !has_delay(&msg),
            "<delay/> MAY be omitted when the subject was never set (§7.2.15)"
        );
        assert!(
            extract_occupant_id_from_message(&msg).is_none(),
            "no setter means no input for the XEP-0421 HMAC; omitted, matching established servers"
        );
    }

    #[test]
    fn build_subject_message_delay_from_attribute_is_room_jid_not_setter() {
        // §7.2.15 conditional MUST: "If the <delay/> element is included,
        // its 'from' attribute MUST be set to the JID of the room itself."
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();
        let state = sample_state("hello");

        let msg = build_subject_message(&room, &to, Some(&state), &secret);
        let delay = extract_delay_from_message(&msg).expect("<delay/> present");

        assert_eq!(
            delay.from.as_deref(),
            Some("team@muc.example.com"),
            "delay.from MUST be the room JID"
        );
        assert_ne!(
            delay.from.as_deref(),
            Some("team@muc.example.com/alice-nick"),
            "delay.from MUST NOT be the setter's room/nick"
        );
    }

    #[test]
    fn build_subject_message_occupant_id_is_hmac_of_setter_bare_jid() {
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();
        let state = sample_state("hello");

        let msg = build_subject_message(&room, &to, Some(&state), &secret);

        let id = extract_occupant_id_from_message(&msg).expect("occupant-id stamped");
        let expected = generate_occupant_id(&state.setter, &room, &secret);
        assert_eq!(id, expected);
    }

    #[test]
    fn build_subject_message_delay_stamp_round_trips_as_xep_0082_datetime() {
        // XEP-0203 + XEP-0082: stamp MUST be a valid dateTime; the
        // round-trip through chrono confirms our `to_rfc3339()` output
        // is parseable by any conforming consumer.
        let room = test_room();
        let to = test_recipient();
        let secret = test_secret();
        let state = sample_state("hello");
        let original_stamp = state.set_at;

        let msg = build_subject_message(&room, &to, Some(&state), &secret);
        let delay = extract_delay_from_message(&msg).expect("<delay/> present");

        assert_eq!(delay.stamp, original_stamp);
    }
}
