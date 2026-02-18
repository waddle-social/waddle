//! XEP-0352: Client State Indication
//!
//! Allows clients to indicate their state (active or inactive) to the server.
//! This enables the server to optimize traffic by potentially throttling
//! or batching stanzas for inactive clients.
//!
//! ## Overview
//!
//! Mobile clients can inform the server when they go to the background
//! (inactive) or return to the foreground (active). The server can use
//! this information to:
//! - Reduce battery usage by batching stanzas for inactive clients
//! - Delay non-urgent presence updates
//! - Optimize push notification behavior
//!
//! ## XML Format
//!
//! ```xml
//! <!-- Client indicates it is now inactive -->
//! <inactive xmlns='urn:xmpp:csi:0'/>
//!
//! <!-- Client indicates it is now active -->
//! <active xmlns='urn:xmpp:csi:0'/>
//! ```
//!
//! ## Stream Features
//!
//! The server advertises CSI support in stream features:
//! ```xml
//! <stream:features>
//!   <csi xmlns='urn:xmpp:csi:0'/>
//! </stream:features>
//! ```
//!
//! ## Stanza Buffering
//!
//! When the client is inactive, the server buffers non-urgent stanzas:
//! - **Buffered**: Presence updates (except errors), chat state notifications,
//!   PubSub events
//! - **Delivered immediately**: Direct messages with body content, MUC messages
//!   that mention the user, IQ stanzas, error stanzas

use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0352 Client State Indication.
pub const NS_CSI: &str = "urn:xmpp:csi:0";

/// Client state as indicated by the client.
///
/// Mobile clients typically send `<inactive/>` when backgrounded
/// and `<active/>` when foregrounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientState {
    /// Client is active (foreground, user interacting).
    /// This is the default state for new connections.
    #[default]
    Active,
    /// Client is inactive (background, user not interacting).
    Inactive,
}

impl ClientState {
    /// Returns true if the client is in the active state.
    pub fn is_active(&self) -> bool {
        matches!(self, ClientState::Active)
    }

    /// Returns true if the client is in the inactive state.
    pub fn is_inactive(&self) -> bool {
        matches!(self, ClientState::Inactive)
    }
}

/// Check if an XML element is a CSI `<active/>` stanza.
///
/// Returns true if the element is `<active xmlns='urn:xmpp:csi:0'/>`.
pub fn is_csi_active(elem: &Element) -> bool {
    elem.name() == "active" && elem.ns() == NS_CSI
}

/// Check if an XML element is a CSI `<inactive/>` stanza.
///
/// Returns true if the element is `<inactive xmlns='urn:xmpp:csi:0'/>`.
pub fn is_csi_inactive(elem: &Element) -> bool {
    elem.name() == "inactive" && elem.ns() == NS_CSI
}

/// Check if raw XML data contains a CSI active indication.
///
/// This is useful for quick checks before full parsing.
pub fn data_contains_csi_active(data: &str) -> bool {
    data.contains("<active") && data.contains(NS_CSI)
}

/// Check if raw XML data contains a CSI inactive indication.
///
/// This is useful for quick checks before full parsing.
pub fn data_contains_csi_inactive(data: &str) -> bool {
    data.contains("<inactive") && data.contains(NS_CSI)
}

/// Build the CSI stream feature advertisement.
///
/// Returns the XML string for the CSI feature to be included
/// in the server's stream features.
pub fn build_csi_feature() -> String {
    format!("<csi xmlns='{}'/>", NS_CSI)
}

/// Maximum number of stanzas to buffer when client is inactive.
///
/// This prevents memory issues if a client stays inactive for a long time.
pub const MAX_CSI_BUFFER_SIZE: usize = 100;

/// Urgency level for stanzas when determining CSI buffering behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaUrgency {
    /// Urgent stanzas must be delivered immediately (direct messages, mentions, IQs).
    Urgent,
    /// Non-urgent stanzas can be buffered (presence updates, chat states).
    NonUrgent,
}

impl StanzaUrgency {
    /// Returns true if this stanza is urgent and should bypass buffering.
    pub fn is_urgent(&self) -> bool {
        matches!(self, StanzaUrgency::Urgent)
    }

    /// Returns true if this stanza can be buffered.
    pub fn can_buffer(&self) -> bool {
        matches!(self, StanzaUrgency::NonUrgent)
    }
}

/// Classify a message's urgency for CSI buffering.
///
/// Messages are considered urgent (not buffered) if:
/// - They have a body with content (direct messages or MUC messages)
/// - They are error messages
///
/// Messages that can be buffered:
/// - Chat state notifications (composing, paused, etc.) without body
/// - Receipt confirmations without body
/// - Other body-less messages
///
/// Note: MUC mention detection requires the user's nickname and is handled
/// separately by `is_muc_mention`.
pub fn classify_message_urgency(msg: &Message) -> StanzaUrgency {
    // Error messages are always urgent
    if msg.type_ == MessageType::Error {
        return StanzaUrgency::Urgent;
    }

    // Messages with body content are urgent (direct messages, MUC messages)
    if msg.bodies.values().any(|b| !b.0.is_empty()) {
        return StanzaUrgency::Urgent;
    }

    // Body-less messages (chat states, receipts) can be buffered
    StanzaUrgency::NonUrgent
}

