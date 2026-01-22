//! RFC 6121 Presence Subscription flow implementation.
//!
//! This module implements the presence subscription workflow as defined in RFC 6121.
//! It handles subscription requests (subscribe, subscribed, unsubscribe, unsubscribed)
//! and manages the subscription state machine.
//!
//! ## Subscription Flow
//!
//! 1. User A sends `<presence type='subscribe' to='userB@domain'/>` to request subscription
//! 2. Server routes to User B, who can approve or deny
//! 3. User B sends `<presence type='subscribed' to='userA@domain'/>` to approve
//! 4. Server updates roster state for both users
//! 5. Server sends roster push to all connected resources
//!
//! ## Subscription States
//!
//! - `none`: No subscription in either direction
//! - `to`: User receives contact's presence
//! - `from`: Contact receives user's presence
//! - `both`: Mutual subscription
//!
//! ## State Transitions
//!
//! On outbound `subscribe`:
//! - none → none (ask="subscribe")
//! - from → from (ask="subscribe")
//!
//! On inbound `subscribed`:
//! - none (ask="subscribe") → to
//! - from (ask="subscribe") → both
//!
//! On outbound `subscribed`:
//! - none → from
//! - to → both
//!
//! On outbound `unsubscribe`:
//! - to → none
//! - both → from
//!
//! On inbound `unsubscribed`:
//! - to → none
//! - both → from

use jid::{BareJid, Jid};
use serde::{Deserialize, Serialize};
use tracing::debug;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::roster::{AskType, RosterItem, Subscription};
use crate::XmppError;

/// Presence subscription stanza type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionType {
    /// Request to subscribe to another user's presence.
    Subscribe,
    /// Approval of a subscription request.
    Subscribed,
    /// Request to unsubscribe from another user's presence.
    Unsubscribe,
    /// Notification that subscription has been revoked.
    Unsubscribed,
}

impl SubscriptionType {
    /// Convert from xmpp_parsers presence type.
    pub fn from_presence_type(ptype: &PresenceType) -> Option<Self> {
        match ptype {
            PresenceType::Subscribe => Some(SubscriptionType::Subscribe),
            PresenceType::Subscribed => Some(SubscriptionType::Subscribed),
            PresenceType::Unsubscribe => Some(SubscriptionType::Unsubscribe),
            PresenceType::Unsubscribed => Some(SubscriptionType::Unsubscribed),
            _ => None,
        }
    }

    /// Convert to xmpp_parsers presence type.
    pub fn to_presence_type(&self) -> PresenceType {
        match self {
            SubscriptionType::Subscribe => PresenceType::Subscribe,
            SubscriptionType::Subscribed => PresenceType::Subscribed,
            SubscriptionType::Unsubscribe => PresenceType::Unsubscribe,
            SubscriptionType::Unsubscribed => PresenceType::Unsubscribed,
        }
    }

    /// Get the string representation for the type attribute.
    pub fn as_str(&self) -> &'static str {
        match self {
            SubscriptionType::Subscribe => "subscribe",
            SubscriptionType::Subscribed => "subscribed",
            SubscriptionType::Unsubscribe => "unsubscribe",
            SubscriptionType::Unsubscribed => "unsubscribed",
        }
    }
}

/// A parsed presence subscription request.
#[derive(Debug, Clone)]
pub struct PresenceSubscriptionRequest {
    /// The type of subscription request.
    pub subscription_type: SubscriptionType,
    /// The sender's JID (from the connection, not the stanza).
    pub from: BareJid,
    /// The target JID of the subscription request.
    pub to: BareJid,
    /// Optional status message (for subscribe requests).
    pub status: Option<String>,
    /// Stanza ID for tracking.
    pub id: Option<String>,
}

impl PresenceSubscriptionRequest {
    /// Create a new subscription request.
    pub fn new(
        subscription_type: SubscriptionType,
        from: BareJid,
        to: BareJid,
    ) -> Self {
        Self {
            subscription_type,
            from,
            to,
            status: None,
            id: None,
        }
    }

