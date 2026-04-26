//! RFC 6121 Roster Management shared primitives.
//!
//! This module provides the core roster types used by both the XMPP server
//! and client crates: [`RosterItem`], [`Subscription`], [`AskType`], and XML
//! helpers for building roster IQ stanzas.

use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::{fmt, str::FromStr};

use crate::error::CoreError;

/// Namespace for RFC 6121 Roster Management.
pub const ROSTER_NS: &str = "jabber:iq:roster";

/// A roster item representing a contact in the user's roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterItem {
    /// The contact's JID (bare JID without resource).
    pub jid: BareJid,
    /// Optional human-readable name for the contact.
    pub name: Option<String>,
    /// Current subscription state.
    pub subscription: Subscription,
    /// Pending subscription request (only "subscribe" is valid per RFC 6121).
    pub ask: Option<AskType>,
    /// Contact has been pre-approved for a future subscription request.
    pub approved: bool,
    /// Groups this contact belongs to.
    pub groups: Vec<String>,
}

impl RosterItem {
    /// Create a new roster item with minimal information.
    pub fn new(jid: BareJid) -> Self {
        Self {
            jid,
            name: None,
            subscription: Subscription::None,
            ask: None,
            approved: false,
            groups: Vec::new(),
        }
    }

    /// Create a roster item with a name.
    pub fn with_name(jid: BareJid, name: impl Into<String>) -> Self {
        Self {
            jid,
            name: Some(name.into()),
            subscription: Subscription::None,
            ask: None,
            approved: false,
            groups: Vec::new(),
        }
    }

    /// Set the subscription state.
    pub fn set_subscription(mut self, subscription: Subscription) -> Self {
        self.subscription = subscription;
        self
    }

    /// Set the ask state.
    pub fn set_ask(mut self, ask: Option<AskType>) -> Self {
        self.ask = ask;
        self
    }

    /// Add a group.
    pub fn add_group(mut self, group: impl Into<String>) -> Self {
        self.groups.push(group.into());
        self
    }

    /// Parse a roster item from an XML element.
    pub fn from_element(elem: &Element) -> Result<Self, CoreError> {
        let jid_str = elem.attr("jid").ok_or_else(|| {
            CoreError::bad_request(Some("Roster item missing 'jid' attribute".to_string()))
        })?;

        let jid: BareJid = jid_str.parse().map_err(|e| {
            CoreError::bad_request(Some(format!("Invalid JID '{}': {}", jid_str, e)))
        })?;

        let name = elem.attr("name").map(|s| s.to_string());

        let subscription = elem
            .attr("subscription")
            .map(str::parse::<Subscription>)
            .transpose()?
            .unwrap_or(Subscription::None);

        let ask = elem.attr("ask").map(str::parse::<AskType>).transpose()?;
        let approved = matches!(elem.attr("approved"), Some("true") | Some("1"));

        let mut groups = Vec::new();
        let mut seen_groups = HashSet::new();
        for group_elem in elem
            .children()
            .filter(|c| c.name() == "group" && c.ns() == ROSTER_NS)
        {
            let group = group_elem.text();
            if group.trim().is_empty() {
                return Err(CoreError::not_acceptable(Some(
                    "Roster group name must not be empty".to_string(),
                )));
            }
            if !seen_groups.insert(group.clone()) {
                return Err(CoreError::bad_request(Some(
                    "Roster group names must be unique".to_string(),
                )));
            }
            groups.push(group);
        }

        Ok(Self {
            jid,
            name,
            subscription,
            ask,
            approved,
            groups,
        })
    }

    /// Convert this roster item to an XML element.
    pub fn to_element(&self) -> Element {
        let mut builder = Element::builder("item", ROSTER_NS)
            .attr("jid", self.jid.to_string())
            .attr("subscription", self.subscription.as_str());

        if let Some(ref name) = self.name {
            builder = builder.attr("name", name);
        }

        if let Some(ref ask) = self.ask {
            builder = builder.attr("ask", ask.as_str());
        }

        if self.approved {
            builder = builder.attr("approved", "true");
        }

        for group in &self.groups {
            let group_elem = Element::builder("group", ROSTER_NS)
                .append(group.clone())
                .build();
            builder = builder.append(group_elem);
        }

        builder.build()
    }
}

/// Subscription state for a roster item.
///
/// Per RFC 6121, these are the valid subscription states:
/// - `none`: No subscription exists
/// - `to`: User has subscribed to contact's presence (user receives)
/// - `from`: Contact has subscribed to user's presence (user sends)
/// - `both`: Mutual subscription (bidirectional)
/// - `remove`: Special value to remove an item from the roster
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Subscription {
    /// No subscription exists between user and contact.
    #[default]
    None,
    /// User is subscribed to contact's presence.
    To,
    /// Contact is subscribed to user's presence.
    From,
    /// Mutual subscription - both directions are subscribed.
    Both,
    /// Special value used in roster set to remove an item.
    Remove,
}

impl Subscription {
    /// Get the XML attribute value for this subscription state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Subscription::None => "none",
            Subscription::To => "to",
            Subscription::From => "from",
            Subscription::Both => "both",
            Subscription::Remove => "remove",
        }
    }

    /// Check if this is a removal request.
    pub fn is_remove(&self) -> bool {
        matches!(self, Subscription::Remove)
    }
}

