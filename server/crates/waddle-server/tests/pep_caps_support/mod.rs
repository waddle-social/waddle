//! Shared XEP-0115/XEP-0163 helpers for PEP wire tests: advertise caps
//! with `+notify` features and wait for pubsub events of one node.

use std::time::Duration;
use waddle_ws_test_support as ws_common;
use ws_common::WsXmppClient;

const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

pub fn caps_ver(name: &str, features: &[&str]) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    let identities = vec![Identity::new("client", "pc", Some(name))];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    waddle_xmpp::xep::xep0115::compute_caps_hash(&identities, &features)
}

pub async fn wait_for_event(
    client: &mut WsXmppClient,
    node: &str,
    dur: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && frame.contains(&format!(r#"node='{node}'"#))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}

/// Go available with caps advertising `features`, answering the server's
/// disco#info query.
pub async fn go_available_with_caps(client: &mut WsXmppClient, name: &str, features: &[&str]) {
    let full = client.full_jid.clone().expect("full jid");
    let caps_node = format!("https://{name}.example/caps");
    let ver = caps_ver(name, features);
    client
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("presence with caps");
    let query = client
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server queries caps");
    let iq_id = ws_common::extract_attr_after(&query, "<iq", "id").expect("iq id");
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="{name}"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("disco reply");
}
