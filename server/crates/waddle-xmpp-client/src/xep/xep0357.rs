//! XEP-0357 — typed `<enable/>` / `<disable/>` IQ builders.
//!
//! These IQs flow from the chat client to the user XMPP server. They
//! never carry provider credentials — those live behind the Push
//! Service component and are registered separately via XEP-0050
//! ad-hoc commands on `push.<domain>` (see [`crate::push`]).
//!
//! `publish_options` is a free-form XEP-0004 data form (XEP-0357 §5).
//! The user server passes it through to the PubSub publish unchanged,
//! so any `pubsub#publish-options` constraint the publisher wants to
//! apply (e.g. `pubsub#persist_items=false`) belongs in this form.

use minidom::Element;
use xmpp_parsers::data_forms::DataForm;

use crate::discovery::ids::next_id;
use crate::discovery::CLIENT_NS;

/// XEP-0357 namespace.
pub const NS_PUSH: &str = "urn:xmpp:push:0";

/// Build the XEP-0357 §5 `<enable/>` IQ. Never carries
/// provider-credential fields — those belong to the Push Service
/// registration round trip.
pub fn build_xep0357_enable_iq(
    service_jid: &str,
    node: &str,
    publish_options: Option<DataForm>,
) -> Element {
    let id = format!("push-enable-{}", next_id());
    let mut enable = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service_jid)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    if let Some(form) = publish_options {
        let element = Element::from(form);
        enable = enable.append(element);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(enable.build())
        .build()
}

/// Build the XEP-0357 §6.1 `<disable/>` IQ. A `None` `node` disables
/// ALL nodes at the service for the bound user.
pub fn build_xep0357_disable_iq(service_jid: &str, node: Option<&str>) -> Element {
    let id = format!("push-disable-{}", next_id());
    let mut disable = Element::builder("disable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service_jid);
    if let Some(node) = node {
        disable = disable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(disable.build())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

    fn hidden_text_single(var: &str, value: &str) -> Field {
        Field {
            var: Some(var.to_string()),
            type_: FieldType::Hidden,
            label: None,
            required: false,
            desc: None,
            options: vec![],
            values: vec![value.to_string()],
            media: vec![],
            validate: None,
        }
    }

    #[test]
    fn enable_iq_with_no_publish_options_carries_no_form_or_provider_fields() {
        let iq = build_xep0357_enable_iq("push.example.com", "node-abc", None);
        assert_eq!(iq.attr("type"), Some("set"));
        let enable = iq.get_child("enable", NS_PUSH).expect("enable");
        assert_eq!(enable.attr("jid"), Some("push.example.com"));
        assert_eq!(enable.attr("node"), Some("node-abc"));
        assert!(enable.get_child("x", "jabber:x:data").is_none());
        // Issue #718 acceptance criterion: zero provider-credential
        // children. The Push Service is the only consumer of those
        // values (registered separately via XEP-0050).
        assert!(enable
            .get_child("provider-endpoint", "urn:waddle:push-service:0")
            .is_none());
        assert!(enable
            .get_child("provider-token", "urn:waddle:push-service:0")
            .is_none());
        assert!(enable
            .get_child("provider-key-material", "urn:waddle:push-service:0")
            .is_none());
    }

    #[test]
    fn enable_iq_with_publish_options_serializes_form_type_and_fields() {
        let publish_options = DataForm::new(
            DataFormType::Submit,
            "http://jabber.org/protocol/pubsub#publish-options",
            vec![hidden_text_single("pubsub#persist_items", "false")],
        );
        let iq = build_xep0357_enable_iq("push.example.com", "node-abc", Some(publish_options));
        let enable = iq.get_child("enable", NS_PUSH).expect("enable");
        let form = enable
            .get_child("x", "jabber:x:data")
            .expect("publish-options form");
        assert_eq!(form.attr("type"), Some("submit"));
        let fields: Vec<(String, String)> = form
            .children()
            .filter(|c| c.name() == "field" && c.ns() == "jabber:x:data")
            .filter_map(|field| {
                Some((
                    field.attr("var")?.to_string(),
                    field.get_child("value", "jabber:x:data")?.text(),
                ))
            })
            .collect();
        assert!(fields.iter().any(|(var, value)| var == "FORM_TYPE"
            && value == "http://jabber.org/protocol/pubsub#publish-options"));
        assert!(fields
            .iter()
            .any(|(var, value)| var == "pubsub#persist_items" && value == "false"));
    }

    #[test]
    fn disable_iq_with_node_carries_node_attribute() {
        let iq = build_xep0357_disable_iq("push.example.com", Some("node-abc"));
        let disable = iq.get_child("disable", NS_PUSH).expect("disable");
        assert_eq!(disable.attr("jid"), Some("push.example.com"));
        assert_eq!(disable.attr("node"), Some("node-abc"));
    }

    #[test]
    fn disable_iq_without_node_omits_node_attribute() {
        // XEP-0357 §6.1: a disable without a node disables ALL nodes
        // at the service for this user. Pin the attribute omission so
        // a future refactor can't silently emit `node=""`.
        let iq = build_xep0357_disable_iq("push.example.com", None);
        let disable = iq.get_child("disable", NS_PUSH).expect("disable");
        assert_eq!(disable.attr("jid"), Some("push.example.com"));
        assert!(disable.attr("node").is_none());
    }
}