impl fmt::Display for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Subscription {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Subscription::None),
            "to" => Ok(Subscription::To),
            "from" => Ok(Subscription::From),
            "both" => Ok(Subscription::Both),
            "remove" => Ok(Subscription::Remove),
            _ => Err(CoreError::bad_request(Some(format!(
                "Invalid subscription state: {}",
                s
            )))),
        }
    }
}

/// Ask type for pending subscription requests.
///
/// Per RFC 6121, only "subscribe" is valid for the ask attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AskType {
    /// User has sent a subscription request to the contact.
    Subscribe,
}

impl AskType {
    /// Get the XML attribute value for this ask type.
    pub fn as_str(&self) -> &'static str {
        match self {
            AskType::Subscribe => "subscribe",
        }
    }
}

impl fmt::Display for AskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for AskType {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "subscribe" => Ok(AskType::Subscribe),
            _ => Err(CoreError::bad_request(Some(format!(
                "Invalid ask type: {}",
                s
            )))),
        }
    }
}

/// Build a roster get IQ stanza.
///
/// ```xml
/// <iq type='get' id='...'>
///   <query xmlns='jabber:iq:roster'/>
/// </iq>
/// ```
pub fn build_roster_get_iq(id: &str) -> Element {
    let query = Element::builder("query", ROSTER_NS).build();
    Element::builder("iq", "jabber:client")
        .attr("type", "get")
        .attr("id", id)
        .append(query)
        .build()
}

/// Build a roster set IQ stanza for a single item.
///
/// ```xml
/// <iq type='set' id='...'>
///   <query xmlns='jabber:iq:roster'>
///     <item .../>
///   </query>
/// </iq>
/// ```
pub fn build_roster_set_iq(id: &str, item: &RosterItem) -> Element {
    let query = Element::builder("query", ROSTER_NS)
        .append(item.to_element())
        .build();
    Element::builder("iq", "jabber:client")
        .attr("type", "set")
        .attr("id", id)
        .append(query)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roster_item_from_element_happy_path() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("jid", "contact@example.com")
            .attr("name", "Alice")
            .attr("subscription", "both")
            .attr("ask", "subscribe")
            .append(
                Element::builder("group", ROSTER_NS)
                    .append("Friends")
                    .build(),
            )
            .build();

        let item = RosterItem::from_element(&elem).unwrap();

        assert_eq!(item.jid.to_string(), "contact@example.com");
        assert_eq!(item.name, Some("Alice".to_string()));
        assert_eq!(item.subscription, Subscription::Both);
        assert_eq!(item.ask, Some(AskType::Subscribe));
        assert_eq!(item.groups, vec!["Friends".to_string()]);
    }

    #[test]
    fn test_roster_item_from_element_missing_jid() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("name", "Alice")
            .build();

        let result = RosterItem::from_element(&elem);
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::BadRequest(Some(msg)) => assert!(msg.contains("missing 'jid'")),
            e => panic!("unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_roster_item_from_element_invalid_subscription() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("jid", "contact@example.com")
            .attr("subscription", "bogus")
            .build();

        let result = RosterItem::from_element(&elem);
        assert!(result.is_err());
    }

    #[test]
    fn test_roster_item_from_element_invalid_ask() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("jid", "contact@example.com")
            .attr("ask", "bogus")
            .build();

        let result = RosterItem::from_element(&elem);
        assert!(result.is_err());
    }

    #[test]
    fn test_roster_item_to_element_roundtrip() {
        let jid: BareJid = "contact@example.com".parse().unwrap();
        let original = RosterItem::with_name(jid, "Bob")
            .set_subscription(Subscription::To)
            .set_ask(Some(AskType::Subscribe))
            .add_group("Work");

        let elem = original.to_element();
        let parsed = RosterItem::from_element(&elem).unwrap();

        assert_eq!(parsed.jid, original.jid);
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.subscription, original.subscription);
        assert_eq!(parsed.ask, original.ask);
        assert_eq!(parsed.groups, original.groups);
    }

    #[test]
    fn test_subscription_from_str_all_variants() {
        assert_eq!("none".parse::<Subscription>().unwrap(), Subscription::None);
        assert_eq!("to".parse::<Subscription>().unwrap(), Subscription::To);
        assert_eq!("from".parse::<Subscription>().unwrap(), Subscription::From);
        assert_eq!("both".parse::<Subscription>().unwrap(), Subscription::Both);
        assert_eq!(
            "remove".parse::<Subscription>().unwrap(),
            Subscription::Remove
        );
        assert!("invalid".parse::<Subscription>().is_err());
    }

    #[test]
    fn test_ask_type_from_str() {
        assert_eq!("subscribe".parse::<AskType>().unwrap(), AskType::Subscribe);
        assert!("invalid".parse::<AskType>().is_err());
    }

    #[test]
    fn test_build_roster_get_iq() {
        let iq = build_roster_get_iq("r1");
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("id"), Some("r1"));
        let query = iq.children().next().unwrap();
        assert_eq!(query.name(), "query");
        assert_eq!(query.ns(), ROSTER_NS);
    }

    #[test]
    fn test_build_roster_set_iq() {
        let jid: BareJid = "friend@example.com".parse().unwrap();
        let item = RosterItem::with_name(jid, "Friend");
        let iq = build_roster_set_iq("s1", &item);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("id"), Some("s1"));
        let query = iq.children().next().unwrap();
        assert_eq!(query.name(), "query");
        assert_eq!(query.ns(), ROSTER_NS);
        let item_elem = query.children().next().unwrap();
        assert_eq!(item_elem.attr("jid"), Some("friend@example.com"));
    }
}
