//! XEP-0357: Push Notifications
//!
//! Enables and disables push notification subscriptions via IQ stanzas.
//! A client sends `<enable/>` with a push service JID and optional node/options
//! to register for push, or `<disable/>` to unregister.
//!
//! ## XML Format
//!
//! Enable push with PubSub publish options (data form fields):
//! ```xml
//! <iq type='set' id='push1'>
//!   <enable xmlns='urn:xmpp:push:0' jid='push-service.example.com' node='node-1'>
//!     <x xmlns='jabber:x:data' type='submit'>
//!       <field var='FORM_TYPE'><value>http://jabber.org/protocol/pubsub#publish-options</value></field>
//!       <field var='secret'><value>opaque-push-service-secret</value></field>
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

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::xep0004::NS_DATA_FORMS;

/// Namespace for XEP-0357 Push Notifications.
pub const NS_PUSH: &str = "urn:xmpp:push:0";

/// XEP-0060 publish-options FORM_TYPE used by XEP-0357 enable options.
pub const NS_PUBSUB_PUBLISH_OPTIONS: &str = "http://jabber.org/protocol/pubsub#publish-options";

/// A parsed push enable request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEnable {
    /// The JID of the push service.
    pub jid: BareJid,
    /// The PubSub node on the push service.
    pub node: Option<String>,
    /// Key-value pairs from the XEP-0060 publish-options form.
    pub options: Vec<(String, String)>,
    /// The original XEP-0004 publish-options form for later PubSub publish.
    pub publish_options: Option<Element>,
}

/// A parsed push disable request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDisable {
    /// The JID of the push service to disable.
    pub jid: BareJid,
    /// The PubSub node on the push service to disable.
    pub node: Option<String>,
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an IQ stanza is a push enable request.
pub fn is_push_enable(iq: &Iq) -> bool {
    match iq {
        Iq::Set { payload: elem, .. } => elem.name() == "enable" && elem.ns() == NS_PUSH,
        _ => false,
    }
}

/// Check if an IQ stanza is a push disable request.
pub fn is_push_disable(iq: &Iq) -> bool {
    match iq {
        Iq::Set { payload: elem, .. } => elem.name() == "disable" && elem.ns() == NS_PUSH,
        _ => false,
    }
}

// ── Parsing ─────────────────────────────────────────────────────────

/// Parse a push enable request from an IQ stanza.
///
/// Returns `None` if the IQ is not a valid push enable request.
pub fn parse_push_enable(iq: &Iq) -> Option<PushEnable> {
    let elem = match iq {
        Iq::Set { payload: elem, .. } if elem.name() == "enable" && elem.ns() == NS_PUSH => elem,
        _ => return None,
    };

    let jid = parse_push_service_jid(elem.attr("jid")?)?;
    let node = elem
        .attr("node")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let publish_options = parse_publish_options_form(elem);
    let options = publish_options
        .as_ref()
        .map(parse_data_form_options)
        .unwrap_or_default();

    Some(PushEnable {
        jid,
        node,
        options,
        publish_options,
    })
}

/// Parse a push disable request from an IQ stanza.
///
/// Returns `None` if the IQ is not a valid push disable request.
pub fn parse_push_disable(iq: &Iq) -> Option<PushDisable> {
    let elem = match iq {
        Iq::Set { payload: elem, .. } if elem.name() == "disable" && elem.ns() == NS_PUSH => elem,
        _ => return None,
    };

    let jid = parse_push_service_jid(elem.attr("jid")?)?;
    let node = elem
        .attr("node")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    Some(PushDisable { jid, node })
}

/// Extract key-value pairs from a XEP-0004 data form element.
fn parse_data_form_options(form: &Element) -> Vec<(String, String)> {
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

fn parse_push_service_jid(raw: &str) -> Option<BareJid> {
    if raw.is_empty() || raw.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return None;
    }
    raw.parse::<BareJid>().ok()
}

fn parse_publish_options_form(parent: &Element) -> Option<Element> {
    parent
        .children()
        .find(|child| {
            child.name() == "x"
                && child.ns() == NS_DATA_FORMS
                && child.attr("type") == Some("submit")
                && data_form_type(child).as_deref() == Some(NS_PUBSUB_PUBLISH_OPTIONS)
        })
        .cloned()
}

fn data_form_type(form: &Element) -> Option<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.ns() == NS_DATA_FORMS)
        .find(|field| field.attr("var") == Some("FORM_TYPE"))
        .and_then(|field| field.get_child("value", NS_DATA_FORMS))
        .map(Element::text)
        .filter(|value| !value.is_empty())
}

// ── Building ────────────────────────────────────────────────────────

/// Build an IQ result for a push enable request.
pub fn build_push_enable_result(iq: &Iq) -> Iq {
    Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_string(),
        payload: None,
    }
}

/// Build an IQ result for a push disable request.
pub fn build_push_disable_result(iq: &Iq) -> Iq {
    Iq::Result {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_string(),
        payload: None,
    }
}

#[cfg(test)]
mod tests;
