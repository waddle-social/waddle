//! Shared Personal Eventing Protocol helpers.

use jid::BareJid;
use xmpp_parsers::iq::Iq;

use super::node::AccessModel;
use super::stanzas::is_pubsub_iq;
use crate::disco::{Feature, Identity};

pub const PEP_NODE_BOOKMARKS: &str = "urn:xmpp:bookmarks:1";
pub const PEP_NODE_AVATAR_DATA: &str = "urn:xmpp:avatar:data";
pub const PEP_NODE_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";

/// XEP-0490 §3 Message Displayed Synchronization PEP node + namespace.
///
/// The same string is both the PEP node id and the payload namespace —
/// matches the wire shape in the XEP-0490 examples.
pub const PEP_NODE_MDS_DISPLAYED: &str = "urn:xmpp:mds:displayed:0";

/// XEP-0292 §3 vCard4 PEP node.
///
/// Mirrored as `waddle_xmpp::xep::xep0292::PEP_NODE_VCARD4` for use at
/// the wire-handling layer; defined here so the well-known-node
/// configuration table in `NodeConfig::pep_for_node` can reference it
/// without pulling `waddle-xmpp` into `waddle-xmpp-core`. The two
/// constants are pinned equal by a `waddle-xmpp` test.
pub const PEP_NODE_VCARD4: &str = "urn:xmpp:vcard4";

/// Waddle DND PEP node + namespace (issue #367).
///
/// Mirrored as `waddle_xmpp::xep::xep_waddle_dnd::PEP_NODE_WADDLE_DND`
/// so the well-known-node configuration table in
/// `NodeConfig::pep_for_node` can reference it without pulling
/// `waddle-xmpp` into `waddle-xmpp-core`. The two constants are pinned
/// equal by a `waddle-xmpp` test.
///
/// The DND payload contains sensitive personal data (timezone, weekly
/// sleep windows, snooze deadlines), so the well-known config forces
/// `access_model = whitelist` and `send_last_published_item = never`
/// to keep this off the roster-contact PEP fan-out. The authoritative
/// consumer is the server-side projection at the T1 push gate.
pub const PEP_NODE_WADDLE_DND: &str = "urn:waddle:dnd:0";

/// Check if an IQ is a PEP request for the current user.
pub fn is_pep_request(iq: &Iq, user_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    match iq.to() {
        None => true,
        Some(to_jid) => to_jid.to_bare() == *user_jid,
    }
}

/// Check if a PubSub IQ is addressed to another user's PEP service.
pub fn is_pep_request_to(iq: &Iq, target_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    match iq.to() {
        Some(to_jid) => to_jid.to_bare() == *target_jid,
        None => false,
    }
}

/// Shared PEP node classification helpers.
pub struct PepHandler;

impl PepHandler {
    /// Check if a node name is one of the built-in well-known PEP nodes.
    pub fn is_well_known_node(node: &str) -> bool {
        node == PEP_NODE_BOOKMARKS
            || node == PEP_NODE_AVATAR_DATA
            || node == PEP_NODE_AVATAR_METADATA
            || node == PEP_NODE_MDS_DISPLAYED
            || node == PEP_NODE_VCARD4
            || node == PEP_NODE_WADDLE_DND
            || node == "http://jabber.org/protocol/nick"
            || node == "http://jabber.org/protocol/mood"
            || node == "http://jabber.org/protocol/activity"
            || node == "http://jabber.org/protocol/tune"
            || node == "http://jabber.org/protocol/geoloc"
            || node == "urn:xmpp:microblog:0"
    }

    /// Get the default access model for a well-known PEP node.
    pub fn default_access_model_for_node(node: &str) -> AccessModel {
        if node == PEP_NODE_BOOKMARKS
            || node == PEP_NODE_MDS_DISPLAYED
            || node == PEP_NODE_WADDLE_DND
        {
            return AccessModel::Whitelist;
        }
        if node == PEP_NODE_VCARD4 {
            // XEP-0292 §6.1: vCard4 is publicly readable; the canonical
            // PEP node access model is `open`.
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
///
/// XEP-0163 §6 mandates `http://jabber.org/protocol/pubsub#pep`
/// on the user's bare-JID disco#info as the canonical PEP-support
/// signal — without it, clients fall back to per-node `+notify`
/// probing or skip PEP-driven features entirely.
/// Note: `urn:waddle:dnd:0+notify` is deliberately NOT advertised
/// here. The DND PEP node defaults to `AccessModel::Whitelist` +
/// `SendLastPublishedItem::Never` (see [`super::node::NodeConfig::waddle_dnd_defaults`])
/// so there is no roster-contact fanout to opt into. The user's own
/// resources fetch DND state via XEP-0060 `<items/>` get on resume.
pub fn pep_features() -> Vec<Feature> {
    vec![
        Feature::pubsub(),
        Feature::pep(),
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
        Feature::mam_extended(),
        Feature::push(),
        Feature::bookmarks_compat(),
        Feature::new("urn:xmpp:bookmarks:1#compat-pep"),
        Feature::vcard4(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    fn make_pubsub_iq(to: Option<&str>) -> Iq {
        let pubsub = Element::builder("pubsub", super::super::stanzas::NS_PUBSUB)
            .append(
                Element::builder("items", super::super::stanzas::NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test")
                    .build(),
            )
            .build();

        Iq::Get {
            from: Some("user@example.com/resource".parse().expect("valid jid")),
            to: to.map(|s| s.parse().expect("valid jid")),
            id: "test-1".to_string(),
            payload: pubsub,
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
        assert!(!PepHandler::is_well_known_node(
            "eu.siacs.conversations.axolotl.devicelist"
        ));
    }

    #[test]
    fn pep_features_do_not_advertise_unimplemented_config_options() {
        let features = pep_features();
        assert!(!features.contains(&Feature::new(
            "http://jabber.org/protocol/pubsub#config-node-max"
        )));
        assert!(!features.contains(&Feature::new("eu.siacs.conversations.axolotl.whitelisted")));
    }
}
