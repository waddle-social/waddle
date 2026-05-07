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

pub use waddle_xmpp_core::roster::{
    AskType, ROSTER_NS, RosterItem, RosterVersion, RosterVersionRequest, Subscription,
};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use std::collections::HashSet;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Parsed roster query.
#[derive(Debug, Clone)]
pub struct RosterQuery {
    /// Inbound `ver` attribute, classified per XEP-0237 §2.5.
    pub ver: RosterVersionRequest,
    /// Items in the query (for set operations).
    pub items: Vec<RosterItem>,
}

impl RosterQuery {
    /// Create an empty roster query (for get operations).
    pub fn empty() -> Self {
        Self {
            ver: RosterVersionRequest::Absent,
            items: Vec::new(),
        }
    }

    /// Create a roster query with items (for set operations).
    pub fn with_items(items: Vec<RosterItem>) -> Self {
        Self {
            ver: RosterVersionRequest::Absent,
            items,
        }
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

    let ver = RosterVersionRequest::from_attr(query_elem.attr("ver"));

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

    let ver = RosterVersionRequest::from_attr(query_elem.attr("ver"));

    // Parse item elements. For roster set operations, client-provided
    // subscription/ask values are ignored except for subscription='remove'
    // per RFC 6121 Section 2.1.2.
    let items: Result<Vec<RosterItem>, XmppError> = query_elem
        .children()
        .filter(|c| c.name() == "item" && c.ns() == ROSTER_NS)
        .map(parse_roster_set_item)
        .collect();

    let items = items?;

    if items.is_empty() {
        return Err(XmppError::bad_request(Some(
            "Roster set must contain exactly one item".to_string(),
        )));
    }
    if items.len() > 1 {
        return Err(XmppError::bad_request(Some(
            "Roster set must contain exactly one item".to_string(),
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
        approved: false,
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
pub fn build_roster_result(
    original_iq: &Iq,
    items: &[RosterItem],
    ver: Option<&RosterVersion>,
) -> Iq {
    let mut query_builder = Element::builder("query", ROSTER_NS);

    // Add version if roster versioning is supported
    if let Some(v) = ver {
        query_builder = query_builder.attr("ver", v.as_str());
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
    from_jid: &BareJid,
    to_jid: &FullJid,
    item: &RosterItem,
    ver: Option<&RosterVersion>,
) -> Iq {
    let mut query_builder = Element::builder("query", ROSTER_NS);

    if let Some(v) = ver {
        query_builder = query_builder.attr("ver", v.as_str());
    }

    query_builder = query_builder.append(item.to_element());

    let query = query_builder.build();

    Iq {
        from: Some(Jid::from(from_jid.clone())),
        to: Some(Jid::from(to_jid.clone())),
        id: push_id.to_string(),
        payload: xmpp_parsers::iq::IqType::Set(query),
    }
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
mod tests;
