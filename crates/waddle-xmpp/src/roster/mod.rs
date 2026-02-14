//! RFC 6121 Roster Management implementation.
//!
//! The roster (contact list) is a key feature of XMPP instant messaging.
//! This module implements:
//!
//! - Roster get: Retrieve the user's contact list
//! - Roster set: Add, update, or remove contacts
//! - Roster push: Notify connected resources when roster changes
//!
//! ## Protocol Overview
//!
//! Roster get:
//! ```xml
//! <iq type='get' id='roster-1'>
//!   <query xmlns='jabber:iq:roster'/>
//! </iq>
//! ```
//!
//! Roster set (add/update):
//! ```xml
//! <iq type='set' id='roster-2'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' name='Contact Name'>
//!       <group>Friends</group>
//!     </item>
//!   </query>
//! </iq>
//! ```
//!
//! Roster set (remove):
//! ```xml
//! <iq type='set' id='roster-3'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' subscription='remove'/>
//!   </query>
//! </iq>
//! ```
//!
//! Roster push (server to client):
//! ```xml
//! <iq type='set' id='push-1'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' subscription='both'/>
//!   </query>
//! </iq>
//! ```

pub mod storage;

pub use storage::RosterStorage;

use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::XmppError;

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
    pub fn from_element(elem: &Element) -> Result<Self, XmppError> {
        // JID is required
        let jid_str = elem.attr("jid").ok_or_else(|| {
            XmppError::bad_request(Some("Roster item missing 'jid' attribute".to_string()))
        })?;

        let jid: BareJid = jid_str.parse().map_err(|e| {
            XmppError::bad_request(Some(format!("Invalid JID '{}': {}", jid_str, e)))
        })?;

        // Name is optional
        let name = elem.attr("name").map(|s| s.to_string());

        // Subscription is optional, defaults to None
        let subscription = elem
            .attr("subscription")
            .map(Subscription::from_str)
            .transpose()?
            .unwrap_or(Subscription::None);

        // Ask is optional
        let ask = elem.attr("ask").map(AskType::from_str).transpose()?;

        // Groups are child elements. RFC 6121 does not allow empty or duplicate
        // group names within a roster item.
        let mut groups = Vec::new();
        let mut seen_groups = HashSet::new();
        for group_elem in elem
            .children()
            .filter(|c| c.name() == "group" && c.ns() == ROSTER_NS)
        {
            let group = group_elem.text();
            if group.trim().is_empty() {
                return Err(XmppError::not_acceptable(Some(
                    "Roster group name must not be empty".to_string(),
                )));
            }
            if !seen_groups.insert(group.clone()) {
                return Err(XmppError::bad_request(Some(
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
    /// The user will receive presence updates from the contact.
    To,
    /// Contact is subscribed to user's presence.
    /// The user will send presence updates to the contact.
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

    /// Parse a subscription state from a string.
    pub fn from_str(s: &str) -> Result<Self, XmppError> {
        match s {
            "none" => Ok(Subscription::None),
            "to" => Ok(Subscription::To),
            "from" => Ok(Subscription::From),
            "both" => Ok(Subscription::Both),
            "remove" => Ok(Subscription::Remove),
            _ => Err(XmppError::bad_request(Some(format!(
                "Invalid subscription state: {}",
                s
            )))),
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

/// Ask type for pending subscription requests.
///
/// Per RFC 6121, only "subscribe" is valid for the ask attribute.
/// This indicates that the user has sent a subscription request to
/// the contact but has not yet received approval.
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

    /// Parse an ask type from a string.
    pub fn from_str(s: &str) -> Result<Self, XmppError> {
        match s {
            "subscribe" => Ok(AskType::Subscribe),
            _ => Err(XmppError::bad_request(Some(format!(
                "Invalid ask type: {}",
                s
            )))),
        }
    }
}

impl fmt::Display for AskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Parsed roster query.
#[derive(Debug, Clone)]
pub struct RosterQuery {
    /// Optional version for roster versioning (XEP-0237).
    pub ver: Option<String>,
    /// Items in the query (for set operations).
    pub items: Vec<RosterItem>,
}

impl RosterQuery {
    /// Create an empty roster query (for get operations).
    pub fn empty() -> Self {
        Self {
            ver: None,
            items: Vec::new(),
        }
    }

    /// Create a roster query with items (for set operations).
    pub fn with_items(items: Vec<RosterItem>) -> Self {
        Self { ver: None, items }
    }
}

/// Check if an IQ is a roster get request.
///
/// Returns true if the IQ contains:
/// ```xml
/// <iq type='get'>
///   <query xmlns='jabber:iq:roster'/>
/// </iq>
/// ```
pub fn is_roster_get(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => elem.name() == "query" && elem.ns() == ROSTER_NS,
        _ => false,
    }
}

/// Check if an IQ is a roster set request.
///
/// Returns true if the IQ contains:
/// ```xml
/// <iq type='set'>
///   <query xmlns='jabber:iq:roster'>
///     <item .../>
///   </query>
/// </iq>
/// ```
pub fn is_roster_set(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "query" && elem.ns() == ROSTER_NS,
        _ => false,
    }
}

/// Parse a roster get query from an IQ stanza.
pub fn parse_roster_get(iq: &Iq) -> Result<RosterQuery, XmppError> {
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "query" && elem.ns() == ROSTER_NS {
                elem
            } else {
                return Err(XmppError::bad_request(Some(
                    "Missing roster query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "Roster get must be IQ type='get'".to_string(),
            )));
        }
    };

    let ver = query_elem.attr("ver").map(|s| s.to_string());

    debug!(ver = ?ver, "Parsed roster get query");

    Ok(RosterQuery {
        ver,
        items: Vec::new(),
    })
}

/// Parse a roster set query from an IQ stanza.
///
/// Extracts the roster item(s) from the query. Per RFC 6121,
/// a roster set should contain exactly one item element.
pub fn parse_roster_set(iq: &Iq) -> Result<RosterQuery, XmppError> {
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => {
            if elem.name() == "query" && elem.ns() == ROSTER_NS {
                elem
            } else {
                return Err(XmppError::bad_request(Some(
                    "Missing roster query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "Roster set must be IQ type='set'".to_string(),
            )));
        }
    };

    let ver = query_elem.attr("ver").map(|s| s.to_string());

    // Parse item elements. For roster set operations, client-provided
    // subscription/ask values are ignored except for subscription='remove'
    // per RFC 6121 Section 2.1.2.
    let items: Result<Vec<RosterItem>, XmppError> = query_elem
        .children()
        .filter(|c| c.name() == "item" && c.ns() == ROSTER_NS)
        .map(parse_roster_set_item)
        .collect();

    let items = items?;

    // Per RFC 6121, roster set should have exactly one item
    if items.is_empty() {
        return Err(XmppError::bad_request(Some(
            "Roster set must contain at least one item".to_string(),
        )));
    }

    debug!(
        item_count = items.len(),
        jid = %items[0].jid,
        subscription = %items[0].subscription,
        "Parsed roster set query"
    );

    Ok(RosterQuery { ver, items })
}

/// Parse a roster set item per RFC 6121 semantics.
///
/// Client-provided `subscription` and `ask` values are server-controlled and
/// therefore ignored, except for `subscription='remove'` which requests item
/// deletion.
fn parse_roster_set_item(elem: &Element) -> Result<RosterItem, XmppError> {
    let jid_str = elem.attr("jid").ok_or_else(|| {
        XmppError::bad_request(Some("Roster item missing 'jid' attribute".to_string()))
    })?;

    let jid: BareJid = jid_str
        .parse()
        .map_err(|e| XmppError::bad_request(Some(format!("Invalid JID '{}': {}", jid_str, e))))?;

    let name = elem.attr("name").map(|s| s.to_string());

    let subscription = match elem.attr("subscription") {
        Some("remove") => Subscription::Remove,
        _ => Subscription::None,
    };

    let mut groups = Vec::new();
    let mut seen_groups = HashSet::new();
    for group_elem in elem
        .children()
        .filter(|c| c.name() == "group" && c.ns() == ROSTER_NS)
    {
        let group = group_elem.text();
        if group.trim().is_empty() {
            return Err(XmppError::not_acceptable(Some(
                "Roster group name must not be empty".to_string(),
            )));
        }
        if !seen_groups.insert(group.clone()) {
            return Err(XmppError::bad_request(Some(
                "Roster group names must be unique".to_string(),
            )));
        }
        groups.push(group);
    }

    Ok(RosterItem {
        jid,
        name,
        subscription,
        ask: None,
        groups,
    })
}

/// Build a roster result IQ response.
///
/// Returns the user's roster with all contact items:
/// ```xml
/// <iq type='result' id='...'>
///   <query xmlns='jabber:iq:roster'>
///     <item jid='...' name='...' subscription='...'/>
///     ...
///   </query>
/// </iq>
/// ```
pub fn build_roster_result(original_iq: &Iq, items: &[RosterItem], ver: Option<&str>) -> Iq {
    let mut query_builder = Element::builder("query", ROSTER_NS);

    // Add version if roster versioning is supported
    if let Some(v) = ver {
        query_builder = query_builder.attr("ver", v);
    }

    // Add all roster items
    for item in items {
        query_builder = query_builder.append(item.to_element());
    }

    let query = query_builder.build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(query)),
    }
}

/// Build an empty roster result IQ response.
///
/// Used when acknowledging a roster set:
/// ```xml
/// <iq type='result' id='...'/>
/// ```
pub fn build_roster_result_empty(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

/// Build a roster push IQ.
///
/// Per RFC 6121, the server sends roster push to all connected
/// resources when the roster changes:
/// ```xml
/// <iq type='set' id='push-1' to='user@domain/resource'>
///   <query xmlns='jabber:iq:roster' ver='...'>
///     <item jid='...' subscription='...'/>
///   </query>
/// </iq>
/// ```
pub fn build_roster_push(
    push_id: &str,
    to_jid: &str,
    item: &RosterItem,
    ver: Option<&str>,
) -> Result<Iq, jid::Error> {
    let mut query_builder = Element::builder("query", ROSTER_NS);

    if let Some(v) = ver {
        query_builder = query_builder.attr("ver", v);
    }

    query_builder = query_builder.append(item.to_element());

    let query = query_builder.build();

    Ok(Iq {
        from: None, // Server to client, no from
        to: Some(to_jid.parse()?),
        id: push_id.to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query),
    })
}

/// Result of a roster set operation.
#[derive(Debug, Clone)]
pub enum RosterSetResult {
    /// Item was added to the roster.
    Added(RosterItem),
    /// Item was updated in the roster.
    Updated(RosterItem),
    /// Item was removed from the roster.
    Removed(BareJid),
}

impl RosterSetResult {
    /// Get the JID of the affected item.
    pub fn jid(&self) -> &BareJid {
        match self {
            RosterSetResult::Added(item) => &item.jid,
            RosterSetResult::Updated(item) => &item.jid,
            RosterSetResult::Removed(jid) => jid,
        }
    }

    /// Get the roster item for push notifications.
    ///
    /// For removals, creates an item with subscription="remove".
    pub fn to_push_item(&self) -> RosterItem {
        match self {
            RosterSetResult::Added(item) | RosterSetResult::Updated(item) => item.clone(),
            RosterSetResult::Removed(jid) => {
                RosterItem::new(jid.clone()).set_subscription(Subscription::Remove)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roster_item_new() {
        let jid: BareJid = "contact@example.com".parse().unwrap();
        let item = RosterItem::new(jid.clone());

        assert_eq!(item.jid, jid);
        assert_eq!(item.name, None);
        assert_eq!(item.subscription, Subscription::None);
        assert_eq!(item.ask, None);
        assert!(item.groups.is_empty());
    }

    #[test]
    fn test_roster_item_with_name() {
        let jid: BareJid = "contact@example.com".parse().unwrap();
        let item = RosterItem::with_name(jid.clone(), "My Contact");

        assert_eq!(item.jid, jid);
        assert_eq!(item.name, Some("My Contact".to_string()));
    }

    #[test]
    fn test_roster_item_builder() {
        let jid: BareJid = "contact@example.com".parse().unwrap();
        let item = RosterItem::new(jid.clone())
            .set_subscription(Subscription::Both)
            .set_ask(Some(AskType::Subscribe))
            .add_group("Friends")
            .add_group("Work");

        assert_eq!(item.subscription, Subscription::Both);
        assert_eq!(item.ask, Some(AskType::Subscribe));
        assert_eq!(item.groups, vec!["Friends", "Work"]);
    }

    #[test]
    fn test_subscription_from_str() {
        assert_eq!(Subscription::from_str("none").unwrap(), Subscription::None);
        assert_eq!(Subscription::from_str("to").unwrap(), Subscription::To);
        assert_eq!(Subscription::from_str("from").unwrap(), Subscription::From);
        assert_eq!(Subscription::from_str("both").unwrap(), Subscription::Both);
        assert_eq!(
            Subscription::from_str("remove").unwrap(),
            Subscription::Remove
        );
        assert!(Subscription::from_str("invalid").is_err());
    }

    #[test]
    fn test_subscription_as_str() {
        assert_eq!(Subscription::None.as_str(), "none");
        assert_eq!(Subscription::To.as_str(), "to");
        assert_eq!(Subscription::From.as_str(), "from");
        assert_eq!(Subscription::Both.as_str(), "both");
        assert_eq!(Subscription::Remove.as_str(), "remove");
    }

    #[test]
    fn test_ask_type_from_str() {
        assert_eq!(AskType::from_str("subscribe").unwrap(), AskType::Subscribe);
        assert!(AskType::from_str("invalid").is_err());
    }

    #[test]
    fn test_roster_item_to_element() {
        let jid: BareJid = "contact@example.com".parse().unwrap();
        let item = RosterItem::with_name(jid, "Alice")
            .set_subscription(Subscription::Both)
            .add_group("Friends");

        let elem = item.to_element();

        assert_eq!(elem.name(), "item");
        assert_eq!(elem.ns(), ROSTER_NS);
        assert_eq!(elem.attr("jid"), Some("contact@example.com"));
        assert_eq!(elem.attr("name"), Some("Alice"));
        assert_eq!(elem.attr("subscription"), Some("both"));

        let groups: Vec<_> = elem.children().filter(|c| c.name() == "group").collect();
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn test_roster_item_from_element() {
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
    fn test_roster_item_from_element_minimal() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("jid", "contact@example.com")
            .build();

        let item = RosterItem::from_element(&elem).unwrap();

        assert_eq!(item.jid.to_string(), "contact@example.com");
        assert_eq!(item.name, None);
        assert_eq!(item.subscription, Subscription::None);
        assert_eq!(item.ask, None);
        assert!(item.groups.is_empty());
    }

    #[test]
    fn test_roster_item_from_element_missing_jid() {
        let elem = Element::builder("item", ROSTER_NS)
            .attr("name", "Alice")
            .build();

        let result = RosterItem::from_element(&elem);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_roster_get() {
        let query_elem = Element::builder("query", ROSTER_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(is_roster_get(&iq));
    }

    #[test]
    fn test_is_not_roster_get_wrong_ns() {
        let query_elem = Element::builder("query", "wrong:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(!is_roster_get(&iq));
    }

    #[test]
    fn test_is_not_roster_get_wrong_type() {
        let query_elem = Element::builder("query", ROSTER_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        assert!(!is_roster_get(&iq));
    }

    #[test]
    fn test_is_roster_set() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        assert!(is_roster_set(&iq));
    }

    #[test]
    fn test_parse_roster_get() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .attr("ver", "abc123")
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let query = parse_roster_get(&iq).unwrap();
        assert_eq!(query.ver, Some("abc123".to_string()));
        assert!(query.items.is_empty());
    }

    #[test]
    fn test_parse_roster_set() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .attr("name", "Alice")
                    .attr("subscription", "both")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let query = parse_roster_set(&iq).unwrap();
        assert_eq!(query.items.len(), 1);
        assert_eq!(query.items[0].jid.to_string(), "contact@example.com");
        assert_eq!(query.items[0].name, Some("Alice".to_string()));
        assert_eq!(query.items[0].subscription, Subscription::None);
    }

    #[test]
    fn test_parse_roster_set_remove() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .attr("subscription", "remove")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let query = parse_roster_set(&iq).unwrap();
        assert_eq!(query.items.len(), 1);
        assert!(query.items[0].subscription.is_remove());
    }

    #[test]
    fn test_parse_roster_set_invalid_subscription_ignored() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .attr("subscription", "foobar")
                    .build(),
            )
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let query = parse_roster_set(&iq).unwrap();
        assert_eq!(query.items.len(), 1);
        assert_eq!(query.items[0].subscription, Subscription::None);
    }

    #[test]
    fn test_parse_roster_set_empty_items() {
        let query_elem = Element::builder("query", ROSTER_NS).build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let result = parse_roster_set(&iq);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_roster_set_duplicate_groups_rejected() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .append(
                        Element::builder("group", ROSTER_NS)
                            .append("Friends")
                            .build(),
                    )
                    .append(
                        Element::builder("group", ROSTER_NS)
                            .append("Friends")
                            .build(),
                    )
                    .build(),
            )
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let result = parse_roster_set(&iq);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_roster_set_empty_group_rejected() {
        let query_elem = Element::builder("query", ROSTER_NS)
            .append(
                Element::builder("item", ROSTER_NS)
                    .attr("jid", "contact@example.com")
                    .append(Element::builder("group", ROSTER_NS).append("").build())
                    .build(),
            )
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let result = parse_roster_set(&iq);
        assert!(matches!(
            result,
            Err(crate::XmppError::Stanza {
                condition: crate::error::StanzaErrorCondition::NotAcceptable,
                ..
            })
        ));
    }

    #[test]
    fn test_build_roster_result() {
        let query_elem = Element::builder("query", ROSTER_NS).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let items = vec![
            RosterItem::with_name("contact1@example.com".parse().unwrap(), "Alice")
                .set_subscription(Subscription::Both),
            RosterItem::with_name("contact2@example.com".parse().unwrap(), "Bob")
                .set_subscription(Subscription::To),
        ];

        let response = build_roster_result(&original_iq, &items, Some("ver123"));

        assert_eq!(response.id, "roster-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));

        if let xmpp_parsers::iq::IqType::Result(Some(elem)) = response.payload {
            assert_eq!(elem.attr("ver"), Some("ver123"));
            let item_count = elem.children().filter(|c| c.name() == "item").count();
            assert_eq!(item_count, 2);
        }
    }

    #[test]
    fn test_build_roster_result_empty() {
        let query_elem = Element::builder("query", ROSTER_NS).build();
        let original_iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "roster-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        let response = build_roster_result_empty(&original_iq);

        assert_eq!(response.id, "roster-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
    }

    #[test]
    fn test_build_roster_push() {
        let item = RosterItem::with_name("contact@example.com".parse().unwrap(), "Alice")
            .set_subscription(Subscription::Both);

        let push = build_roster_push("push-1", "user@example.com/resource", &item, Some("ver456"))
            .expect("valid JID should succeed");

        assert_eq!(push.id, "push-1");
        assert_eq!(
            push.to.as_ref().unwrap().to_string(),
            "user@example.com/resource"
        );
        assert!(push.from.is_none());

        if let xmpp_parsers::iq::IqType::Set(elem) = push.payload {
            assert_eq!(elem.name(), "query");
            assert_eq!(elem.ns(), ROSTER_NS);
            assert_eq!(elem.attr("ver"), Some("ver456"));

            let items: Vec<_> = elem.children().filter(|c| c.name() == "item").collect();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].attr("jid"), Some("contact@example.com"));
        } else {
            panic!("Expected Set payload");
        }
    }

    #[test]
    fn test_roster_set_result_to_push_item() {
        let jid: BareJid = "contact@example.com".parse().unwrap();

        // Added
        let added = RosterSetResult::Added(RosterItem::with_name(jid.clone(), "Alice"));
        let push_item = added.to_push_item();
        assert_eq!(push_item.name, Some("Alice".to_string()));

        // Removed
        let removed = RosterSetResult::Removed(jid.clone());
        let push_item = removed.to_push_item();
        assert_eq!(push_item.subscription, Subscription::Remove);
    }

    #[test]
    fn test_build_roster_push_invalid_jid() {
        let item = RosterItem::with_name("contact@example.com".parse().unwrap(), "Alice")
            .set_subscription(Subscription::Both);

        let result = build_roster_push("push-1", "@", &item, None);
        assert!(result.is_err(), "Empty-local JID should return Err");
    }
}