    /// Add a status message.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// Add a stanza ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// Result of parsing a presence stanza for subscription handling.
#[derive(Debug)]
pub enum PresenceAction {
    /// This is a subscription-related presence stanza.
    Subscription(PresenceSubscriptionRequest),
    /// This is a regular presence update (available, unavailable, etc.).
    PresenceUpdate(Presence),
    /// This is a presence probe request.
    Probe { from: BareJid, to: BareJid },
}

/// Parse a presence stanza and determine if it's subscription-related.
///
/// Returns `PresenceAction::Subscription` for subscribe/subscribed/unsubscribe/unsubscribed,
/// `PresenceAction::Probe` for probe requests, or `PresenceAction::PresenceUpdate` for
/// regular presence updates.
pub fn parse_subscription_presence(
    pres: &Presence,
    sender_jid: &BareJid,
) -> Result<PresenceAction, XmppError> {
    // Check if this is a subscription-related presence
    if let Some(ref ptype) = pres.type_ {
        // Check for probe first
        if matches!(ptype, PresenceType::Probe) {
            let to = pres.to.as_ref()
                .ok_or_else(|| XmppError::bad_request(Some("Probe presence must have 'to' attribute".to_string())))?;
            let to_bare = match to.clone().try_into_full() {
                Ok(full) => full.to_bare(),
                Err(bare) => bare,
            };
            return Ok(PresenceAction::Probe {
                from: sender_jid.clone(),
                to: to_bare,
            });
        }

        // Check for subscription types
        if let Some(sub_type) = SubscriptionType::from_presence_type(ptype) {
            // Extract the target JID
            let to = pres.to.as_ref()
                .ok_or_else(|| XmppError::bad_request(Some("Subscription presence must have 'to' attribute".to_string())))?;

            let to_bare = match to.clone().try_into_full() {
                Ok(full) => full.to_bare(),
                Err(bare) => bare,
            };

            // Extract status message if present (get first status value)
            let status = pres.statuses.values().next().cloned();

            let request = PresenceSubscriptionRequest {
                subscription_type: sub_type,
                from: sender_jid.clone(),
                to: to_bare,
                status,
                id: pres.id.clone(),
            };

            debug!(
                subscription_type = ?sub_type,
                from = %sender_jid,
                to = %request.to,
                "Parsed subscription presence"
            );

            return Ok(PresenceAction::Subscription(request));
        }
    }

    // Not subscription-related, treat as regular presence
    Ok(PresenceAction::PresenceUpdate(pres.clone()))
}

/// Build a subscription presence stanza.
pub fn build_subscription_presence(
    subscription_type: SubscriptionType,
    from: &BareJid,
    to: &BareJid,
    status: Option<&str>,
) -> Presence {
    let mut pres = Presence::new(subscription_type.to_presence_type());
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));

    if let Some(status_text) = status {
        pres.statuses.insert(String::new(), status_text.to_string());
    }

    pres
}

/// Build an unavailable presence stanza for broadcasting to subscribers.
pub fn build_unavailable_presence(
    from: &BareJid,
    to: &BareJid,
) -> Presence {
    let mut pres = Presence::new(PresenceType::Unavailable);
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));
    pres
}

/// Build an available presence stanza for broadcasting to subscribers.
pub fn build_available_presence(
    from: &jid::FullJid,
    to: &BareJid,
    show: Option<&str>,
    status: Option<&str>,
    priority: i8,
) -> Presence {
    let mut pres = Presence::new(PresenceType::None);
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));
    pres.priority = priority;

    if let Some(show_str) = show {
        pres.show = match show_str {
            "away" => Some(xmpp_parsers::presence::Show::Away),
            "chat" => Some(xmpp_parsers::presence::Show::Chat),
            "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
            "xa" => Some(xmpp_parsers::presence::Show::Xa),
            _ => None,
        };
    }

    if let Some(status_text) = status {
        pres.statuses.insert(String::new(), status_text.to_string());
    }

    pres
}

/// Subscription state machine for managing state transitions.
///
/// This follows the state transition rules from RFC 6121 Section 3.
#[derive(Debug, Clone)]
pub struct SubscriptionStateMachine;

