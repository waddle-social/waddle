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

/// Server-generated, opaque roster version identifier (XEP-0237 §2.5).
///
/// The wire form is a non-empty string. The value is opaque to the client.
/// Construct only via [`RosterVersion::generate`] (UUID v4) or
/// [`RosterVersion::from_str`] (rejects empty input — the empty-string case
/// on the wire means "client wants to bootstrap" and is modelled by
/// [`RosterVersionRequest::Bootstrap`], not by an empty `RosterVersion`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RosterVersion(String);

impl RosterVersion {
    /// Generate a fresh opaque version identifier.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Borrow the wire form for serialisation at the I/O boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RosterVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RosterVersion {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(CoreError::bad_request(Some(
                "Roster version must not be empty (use RosterVersionRequest::Bootstrap)"
                    .to_string(),
            )));
        }
        Ok(Self(s.to_string()))
    }
}

/// Inbound `ver` attribute on a roster get, classified per XEP-0237 §2.5.
///
/// XEP-0237 distinguishes three cases on the wire:
///
/// | Wire shape | Meaning |
/// | --- | --- |
/// | no `ver` attribute | client does not support roster versioning |
/// | `ver=""` | client supports versioning; cache is absent or corrupt — bootstrap |
/// | `ver="<id>"` | client claims its cache is at `<id>` |
///
/// Collapsing these distinctions on parse loses information the wire carries,
/// so we preserve them at the type level. Per the Waddle typed-payloads rule,
/// untyped input is parsed exactly once and the untyped form is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterVersionRequest {
    /// Inbound query had no `ver` attribute. Client does not support
    /// roster versioning (or chose not to participate this round).
    Absent,
    /// Inbound query had `ver=""`. Client wants to bootstrap; treat as a
    /// stale cache (full roster + fresh `ver` in the response).
    Bootstrap,
    /// Inbound query had `ver="<id>"`. Compare against the user's current
    /// roster version and return either an empty result (matching) or the
    /// full roster with a fresh `ver` (stale).
    Cached(RosterVersion),
}

impl RosterVersionRequest {
    /// Parse an inbound `ver` attribute value, where `None` means the
    /// attribute was absent and `Some(s)` carries its (possibly empty) value.
    pub fn from_attr(attr: Option<&str>) -> Self {
        match attr {
            None => Self::Absent,
            Some("") => Self::Bootstrap,
            // Construct via FromStr so the empty-rejection invariant on
            // RosterVersion holds at every construction site, even though the
            // empty case is already steered to Bootstrap above.
            Some(s) => match s.parse::<RosterVersion>() {
                Ok(v) => Self::Cached(v),
                Err(_) => Self::Bootstrap,
            },
        }
    }

    /// Whether the request signals roster-versioning support at all.
    /// `Absent` is the only case where it does not.
    pub fn signals_support(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Return the cached version, if present.
    pub fn cached(&self) -> Option<&RosterVersion> {
        match self {
            Self::Cached(v) => Some(v),
            _ => None,
        }
    }
}

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
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                self.jid.to_string(),
            )
            .attr(
                minidom::rxml::xml_ncname!("subscription").to_owned(),
                self.subscription.as_str(),
            );

        if let Some(ref name) = self.name {
            builder = builder.attr(minidom::rxml::xml_ncname!("name").to_owned(), name);
        }

        if let Some(ref ask) = self.ask {
            builder = builder.attr(minidom::rxml::xml_ncname!("ask").to_owned(), ask.as_str());
        }

        if self.approved {
            builder = builder.attr(minidom::rxml::xml_ncname!("approved").to_owned(), "true");
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
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
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query)
        .build()
}

#[cfg(test)]
mod tests;
