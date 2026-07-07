//! Shared RFC 6121 presence subscription parsing/building helpers.

use jid::{BareJid, FullJid, Jid};
use serde::{Deserialize, Serialize};
use tracing::debug;
use xmpp_parsers::presence::{Presence, Show as XmppShow, Type as PresenceType};

use crate::CoreError;

/// Presence subscription stanza type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionType {
    Subscribe,
    Subscribed,
    Unsubscribe,
    Unsubscribed,
}

impl SubscriptionType {
    pub fn from_presence_type(ptype: &PresenceType) -> Option<Self> {
        match ptype {
            PresenceType::Subscribe => Some(SubscriptionType::Subscribe),
            PresenceType::Subscribed => Some(SubscriptionType::Subscribed),
            PresenceType::Unsubscribe => Some(SubscriptionType::Unsubscribe),
            PresenceType::Unsubscribed => Some(SubscriptionType::Unsubscribed),
            _ => None,
        }
    }

    pub fn to_presence_type(&self) -> PresenceType {
        match self {
            SubscriptionType::Subscribe => PresenceType::Subscribe,
            SubscriptionType::Subscribed => PresenceType::Subscribed,
            SubscriptionType::Unsubscribe => PresenceType::Unsubscribe,
            SubscriptionType::Unsubscribed => PresenceType::Unsubscribed,
        }
    }

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
    pub subscription_type: SubscriptionType,
    pub from: BareJid,
    pub to: BareJid,
    pub status: Option<String>,
    pub id: Option<String>,
    pub payloads: Vec<minidom::Element>,
}

impl PresenceSubscriptionRequest {
    pub fn new(subscription_type: SubscriptionType, from: BareJid, to: BareJid) -> Self {
        Self {
            subscription_type,
            from,
            to,
            status: None,
            id: None,
            payloads: Vec::new(),
        }
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// Result of parsing a presence stanza for subscription handling.
#[derive(Debug)]
pub enum PresenceAction {
    Subscription(PresenceSubscriptionRequest),
    PresenceUpdate(Presence),
    Probe {
        from: BareJid,
        to: BareJid,
        to_was_full: bool,
    },
}

/// Parse a presence stanza and determine if it's subscription-related.
pub fn parse_subscription_presence(
    pres: &Presence,
    sender_jid: &BareJid,
) -> Result<PresenceAction, CoreError> {
    if matches!(pres.type_, PresenceType::Probe) {
        let to = pres.to.as_ref().ok_or_else(|| {
            CoreError::bad_request(Some("Probe presence must have 'to' attribute".to_string()))
        })?;
        let (to_bare, to_was_full) = match to.clone().try_into_full() {
            Ok(full) => (full.to_bare(), true),
            Err(bare) => (bare, false),
        };
        return Ok(PresenceAction::Probe {
            from: sender_jid.clone(),
            to: to_bare,
            to_was_full,
        });
    }

    if let Some(sub_type) = SubscriptionType::from_presence_type(&pres.type_) {
        let to = pres.to.as_ref().ok_or_else(|| {
            CoreError::bad_request(Some(
                "Subscription presence must have 'to' attribute".to_string(),
            ))
        })?;

        let to_bare = match to.clone().try_into_full() {
            Ok(full) => full.to_bare(),
            Err(bare) => bare,
        };

        let request = PresenceSubscriptionRequest {
            subscription_type: sub_type,
            from: sender_jid.clone(),
            to: to_bare,
            status: pres.statuses.values().next().cloned(),
            id: pres.id.clone(),
            payloads: pres.payloads.clone(),
        };

        debug!(
            subscription_type = ?sub_type,
            from = %sender_jid,
            to = %request.to,
            "Parsed subscription presence"
        );

        return Ok(PresenceAction::Subscription(request));
    }

    // Only the normal available (no type attribute) and unavailable forms are
    // broadcastable presence updates. Anything else that reaches this point
    // (i.e. type="error") must never be relayed to subscribers — the caller
    // logs and drops it without answering (RFC 6120 §8.3.1: never respond to
    // an error with an error).
    match pres.type_ {
        PresenceType::None | PresenceType::Unavailable => {
            Ok(PresenceAction::PresenceUpdate(pres.clone()))
        }
        ref other => Err(CoreError::bad_request(Some(format!(
            "presence of type {other:?} is not a broadcastable presence update"
        )))),
    }
}

/// Build a subscription presence stanza.
pub fn build_subscription_presence(
    subscription_type: SubscriptionType,
    from: &BareJid,
    to: &BareJid,
    status: Option<&str>,
    payloads: &[minidom::Element],
) -> Presence {
    let mut pres = Presence::new(subscription_type.to_presence_type());
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));