/// Classify a presence stanza's urgency for CSI buffering.
///
/// Presence stanzas are generally non-urgent and can be buffered, except:
/// - Error presences (must be delivered immediately)
/// - Subscription requests (need user attention)
pub fn classify_presence_urgency(pres: &Presence) -> StanzaUrgency {
    use xmpp_parsers::presence::Type;

    match pres.type_ {
        // Error presences must be delivered immediately
        Type::Error => StanzaUrgency::Urgent,
        // Subscription requests/responses need user attention
        Type::Subscribe | Type::Subscribed | Type::Unsubscribe | Type::Unsubscribed => {
            StanzaUrgency::Urgent
        }
        // Regular presence updates (available, unavailable, probe) can be buffered
        Type::None | Type::Unavailable | Type::Probe => StanzaUrgency::NonUrgent,
    }
}

/// Check if a MUC message mentions the given nickname.
///
/// Performs a case-insensitive search for the nickname in any of the
/// message bodies. This is used to ensure mentioned users receive
/// notifications even when inactive.
///
/// # Arguments
///
/// * `msg` - The message to check
/// * `nickname` - The user's MUC nickname to search for
///
/// # Returns
///
/// `true` if the nickname is mentioned in any body, `false` otherwise
pub fn is_muc_mention(msg: &Message, nickname: &str) -> bool {
    // Only check groupchat messages
    if msg.type_ != MessageType::Groupchat {
        return false;
    }

    let nickname_lower = nickname.to_lowercase();

    // Check each body for the nickname
    for body in msg.bodies.values() {
        let body_lower = body.0.to_lowercase();
        // Check for common mention patterns:
        // - Direct mention (nickname at word boundary)
        // - @mention style
        if contains_mention(&body_lower, &nickname_lower) {
            return true;
        }
    }

    false
}

