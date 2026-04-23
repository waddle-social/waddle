//! XEP-0357 APNS push notification registration suite.

use minidom::Element;
use waddle_xmpp::push::{InMemoryPushStore, PushSubscription, PushSubscriptionStore};
use waddle_xmpp::xep::xep0357::{parse_push_disable, parse_push_enable, NS_PUSH};
use xmpp_parsers::iq::{Iq, IqType};

const DATA_FORMS_NS: &str = "jabber:x:data";

fn apns_enable_iq(user: &str, service: &str, node: &str, token: &str) -> Iq {
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "device-token")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(token)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "platform")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append("apple")
                        .build(),
                )
                .build(),
        )
        .build();

    Iq {
        from: Some(user.parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push-enable".to_string(),
        payload: IqType::Set(
            Element::builder("enable", NS_PUSH)
                .attr("jid", service)
                .attr("node", node)
                .append(form)
                .build(),
        ),
    }
}

fn disable_iq(user: &str, service: &str, node: &str) -> Iq {
    Iq {
        from: Some(user.parse().expect("valid jid")),
        to: Some("example.com".parse().expect("valid jid")),
        id: "push-disable".to_string(),
        payload: IqType::Set(
            Element::builder("disable", NS_PUSH)
                .attr("jid", service)
                .attr("node", node)
                .build(),
        ),
    }
}

fn subscription(user: &str, service: &str, node: &str, token: &str) -> PushSubscription {
    PushSubscription {
        user_jid: user.to_string(),
        service_jid: service.to_string(),
        node: Some(node.to_string()),
        device_token: Some(token.to_string()),
        platform: Some("apple".to_string()),
        sandbox: false,
        endpoint: None,
        p256dh: None,
        auth_key: None,
    }
}

#[tokio::test]
async fn register_and_upsert_subscription() {
    let store = InMemoryPushStore::new();
    store
        .register(subscription(
            "alice@example.com",
            "push.example.com",
            "n1",
            "tok1",
        ))
        .await
        .expect("register should succeed");
    store
        .register(subscription(
            "alice@example.com",
            "push.example.com",
            "n1",
            "tok2",
        ))
        .await
        .expect("upsert should succeed");

    let subs = store
        .get_for_user("alice@example.com")
        .await
        .expect("read should succeed");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].device_token.as_deref(), Some("tok2"));
}

#[tokio::test]
async fn unregister_subscription() {
    let store = InMemoryPushStore::new();
    store
        .register(subscription(
            "alice@example.com",
            "push.example.com",
            "n1",
            "tok1",
        ))
        .await
        .expect("register should succeed");

    store
        .remove("alice@example.com", "push.example.com", Some("n1"))
        .await
        .expect("remove should succeed");
    assert!(store
        .get_for_user("alice@example.com")
        .await
        .expect("read should succeed")
        .is_empty());
}

#[tokio::test]
async fn subscriptions_are_user_isolated() {
    let store = InMemoryPushStore::new();
    store
        .register(subscription(
            "alice@example.com",
            "push.example.com",
            "n1",
            "tok1",
        ))
        .await
        .expect("register alice should succeed");
    store
        .register(subscription(
            "bob@example.com",
            "push.example.com",
            "n1",
            "tok2",
        ))
        .await
        .expect("register bob should succeed");

    let alice = store
        .get_for_user("alice@example.com")
        .await
        .expect("read alice should succeed");
    let bob = store
        .get_for_user("bob@example.com")
        .await
        .expect("read bob should succeed");
    assert_eq!(alice.len(), 1);
    assert_eq!(bob.len(), 1);
    assert_eq!(alice[0].device_token.as_deref(), Some("tok1"));
    assert_eq!(bob[0].device_token.as_deref(), Some("tok2"));
}

#[test]
fn parse_apns_enable_iq() {
    let iq = apns_enable_iq(
        "alice@example.com",
        "push.example.com",
        "node-123",
        "HEX_APNS_TOKEN",
    );
    let parsed = parse_push_enable(&iq).expect("iq should parse");
    assert_eq!(parsed.jid, "push.example.com");
    assert_eq!(parsed.node.as_deref(), Some("node-123"));
    assert!(parsed
        .options
        .iter()
        .any(|(k, v)| k == "device-token" && v == "HEX_APNS_TOKEN"));
    assert!(parsed
        .options
        .iter()
        .any(|(k, v)| k == "platform" && v == "apple"));
}

#[test]
fn parse_disable_iq() {
    let iq = disable_iq("alice@example.com", "push.example.com", "node-123");
    let parsed = parse_push_disable(&iq).expect("iq should parse");
    assert_eq!(parsed.jid, "push.example.com");
    assert_eq!(parsed.node.as_deref(), Some("node-123"));
}
