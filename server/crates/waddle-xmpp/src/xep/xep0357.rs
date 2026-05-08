//! XEP-0357: Push Notifications
//!
//! Enables and disables push notification subscriptions via IQ stanzas.
//! A client sends `<enable/>` with a push service JID and optional node/options
//! to register for push, or `<disable/>` to unregister.
//!
//! ## XML Format
//!
//! Enable push with options (data form fields):
//! ```xml
//! <iq type='set' id='push1'>
//!   <enable xmlns='urn:xmpp:push:0' jid='push-service.example.com' node='web-push'>
//!     <x xmlns='jabber:x:data' type='submit'>
//!       <field var='endpoint'><value>https://push.example.com/...</value></field>
//!       <field var='p256dh'><value>BASE64KEY</value></field>
//!       <field var='auth'><value>BASE64AUTH</value></field>
//!     </x>
//!   </enable>
//! </iq>
//! ```
//!
//! Disable push:
//! ```xml
//! <iq type='set' id='push2'>
//!   <disable xmlns='urn:xmpp:push:0' jid='push-service.example.com' node='web-push'/>
//! </iq>
//! ```
//!
//! ## Server Behavior
//!
//! 1. On `<enable/>`, store the push subscription for the user
//! 2. On `<disable/>`, remove the matching subscription
//! 3. When a notification-worthy event occurs, send a push notification
//!    to the registered push service

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0357 Push Notifications.
pub const NS_PUSH: &str = "urn:xmpp:push:0";

/// Namespace for XEP-0004 Data Forms (used in enable options).
const NS_DATA_FORMS: &str = "jabber:x:data";

/// A parsed push enable request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEnable {
    /// The JID of the push service.
    pub jid: String,
    /// The PubSub node on the push service.
    pub node: Option<String>,
    /// Key-value pairs from the data form options.
    pub options: Vec<(String, String)>,
}

/// A parsed push disable request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDisable {
    /// The JID of the push service to disable.
    pub jid: String,
    /// The PubSub node on the push service to disable.
    pub node: Option<String>,
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an IQ stanza is a push enable request.
pub fn is_push_enable(iq: &Iq) -> bool {
    match &iq.payload {
        IqType::Set(elem) => elem.name() == "enable" && elem.ns() == NS_PUSH,
        _ => false,
    }
}

/// Check if an IQ stanza is a push disable request.
pub fn is_push_disable(iq: &Iq) -> bool {
    match &iq.payload {
        IqType::Set(elem) => elem.name() == "disable" && elem.ns() == NS_PUSH,
        _ => false,
    }
}

// ── Parsing ─────────────────────────────────────────────────────────

/// Parse a push enable request from an IQ stanza.
///
/// Returns `None` if the IQ is not a valid push enable request.
pub fn parse_push_enable(iq: &Iq) -> Option<PushEnable> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "enable" && elem.ns() == NS_PUSH => elem,
        _ => return None,
    };

    let jid = elem.attr("jid").filter(|s| !s.is_empty())?.to_owned();
    let node = elem
        .attr("node")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let mut options = parse_data_form_options(elem);
    for attr_key in ["endpoint", "p256dh", "auth"] {
        if let Some(value) = elem.attr(attr_key).filter(|s| !s.is_empty()) {
            if !options.iter().any(|(k, _)| k == attr_key) {
                options.push((attr_key.to_owned(), value.to_owned()));
            }
        }
    }

    Some(PushEnable { jid, node, options })
}

/// Parse a push disable request from an IQ stanza.
///
/// Returns `None` if the IQ is not a valid push disable request.
pub fn parse_push_disable(iq: &Iq) -> Option<PushDisable> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "disable" && elem.ns() == NS_PUSH => elem,
        _ => return None,
    };

    let jid = elem.attr("jid").filter(|s| !s.is_empty())?.to_owned();
    let node = elem
        .attr("node")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    Some(PushDisable { jid, node })
}

/// Extract key-value pairs from a `<x xmlns='jabber:x:data'>` child element.
fn parse_data_form_options(parent: &Element) -> Vec<(String, String)> {
    let Some(form) = parent.get_child("x", NS_DATA_FORMS) else {
        return Vec::new();
    };

    form.children()
        .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
        .filter_map(|field| {
            let var = field.attr("var").filter(|s| !s.is_empty())?;
            let value = field
                .get_child("value", NS_DATA_FORMS)
                .map(|v| v.text())
                .filter(|s| !s.is_empty())?;
            Some((var.to_owned(), value))
        })
        .collect()
}

// ── Building ────────────────────────────────────────────────────────

/// Build an IQ result for a push enable request.
pub fn build_push_enable_result(iq: &Iq) -> Iq {
    Iq {
        from: iq.to.clone(),
        to: iq.from.clone(),
        id: iq.id.clone(),
        payload: IqType::Result(None),
    }
}

/// Build an IQ result for a push disable request.
pub fn build_push_disable_result(iq: &Iq) -> Iq {
    Iq {
        from: iq.to.clone(),
        to: iq.from.clone(),
        id: iq.id.clone(),
        payload: IqType::Result(None),
    }
}

#[cfg(test)]
mod tests;