impl SubscriptionStateMachine {
    /// Apply an outbound subscribe request.
    ///
    /// When user sends subscribe to contact:
    /// - Sets ask="subscribe" on the roster item
    pub fn apply_outbound_subscribe(item: &mut RosterItem) {
        item.ask = Some(AskType::Subscribe);
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            ask = "subscribe",
            "Applied outbound subscribe"
        );
    }

    /// Apply an inbound subscribed response.
    ///
    /// When contact approves our subscription request:
    /// - none (ask=subscribe) → to
    /// - from (ask=subscribe) → both
    /// - Clears ask state
    pub fn apply_inbound_subscribed(item: &mut RosterItem) {
        let new_subscription = match item.subscription {
            Subscription::None => Subscription::To,
            Subscription::From => Subscription::Both,
            // Already subscribed, no change
            Subscription::To => Subscription::To,
            Subscription::Both => Subscription::Both,
            Subscription::Remove => Subscription::To,
        };
        item.subscription = new_subscription;
        item.ask = None;
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Applied inbound subscribed"
        );
    }

    /// Apply an inbound unsubscribed response.
    ///
    /// When contact revokes our subscription:
    /// - to → none
    /// - both → from
    /// - Clears ask state
    pub fn apply_inbound_unsubscribed(item: &mut RosterItem) {
        let new_subscription = match item.subscription {
            Subscription::To => Subscription::None,
            Subscription::Both => Subscription::From,
            // Not subscribed to them anyway
            Subscription::None => Subscription::None,
            Subscription::From => Subscription::From,
            Subscription::Remove => Subscription::None,
        };
        item.subscription = new_subscription;
        item.ask = None;
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Applied inbound unsubscribed"
        );
    }

    /// Apply an outbound subscribed response.
    ///
    /// When user approves contact's subscription request:
    /// - none → from
    /// - to → both
    pub fn apply_outbound_subscribed(item: &mut RosterItem) {
        let new_subscription = match item.subscription {
            Subscription::None => Subscription::From,
            Subscription::To => Subscription::Both,
            // Already from, no change
            Subscription::From => Subscription::From,
            Subscription::Both => Subscription::Both,
            Subscription::Remove => Subscription::From,
        };
        item.subscription = new_subscription;
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Applied outbound subscribed"
        );
    }

    /// Apply an outbound unsubscribed response.
    ///
    /// When user revokes contact's subscription:
    /// - from → none
    /// - both → to
    pub fn apply_outbound_unsubscribed(item: &mut RosterItem) {
        let new_subscription = match item.subscription {
            Subscription::From => Subscription::None,
            Subscription::Both => Subscription::To,
            // Not from anyway
            Subscription::None => Subscription::None,
            Subscription::To => Subscription::To,
            Subscription::Remove => Subscription::None,
        };
        item.subscription = new_subscription;
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Applied outbound unsubscribed"
        );
    }

    /// Apply an outbound unsubscribe request.
    ///
    /// When user sends unsubscribe to contact:
    /// - to → none
    /// - both → from
    pub fn apply_outbound_unsubscribe(item: &mut RosterItem) {
        let new_subscription = match item.subscription {
            Subscription::To => Subscription::None,
            Subscription::Both => Subscription::From,
            // Not subscribed anyway
            Subscription::None => Subscription::None,
            Subscription::From => Subscription::From,
            Subscription::Remove => Subscription::None,
        };
        item.subscription = new_subscription;
        item.ask = None;
        debug!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Applied outbound unsubscribe"
        );
    }

    /// Check if user should receive contact's presence.
    ///
    /// Returns true if subscription is 'to' or 'both'.
    pub fn should_receive_presence(subscription: Subscription) -> bool {
        matches!(subscription, Subscription::To | Subscription::Both)
    }

    /// Check if user should send presence to contact.
    ///
    /// Returns true if subscription is 'from' or 'both'.
    pub fn should_send_presence(subscription: Subscription) -> bool {
        matches!(subscription, Subscription::From | Subscription::Both)
    }
}

