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
//!       <field var='device-token'><value>HEX_APNS_TOKEN</value></field>
//!       <field var='platform'><value>apple</value></field>
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
    for attr_key in ["device-token", "platform"] {
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
mod tests {
    use super::*;

    fn make_enable_iq(jid_attr: &str, node_attr: Option<&str>, with_form: bool) -> Iq {
        let mut enable = Element::builder("enable", NS_PUSH).attr("jid", jid_attr);

        if let Some(node) = node_attr {
            enable = enable.attr("node", node);
        }

        let mut enable_elem = enable.build();

        if with_form {
            let device_token_value = Element::builder("value", NS_DATA_FORMS)
                .append("HEX_APNS_TOKEN")
                .build();
            let device_token_field = Element::builder("field", NS_DATA_FORMS)
                .attr("var", "device-token")
                .append(device_token_value)
                .build();

            let platform_value = Element::builder("value", NS_DATA_FORMS)
                .append("apple")
                .build();
            let platform_field = Element::builder("field", NS_DATA_FORMS)
                .attr("var", "platform")
                .append(platform_value)
                .build();

            let form = Element::builder("x", NS_DATA_FORMS)
                .attr("type", "submit")
                .append(device_token_field)
                .append(platform_field)
                .build();

            enable_elem.append_child(form);
        }

        Iq {
            from: Some("alice@example.com".parse().expect("valid jid")),
            to: Some("example.com".parse().expect("valid jid")),
            id: "push1".to_string(),
            payload: IqType::Set(enable_elem),
        }
    }

    fn make_disable_iq(jid_attr: &str, node_attr: Option<&str>) -> Iq {
        let mut disable = Element::builder("disable", NS_PUSH).attr("jid", jid_attr);

        if let Some(node) = node_attr {
            disable = disable.attr("node", node);
        }

        Iq {
            from: Some("alice@example.com".parse().expect("valid jid")),
            to: Some("example.com".parse().expect("valid jid")),
            id: "push2".to_string(),
            payload: IqType::Set(disable.build()),
        }
    }

    #[test]
    fn test_ns_push_constant() {
        assert_eq!(NS_PUSH, "urn:xmpp:push:0");
    }

    #[test]
    fn test_is_push_enable() {
        let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
        assert!(is_push_enable(&iq));
        assert!(!is_push_disable(&iq));
    }

    #[test]
    fn test_is_push_enable_false_for_get() {
        let elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Get(elem),
        };
        assert!(!is_push_enable(&iq));
    }

    #[test]
    fn test_is_push_enable_false_for_wrong_ns() {
        let elem = Element::builder("enable", "wrong:ns")
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Set(elem),
        };
        assert!(!is_push_enable(&iq));
    }

    #[test]
    fn test_is_push_enable_false_for_result() {
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Result(None),
        };
        assert!(!is_push_enable(&iq));
    }

    #[test]
    fn test_is_push_disable() {
        let iq = make_disable_iq("push-service.example.com", Some("web-push"));
        assert!(is_push_disable(&iq));
        assert!(!is_push_enable(&iq));
    }

    #[test]
    fn test_is_push_disable_false_for_get() {
        let elem = Element::builder("disable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Get(elem),
        };
        assert!(!is_push_disable(&iq));
    }

    #[test]
    fn test_is_push_disable_false_for_wrong_element() {
        let elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Set(elem),
        };
        assert!(!is_push_disable(&iq));
    }

    #[test]
    fn test_parse_push_enable_with_options() {
        let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
        let enable = parse_push_enable(&iq).expect("should parse");

        assert_eq!(enable.jid, "push-service.example.com");
        assert_eq!(enable.node.as_deref(), Some("web-push"));
        assert_eq!(enable.options.len(), 2);

        assert!(enable
            .options
            .iter()
            .any(|(k, v)| k == "device-token" && v == "HEX_APNS_TOKEN"));
        assert!(enable
            .options
            .iter()
            .any(|(k, v)| k == "platform" && v == "apple"));
    }

    #[test]
    fn test_parse_push_enable_without_options() {
        let iq = make_enable_iq("push-service.example.com", Some("web-push"), false);
        let enable = parse_push_enable(&iq).expect("should parse");

        assert_eq!(enable.jid, "push-service.example.com");
        assert_eq!(enable.node.as_deref(), Some("web-push"));
        assert!(enable.options.is_empty());
    }

    #[test]
    fn test_parse_push_enable_with_attribute_options() {
        let elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push-service.example.com")
            .attr("node", "web-push")
            .attr("device-token", "HEX_APNS_TOKEN")
            .attr("platform", "apple")
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().expect("valid jid")),
            to: Some("example.com".parse().expect("valid jid")),
            id: "push1".to_string(),
            payload: IqType::Set(elem),
        };
        let enable = parse_push_enable(&iq).expect("should parse");

        assert_eq!(enable.jid, "push-service.example.com");
        assert_eq!(enable.node.as_deref(), Some("web-push"));
        assert_eq!(enable.options.len(), 2);
        assert!(enable
            .options
            .iter()
            .any(|(k, v)| k == "device-token" && v == "HEX_APNS_TOKEN"));
        assert!(enable
            .options
            .iter()
            .any(|(k, v)| k == "platform" && v == "apple"));
    }

    #[test]
    fn test_parse_push_enable_without_node() {
        let iq = make_enable_iq("push-service.example.com", None, false);
        let enable = parse_push_enable(&iq).expect("should parse");

        assert_eq!(enable.jid, "push-service.example.com");
        assert!(enable.node.is_none());
    }

    #[test]
    fn test_parse_push_enable_missing_jid() {
        let elem = Element::builder("enable", NS_PUSH).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Set(elem),
        };
        assert!(parse_push_enable(&iq).is_none());
    }

    #[test]
    fn test_parse_push_enable_empty_jid() {
        let elem = Element::builder("enable", NS_PUSH).attr("jid", "").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Set(elem),
        };
        assert!(parse_push_enable(&iq).is_none());
    }

    #[test]
    fn test_parse_push_enable_wrong_payload_type() {
        let elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Get(elem),
        };
        assert!(parse_push_enable(&iq).is_none());
    }

    #[test]
    fn test_parse_push_disable() {
        let iq = make_disable_iq("push-service.example.com", Some("web-push"));
        let disable = parse_push_disable(&iq).expect("should parse");

        assert_eq!(disable.jid, "push-service.example.com");
        assert_eq!(disable.node.as_deref(), Some("web-push"));
    }

    #[test]
    fn test_parse_push_disable_without_node() {
        let iq = make_disable_iq("push-service.example.com", None);
        let disable = parse_push_disable(&iq).expect("should parse");

        assert_eq!(disable.jid, "push-service.example.com");
        assert!(disable.node.is_none());
    }

    #[test]
    fn test_parse_push_disable_missing_jid() {
        let elem = Element::builder("disable", NS_PUSH).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Set(elem),
        };
        assert!(parse_push_disable(&iq).is_none());
    }

    #[test]
    fn test_parse_push_disable_wrong_payload_type() {
        let elem = Element::builder("disable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test".to_string(),
            payload: IqType::Get(elem),
        };
        assert!(parse_push_disable(&iq).is_none());
    }

    #[test]
    fn test_build_push_enable_result() {
        let iq = make_enable_iq("push-service.example.com", Some("web-push"), true);
        let result = build_push_enable_result(&iq);

        assert_eq!(result.id, "push1");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        assert!(matches!(result.payload, IqType::Result(None)));
    }

    #[test]
    fn test_build_push_disable_result() {
        let iq = make_disable_iq("push-service.example.com", Some("web-push"));
        let result = build_push_disable_result(&iq);

        assert_eq!(result.id, "push2");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        assert!(matches!(result.payload, IqType::Result(None)));
    }

    #[test]
    fn test_build_result_with_none_addresses() {
        let elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-none".to_string(),
            payload: IqType::Set(elem),
        };
        let result = build_push_enable_result(&iq);
        assert!(result.from.is_none());
        assert!(result.to.is_none());
        assert_eq!(result.id, "test-none");
    }

    #[test]
    fn test_parse_data_form_with_empty_value() {
        let empty_value = Element::builder("value", NS_DATA_FORMS).build();
        let field = Element::builder("field", NS_DATA_FORMS)
            .attr("var", "device-token")
            .append(empty_value)
            .build();
        let form = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(field)
            .build();
        let mut enable_elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        enable_elem.append_child(form);

        let options = parse_data_form_options(&enable_elem);
        assert!(options.is_empty());
    }

    #[test]
    fn test_parse_data_form_with_missing_var() {
        let value = Element::builder("value", NS_DATA_FORMS)
            .append("some-value")
            .build();
        let field = Element::builder("field", NS_DATA_FORMS)
            .append(value)
            .build();
        let form = Element::builder("x", NS_DATA_FORMS)
            .attr("type", "submit")
            .append(field)
            .build();
        let mut enable_elem = Element::builder("enable", NS_PUSH)
            .attr("jid", "push.example.com")
            .build();
        enable_elem.append_child(form);

        let options = parse_data_form_options(&enable_elem);
        assert!(options.is_empty());
    }

    #[test]
    fn test_push_enable_struct_debug() {
        let enable = PushEnable {
            jid: "push.example.com".to_string(),
            node: Some("web-push".to_string()),
            options: vec![("key".to_string(), "val".to_string())],
        };
        let debug = format!("{:?}", enable);
        assert!(debug.contains("push.example.com"));
        assert!(debug.contains("web-push"));
    }

    #[test]
    fn test_push_disable_struct_debug() {
        let disable = PushDisable {
            jid: "push.example.com".to_string(),
            node: None,
        };
        let debug = format!("{:?}", disable);
        assert!(debug.contains("push.example.com"));
    }

    #[test]
    fn test_push_enable_clone_eq() {
        let enable = PushEnable {
            jid: "push.example.com".to_string(),
            node: Some("node1".to_string()),
            options: vec![("k".to_string(), "v".to_string())],
        };
        let cloned = enable.clone();
        assert_eq!(enable, cloned);
    }

    #[test]
    fn test_push_disable_clone_eq() {
        let disable = PushDisable {
            jid: "push.example.com".to_string(),
            node: Some("node1".to_string()),
        };
        let cloned = disable.clone();
        assert_eq!(disable, cloned);
    }
}
