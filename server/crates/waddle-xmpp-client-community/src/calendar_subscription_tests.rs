use super::{safe_url, SubscriptionCommand, WaddleCalendarSubscription};
use minidom::Element;
use waddle_xmpp_client::xep::xep0050::{NS_COMMANDS, NS_DATA_FORMS};
use waddle_xmpp_core::calendar_subscription::{ISSUE, REVOKE};

const ENDPOINT: &str = "wss://xmpp.example.test/ws";
const DEMO: &str = "https://api.example.test/api/calendar/community/sample/events.ics";

fn result(command: &SubscriptionCommand, field: &str) -> Element {
    let node = command.node();
    format!("<iq xmlns='jabber:client' type='result' id='{}' from='example.test'><command xmlns='{NS_COMMANDS}' status='completed' node='{node}'>{field}</command></iq>",command.id).parse().unwrap()
}
fn form(value: &str) -> String {
    format!("<x xmlns='{NS_DATA_FORMS}' type='result'><field var='FORM_TYPE' type='hidden'><value>{ISSUE}</value></field><field var='url' type='text-single'><value>{value}</value></field></x>")
}
#[test]
fn xep0050_subscription_uses_only_the_bare_server_and_correlated_single_stage_result() {
    let own = "alice@example.test/device".parse().unwrap();
    for revoke in [false, true] {
        let action = SubscriptionCommand::new(&own, revoke);
        let iq = action.request();
        assert_eq!(iq.attr("to"), Some("example.test"));
        assert_eq!(iq.attr("type"), Some("set"));
        let command = iq.get_child("command", NS_COMMANDS).unwrap();
        assert_eq!(
            command.attr("node"),
            Some(if revoke { REVOKE } else { ISSUE })
        );
        assert_eq!(command.attr("action"), Some("execute"));
        assert!(command.children().next().is_none());
        let payload = if revoke { String::new() } else { form(DEMO) };
        let reply = result(&action, &payload);
        let valid = action.result(&reply, ENDPOINT).unwrap();
        assert!(matches!(
            (valid, revoke),
            (WaddleCalendarSubscription::Revoked, true)
                | (WaddleCalendarSubscription::Available { .. }, false)
        ));
        let refused = result(
            &action,
            &format!("{payload}<note type='error'>Denied</note>"),
        );
        assert!(action.result(&refused, ENDPOINT).is_none());
        let message = result(&action, &format!("<note type='info'>Done</note>{payload}"));
        assert!(action.result(&message, ENDPOINT).is_some());
        let original = String::from(&reply);
        for (needle, replacement) in [
            ("from='example.test'", "from='another.test'"),
            ("from='example.test'", ""),
            (action.id.as_str(), "other-id"),
            (action.node(), "different-node"),
            ("completed", "executing"),
        ] {
            let bad = original.replace(needle, replacement);
            assert_ne!(bad, original);
            assert!(action.result(&bad.parse().unwrap(), ENDPOINT).is_none());
        }
    }
}
#[test]
fn xep0004_private_link_rejects_ambiguous_or_incorrectly_typed_forms() {
    let own = "alice@example.test".parse().unwrap();
    let issue = SubscriptionCommand::new(&own, false);
    let revoke = SubscriptionCommand::new(&own, true);
    assert!(revoke
        .result(&result(&revoke, &form(DEMO)), ENDPOINT)
        .is_none());
    let valid = form(DEMO);
    for bad in [
        valid.replace("hidden", "text-single"),
        valid.replace("text-single", "text-multi"),
        valid.replace("type='result'", "type='form'"),
        valid.replace(ISSUE, REVOKE),
        valid.replace(
            "</x>",
            "<field var='url'><value>https://other.test/</value></field></x>",
        ),
        valid.replace(
            "</value></field></x>",
            "</value><value>another</value></field></x>",
        ),
        String::new(),
    ] {
        assert!(issue.result(&result(&issue, &bad), ENDPOINT).is_none());
    }
    assert!(
        matches!(issue.result(&result(&issue,&valid),ENDPOINT),Some(WaddleCalendarSubscription::Available{url}) if url==DEMO)
    );
}
#[test]
fn feed_url_requires_https_or_matching_loopback_development_transport() {
    assert!(safe_url(DEMO, ENDPOINT));
    for bad in [
        "https:example.test/token",
        "https://alice:password@example.test/",
        "https://example.test/a#fragment",
        "http://example.test/calendar",
        "file:///tmp/url",
        "javascript:alert(1)",
        "https://example.test/a\nb",
        "https://example.test\\@evil.test",
    ] {
        assert!(!safe_url(bad, ENDPOINT));
    }
    assert!(!safe_url("http://localhost:8000/calendar", ENDPOINT));
    assert!(!safe_url("http://127.0.0.1/calendar", "ws://remote.test/"));
    assert!(safe_url(
        "http://127.0.0.1:8080/calendar",
        "ws://localhost:3000/ws"
    ));
    assert!(safe_url("http://[::1]:8000/calendar", "ws://[::1]/ws"));
}