/// Storage for pending inbound subscription requests.
///
/// When a user receives a subscription request, it's stored here until
/// the user approves or denies it. This enables offline handling of
/// subscription requests.
#[derive(Debug, Clone)]
pub struct PendingSubscription {
    /// JID of the user requesting subscription.
    pub from: BareJid,
    /// Optional status/reason for the request.
    pub status: Option<String>,
    /// Timestamp when the request was received.
    pub received_at: chrono::DateTime<chrono::Utc>,
}

impl PendingSubscription {
    /// Create a new pending subscription.
    pub fn new(from: BareJid) -> Self {
        Self {
            from,
            status: None,
            received_at: chrono::Utc::now(),
        }
    }

    /// Create from a subscription request.
    pub fn from_request(request: &PresenceSubscriptionRequest) -> Self {
        Self {
            from: request.from.clone(),
            status: request.status.clone(),
            received_at: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(jid: &str, subscription: Subscription, ask: Option<AskType>) -> RosterItem {
        let mut item = RosterItem::new(jid.parse().unwrap());
        item.subscription = subscription;
        item.ask = ask;
        item
    }

    #[test]
    fn test_subscription_type_from_presence_type() {
        assert_eq!(
            SubscriptionType::from_presence_type(&PresenceType::Subscribe),
            Some(SubscriptionType::Subscribe)
        );
        assert_eq!(
            SubscriptionType::from_presence_type(&PresenceType::Subscribed),
            Some(SubscriptionType::Subscribed)
        );
        assert_eq!(
            SubscriptionType::from_presence_type(&PresenceType::Unsubscribe),
            Some(SubscriptionType::Unsubscribe)
        );
        assert_eq!(
            SubscriptionType::from_presence_type(&PresenceType::Unsubscribed),
            Some(SubscriptionType::Unsubscribed)
        );
        assert_eq!(
            SubscriptionType::from_presence_type(&PresenceType::Unavailable),
            None
        );
    }

    #[test]
    fn test_outbound_subscribe_sets_ask() {
        let mut item = make_item("contact@example.com", Subscription::None, None);
        SubscriptionStateMachine::apply_outbound_subscribe(&mut item);
        assert_eq!(item.ask, Some(AskType::Subscribe));
        assert_eq!(item.subscription, Subscription::None);
    }

    #[test]
    fn test_inbound_subscribed_none_to_to() {
        let mut item = make_item("contact@example.com", Subscription::None, Some(AskType::Subscribe));
        SubscriptionStateMachine::apply_inbound_subscribed(&mut item);
        assert_eq!(item.subscription, Subscription::To);
        assert_eq!(item.ask, None);
    }

    #[test]
    fn test_inbound_subscribed_from_to_both() {
        let mut item = make_item("contact@example.com", Subscription::From, Some(AskType::Subscribe));
        SubscriptionStateMachine::apply_inbound_subscribed(&mut item);
        assert_eq!(item.subscription, Subscription::Both);
        assert_eq!(item.ask, None);
    }

    #[test]
    fn test_outbound_subscribed_none_to_from() {
        let mut item = make_item("contact@example.com", Subscription::None, None);
        SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
        assert_eq!(item.subscription, Subscription::From);
    }

    #[test]
    fn test_outbound_subscribed_to_to_both() {
        let mut item = make_item("contact@example.com", Subscription::To, None);
        SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
        assert_eq!(item.subscription, Subscription::Both);
    }

    #[test]
    fn test_inbound_unsubscribed_to_to_none() {
        let mut item = make_item("contact@example.com", Subscription::To, None);
        SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
        assert_eq!(item.subscription, Subscription::None);
    }

    #[test]
    fn test_inbound_unsubscribed_both_to_from() {
        let mut item = make_item("contact@example.com", Subscription::Both, None);
        SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
        assert_eq!(item.subscription, Subscription::From);
    }

    #[test]
    fn test_outbound_unsubscribed_from_to_none() {
        let mut item = make_item("contact@example.com", Subscription::From, None);
        SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
        assert_eq!(item.subscription, Subscription::None);
    }

    #[test]
    fn test_outbound_unsubscribed_both_to_to() {
        let mut item = make_item("contact@example.com", Subscription::Both, None);
        SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
        assert_eq!(item.subscription, Subscription::To);
    }

    #[test]
    fn test_outbound_unsubscribe_to_to_none() {
        let mut item = make_item("contact@example.com", Subscription::To, None);
        SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
        assert_eq!(item.subscription, Subscription::None);
    }

    #[test]
    fn test_outbound_unsubscribe_both_to_from() {
        let mut item = make_item("contact@example.com", Subscription::Both, None);
        SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
        assert_eq!(item.subscription, Subscription::From);
    }

    #[test]
    fn test_should_receive_presence() {
        assert!(!SubscriptionStateMachine::should_receive_presence(Subscription::None));
        assert!(SubscriptionStateMachine::should_receive_presence(Subscription::To));
        assert!(!SubscriptionStateMachine::should_receive_presence(Subscription::From));
        assert!(SubscriptionStateMachine::should_receive_presence(Subscription::Both));
    }

    #[test]
    fn test_should_send_presence() {
        assert!(!SubscriptionStateMachine::should_send_presence(Subscription::None));
        assert!(!SubscriptionStateMachine::should_send_presence(Subscription::To));
        assert!(SubscriptionStateMachine::should_send_presence(Subscription::From));
        assert!(SubscriptionStateMachine::should_send_presence(Subscription::Both));
    }

    #[test]
    fn test_build_subscription_presence() {
        let from: BareJid = "user@example.com".parse().unwrap();
        let to: BareJid = "contact@example.com".parse().unwrap();

        let pres = build_subscription_presence(
            SubscriptionType::Subscribe,
            &from,
            &to,
            Some("Please add me"),
        );

        assert_eq!(pres.type_, Some(PresenceType::Subscribe));
        assert_eq!(pres.from, Some(Jid::from(from)));
        assert_eq!(pres.to, Some(Jid::from(to)));
        assert_eq!(pres.statuses.values().next(), Some(&"Please add me".to_string()));
    }

    #[test]
    fn test_parse_subscription_presence() {
        let sender: BareJid = "user@example.com".parse().unwrap();
        let target: BareJid = "contact@example.com".parse().unwrap();

        let mut pres = Presence::new(PresenceType::Subscribe);
        pres.to = Some(Jid::from(target.clone()));
        pres.statuses.insert(String::new(), "Hello".to_string());

        let action = parse_subscription_presence(&pres, &sender).unwrap();

        match action {
            PresenceAction::Subscription(req) => {
                assert_eq!(req.subscription_type, SubscriptionType::Subscribe);
                assert_eq!(req.from, sender);
                assert_eq!(req.to, target);
                assert_eq!(req.status, Some("Hello".to_string()));
            }
            _ => panic!("Expected Subscription action"),
        }
    }

    #[test]
    fn test_parse_probe_presence() {
        let sender: BareJid = "user@example.com".parse().unwrap();
        let target: BareJid = "contact@example.com".parse().unwrap();

        let mut pres = Presence::new(PresenceType::Probe);
        pres.to = Some(Jid::Bare(target.clone()));

        let action = parse_subscription_presence(&pres, &sender).unwrap();

        match action {
            PresenceAction::Probe { from, to } => {
                assert_eq!(from, sender);
                assert_eq!(to, target);
            }
            _ => panic!("Expected Probe action"),
        }
    }

    #[test]
    fn test_parse_regular_presence() {
        let sender: BareJid = "user@example.com".parse().unwrap();

        // Available presence (no type)
        let pres = Presence::new(PresenceType::None);
        let action = parse_subscription_presence(&pres, &sender).unwrap();

        match action {
            PresenceAction::PresenceUpdate(_) => {}
            _ => panic!("Expected PresenceUpdate action"),
        }
    }

    #[test]
    fn test_pending_subscription() {
        let from: BareJid = "contact@example.com".parse().unwrap();
        let pending = PendingSubscription::new(from.clone());

        assert_eq!(pending.from, from);
        assert!(pending.status.is_none());
    }
}
