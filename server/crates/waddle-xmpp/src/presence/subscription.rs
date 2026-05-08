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

use jid::BareJid;
use tracing::debug;
use xmpp_parsers::presence::Presence;

use crate::roster::{AskType, RosterItem, Subscription};
use crate::XmppError;
pub use waddle_xmpp_core::presence::subscription::{
    build_subscription_presence, build_unavailable_presence, PendingSubscription, PresenceAction,
    PresenceSubscriptionRequest, SubscriptionType,
};

/// Parse a presence stanza and determine if it's subscription-related.
pub fn parse_subscription_presence(
    pres: &Presence,
    sender_jid: &BareJid,
) -> Result<PresenceAction, XmppError> {
    waddle_xmpp_core::presence::subscription::parse_subscription_presence(pres, sender_jid)
        .map_err(Into::into)
}

/// Build an available presence stanza for broadcasting to subscribers.
pub fn build_available_presence(
    from: &jid::FullJid,
    to: &BareJid,
    show: Option<&str>,
    status: Option<&str>,
    priority: i8,
) -> Presence {
    let mut pres = waddle_xmpp_core::presence::subscription::build_available_presence(
        from, to, show, status, priority,
    );
    crate::xep::ensure_caps_payload(&mut pres.payloads);
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

#[cfg(test)]
mod tests;
