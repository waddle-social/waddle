//! Shared Personal Eventing Protocol helpers.

use jid::BareJid;
use xmpp_parsers::iq::Iq;

use super::node::AccessModel;
use super::stanzas::is_pubsub_iq;
use crate::disco::{Feature, Identity};

/// Check if an IQ is a PEP request for the current user.
pub fn is_pep_request(iq: &Iq, user_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    match &iq.to {
        None => true,
        Some(to_jid) => to_jid.to_bare() == *user_jid,
    }
}

/// Check if a PubSub IQ is addressed to another user's PEP service.
pub fn is_pep_request_to(iq: &Iq, target_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    match &iq.to {
        Some(to_jid) => to_jid.to_bare() == *target_jid,
        None => false,
    }
}

/// Shared PEP node classification helpers.
pub struct PepHandler;

impl PepHandler {
    /// Check if a node name is one of the built-in well-known PEP nodes.
    pub fn is_well_known_node(node: &str) -> bool {
        node == "urn:xmpp:bookmarks:1"
            || node == "urn:xmpp:avatar:data"
            || node == "urn:xmpp:avatar:metadata"
            || node == "http://jabber.org/protocol/nick"
            || node == "http://jabber.org/protocol/mood"
            || node == "http://jabber.org/protocol/activity"
            || node == "http://jabber.org/protocol/tune"
            || node == "http://jabber.org/protocol/geoloc"
            || node == "urn:xmpp:microblog:0"
            || node.starts_with("eu.siacs.conversations.axolotl")
    }

    /// Get the default access model for a well-known PEP node.
    pub fn default_access_model_for_node(node: &str) -> AccessModel {
        if node == "urn:xmpp:bookmarks:1" {
            return AccessModel::Whitelist;
        }

        if node.starts_with("eu.siacs.conversations.axolotl") {
            return AccessModel::Open;
        }

        AccessModel::Presence
    }
}

/// Build the PEP service disco identity.
pub fn build_pep_identity() -> Identity {
    Identity::new("pubsub", "pep", Some("Personal Eventing Protocol"))
}

/// Shared PEP feature advertisement.
pub fn pep_features() -> Vec<Feature> {
    vec![
        Feature::pubsub(),
        Feature::pubsub_access_presence(),
        Feature::pubsub_access_whitelist(),
        Feature::pubsub_auto_create(),
        Feature::new("http://jabber.org/protocol/pubsub#auto-subscribe"),
        Feature::new("http://jabber.org/protocol/pubsub#filtered-notifications"),
        Feature::pubsub_persistent_items(),
        Feature::pubsub_publish(),
        Feature::pubsub_retrieve_items(),
        Feature::pubsub_subscribe(),
        Feature::mam(),
        Feature::new("urn:xmpp:mam:2#extended"),
        Feature::new("urn:xmpp:push:0"),
        Feature::new("urn:xmpp:pep-vcard-conversion:0"),
        Feature::bookmarks_compat(),
        Feature::new("urn:xmpp:bookmarks:1#compat-pep"),
        Feature::new("http://jabber.org/protocol/pubsub#config-node-max"),
        Feature::new("eu.siacs.conversations.axolotl.whitelisted"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::IqType;

    fn make_pubsub_iq(to: Option<&str>) -> Iq {
        let pubsub = Element::builder("pubsub", super::super::stanzas::NS_PUBSUB)
            .append(
                Element::builder("items", super::super::stanzas::NS_PUBSUB)
                    .attr("node", "test")
                    .build(),
            )
            .build();

        Iq {
            from: Some("user@example.com/resource".parse().expect("valid jid")),
            to: to.map(|s| s.parse().expect("valid jid")),
            id: "test-1".to_string(),
            payload: IqType::Get(pubsub),
        }
    }

    #[test]
    fn detects_pep_requests() {
        let iq = make_pubsub_iq(None);
        let user_jid: BareJid = "user@example.com".parse().expect("valid jid");
        assert!(is_pep_request(&iq, &user_jid));
    }

    #[test]
    fn well_known_nodes_have_expected_access_models() {
        assert!(PepHandler::is_well_known_node("urn:xmpp:bookmarks:1"));
        assert_eq!(
            PepHandler::default_access_model_for_node("urn:xmpp:bookmarks:1"),
            AccessModel::Whitelist
        );
        assert_eq!(
            PepHandler::default_access_model_for_node("eu.siacs.conversations.axolotl.devicelist"),
            AccessModel::Open
        );
    }

    #[test]
    fn pep_features_are_unique_for_config_node_max() {
        let count = pep_features()
            .iter()
            .filter(|feature| feature.0 == "http://jabber.org/protocol/pubsub#config-node-max")
            .count();
        assert_eq!(count, 1);
    }
}