    if let Some(status_text) = status {
        pres.statuses
            .insert(xmpp_parsers::message::Lang::new(), status_text.to_string());
    }

    pres.payloads.extend(payloads.iter().cloned());
    pres
}

/// Build an unavailable presence stanza for broadcasting to subscribers.
pub fn build_unavailable_presence(from: &BareJid, to: &BareJid) -> Presence {
    let mut pres = Presence::new(PresenceType::Unavailable);
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));
    pres
}

/// Build an available presence stanza for broadcasting to subscribers.
pub fn build_available_presence(
    from: &FullJid,
    to: &BareJid,
    show: Option<&str>,
    status: Option<&str>,
    priority: i8,
) -> Presence {
    let mut pres = Presence::new(PresenceType::None).with_priority(priority);
    pres.from = Some(Jid::from(from.clone()));
    pres.to = Some(Jid::from(to.clone()));

    if let Some(show_str) = show {
        pres.show = match show_str {
            "away" => Some(XmppShow::Away),
            "chat" => Some(XmppShow::Chat),
            "dnd" => Some(XmppShow::Dnd),
            "xa" => Some(XmppShow::Xa),
            _ => None,
        };
    }

    if let Some(status_text) = status {
        pres.statuses
            .insert(xmpp_parsers::message::Lang::new(), status_text.to_string());
    }

    pres
}

/// Storage for pending inbound subscription requests.
#[derive(Debug, Clone)]
pub struct PendingSubscription {
    pub from: BareJid,
    pub status: Option<String>,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

impl PendingSubscription {
    pub fn new(from: BareJid) -> Self {
        Self {
            from,
            status: None,
            received_at: chrono::Utc::now(),
        }
    }

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
    fn test_build_subscription_presence() {
        let from: BareJid = "user@example.com".parse().unwrap();
        let to: BareJid = "contact@example.com".parse().unwrap();

        let pres = build_subscription_presence(
            SubscriptionType::Subscribe,
            &from,
            &to,
            Some("Please add me"),
            &[],
        );

        assert_eq!(pres.type_, PresenceType::Subscribe);
        assert_eq!(pres.from, Some(Jid::from(from)));
        assert_eq!(pres.to, Some(Jid::from(to)));
        assert_eq!(
            pres.statuses.values().next(),
            Some(&"Please add me".to_string())
        );
    }

    #[test]
    fn test_build_available_presence() {
        let from: FullJid = "user@example.com/resource".parse().unwrap();
        let to: BareJid = "contact@example.com".parse().unwrap();

        let pres = build_available_presence(&from, &to, Some("chat"), Some("Ready"), 5);

        assert_eq!(pres.from, Some(Jid::from(from)));
        assert_eq!(pres.to, Some(Jid::from(to)));
        assert_eq!(pres.show, Some(XmppShow::Chat));
        assert_eq!(
            pres.priority,
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None)
                .with_priority(5)
                .priority
        );
        assert_eq!(pres.statuses.values().next(), Some(&"Ready".to_string()));
    }

    #[test]
    fn test_parse_subscription_presence() {
        let sender: BareJid = "user@example.com".parse().unwrap();
        let target: BareJid = "contact@example.com".parse().unwrap();

        let mut pres = Presence::new(PresenceType::Subscribe);
        pres.to = Some(Jid::from(target.clone()));
        pres.statuses
            .insert(xmpp_parsers::message::Lang::new(), "Hello".to_string());

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
        pres.to = Some(Jid::from(target.clone()));

        let action = parse_subscription_presence(&pres, &sender).unwrap();

        match action {
            PresenceAction::Probe {
                from,
                to,
                to_was_full,
            } => {
                assert_eq!(from, sender);
                assert_eq!(to, target);
                assert!(!to_was_full);
            }
            _ => panic!("Expected Probe action"),
        }
    }

    #[test]
    fn test_parse_probe_presence_full_jid() {
        let sender: BareJid = "user@example.com".parse().unwrap();
        let target: FullJid = "contact@example.com/resource".parse().unwrap();

        let mut pres = Presence::new(PresenceType::Probe);
        pres.to = Some(Jid::from(target.clone()));

        let action = parse_subscription_presence(&pres, &sender).unwrap();

        match action {
            PresenceAction::Probe {
                from,
                to,
                to_was_full,
            } => {
                assert_eq!(from, sender);
                assert_eq!(to, target.to_bare());
                assert!(to_was_full);
            }
            _ => panic!("Expected Probe action"),
        }
    }

    #[test]
    fn test_parse_regular_presence() {
        let sender: BareJid = "user@example.com".parse().unwrap();
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