/// Check if text contains a mention of the given name.
///
/// Looks for the name as a word boundary (not part of another word).
fn contains_mention(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // Check for @mention style
    let at_mention = format!("@{}", name);
    if text.contains(&at_mention) {
        return true;
    }

    // Check for direct mention at word boundary
    // We look for the name followed by non-alphanumeric or end of string
    let mut search_start = 0;
    while let Some(pos) = text[search_start..].find(name) {
        let actual_pos = search_start + pos;

        // Check if this is at a word boundary (start of text or preceded by non-alphanumeric)
        let at_word_start = actual_pos == 0
            || text[..actual_pos]
                .chars()
                .next_back()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true);

        // Check if this is at a word end (end of text or followed by non-alphanumeric)
        let end_pos = actual_pos + name.len();
        let at_word_end = end_pos >= text.len()
            || text[end_pos..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true);

        if at_word_start && at_word_end {
            return true;
        }

        search_start = actual_pos + 1;
        if search_start >= text.len() {
            break;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_state_default() {
        let state = ClientState::default();
        assert!(state.is_active());
        assert!(!state.is_inactive());
    }

    #[test]
    fn test_client_state_inactive() {
        let state = ClientState::Inactive;
        assert!(!state.is_active());
        assert!(state.is_inactive());
    }

    #[test]
    fn test_is_csi_active() {
        let elem = Element::builder("active", NS_CSI).build();
        assert!(is_csi_active(&elem));

        let other = Element::builder("active", "other:ns").build();
        assert!(!is_csi_active(&other));

        let inactive = Element::builder("inactive", NS_CSI).build();
        assert!(!is_csi_active(&inactive));
    }

    #[test]
    fn test_is_csi_inactive() {
        let elem = Element::builder("inactive", NS_CSI).build();
        assert!(is_csi_inactive(&elem));

        let other = Element::builder("inactive", "other:ns").build();
        assert!(!is_csi_inactive(&other));

        let active = Element::builder("active", NS_CSI).build();
        assert!(!is_csi_inactive(&active));
    }

    #[test]
    fn test_data_contains_csi() {
        let active_data = "<active xmlns='urn:xmpp:csi:0'/>";
        assert!(data_contains_csi_active(active_data));
        assert!(!data_contains_csi_inactive(active_data));

        let inactive_data = "<inactive xmlns='urn:xmpp:csi:0'/>";
        assert!(!data_contains_csi_active(inactive_data));
        assert!(data_contains_csi_inactive(inactive_data));
    }

    #[test]
    fn test_build_csi_feature() {
        let feature = build_csi_feature();
        assert!(feature.contains("<csi"));
        assert!(feature.contains(NS_CSI));
    }

    #[test]
    fn test_stanza_urgency() {
        let urgent = StanzaUrgency::Urgent;
        assert!(urgent.is_urgent());
        assert!(!urgent.can_buffer());

        let non_urgent = StanzaUrgency::NonUrgent;
        assert!(!non_urgent.is_urgent());
        assert!(non_urgent.can_buffer());
    }

    #[test]
    fn test_classify_message_urgency_with_body() {
        use jid::Jid;
        use xmpp_parsers::message::Body;

        let to: Jid = "user@example.com".parse().unwrap();
        let mut msg = Message::new(Some(to));
        msg.bodies.insert(String::new(), Body("Hello!".to_string()));

        assert_eq!(classify_message_urgency(&msg), StanzaUrgency::Urgent);
    }

    #[test]
    fn test_classify_message_urgency_without_body() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let msg = Message::new(Some(to));

        assert_eq!(classify_message_urgency(&msg), StanzaUrgency::NonUrgent);
    }

    #[test]
    fn test_classify_message_urgency_empty_body() {
        use jid::Jid;
        use xmpp_parsers::message::Body;

        let to: Jid = "user@example.com".parse().unwrap();
        let mut msg = Message::new(Some(to));
        msg.bodies.insert(String::new(), Body(String::new()));

        assert_eq!(classify_message_urgency(&msg), StanzaUrgency::NonUrgent);
    }

    #[test]
    fn test_classify_message_urgency_error() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let mut msg = Message::new(Some(to));
        msg.type_ = MessageType::Error;

        assert_eq!(classify_message_urgency(&msg), StanzaUrgency::Urgent);
    }

    #[test]
    fn test_classify_presence_urgency_available() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let pres = Presence::new(xmpp_parsers::presence::Type::None).with_to(to);

        assert_eq!(classify_presence_urgency(&pres), StanzaUrgency::NonUrgent);
    }

    #[test]
    fn test_classify_presence_urgency_unavailable() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let pres = Presence::new(xmpp_parsers::presence::Type::Unavailable).with_to(to);

        assert_eq!(classify_presence_urgency(&pres), StanzaUrgency::NonUrgent);
    }

    #[test]
    fn test_classify_presence_urgency_error() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let pres = Presence::new(xmpp_parsers::presence::Type::Error).with_to(to);

        assert_eq!(classify_presence_urgency(&pres), StanzaUrgency::Urgent);
    }

    #[test]
    fn test_classify_presence_urgency_subscribe() {
        use jid::Jid;

        let to: Jid = "user@example.com".parse().unwrap();
        let pres = Presence::new(xmpp_parsers::presence::Type::Subscribe).with_to(to);

        assert_eq!(classify_presence_urgency(&pres), StanzaUrgency::Urgent);
    }

    #[test]
    fn test_contains_mention_at_style() {
        assert!(contains_mention("hey @alice how are you", "alice"));
        assert!(contains_mention("@bob check this out", "bob"));
        assert!(!contains_mention("hey @alice how are you", "bob"));
    }

    #[test]
    fn test_contains_mention_direct() {
        assert!(contains_mention("alice: can you help?", "alice"));
        assert!(contains_mention("hey alice!", "alice"));
        assert!(contains_mention("alice", "alice"));
        // Should not match if part of another word
        assert!(!contains_mention("malice is bad", "alice"));
        assert!(!contains_mention("alicein wonderland", "alice"));
    }

    #[test]
    fn test_contains_mention_case_insensitive() {
        // The function expects already lowercased input
        assert!(contains_mention("hey alice", "alice"));
    }

    #[test]
    fn test_contains_mention_empty_name() {
        assert!(!contains_mention("hello world", ""));
    }

    #[test]
    fn test_is_muc_mention_groupchat() {
        use jid::Jid;
        use xmpp_parsers::message::Body;

        let to: Jid = "room@muc.example.com".parse().unwrap();
        let mut msg = Message::new(Some(to));
        msg.type_ = MessageType::Groupchat;
        msg.bodies
            .insert(String::new(), Body("hey alice, check this!".to_string()));

        assert!(is_muc_mention(&msg, "alice"));
        assert!(is_muc_mention(&msg, "Alice")); // Case insensitive
        assert!(!is_muc_mention(&msg, "bob"));
    }

    #[test]
    fn test_is_muc_mention_not_groupchat() {
        use jid::Jid;
        use xmpp_parsers::message::Body;

        let to: Jid = "user@example.com".parse().unwrap();
        let mut msg = Message::new(Some(to));
        msg.type_ = MessageType::Chat;
        msg.bodies
            .insert(String::new(), Body("hey alice!".to_string()));

        // Not a groupchat message, so no MUC mention detection
        assert!(!is_muc_mention(&msg, "alice"));
    }
}
