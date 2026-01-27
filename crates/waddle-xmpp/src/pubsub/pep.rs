//! XEP-0163: Personal Eventing Protocol (PEP) handler.
//!
//! PEP is a simplified profile of PubSub that uses the bare JID as the
//! service address. This module handles PEP-specific logic.

use jid::BareJid;
use xmpp_parsers::iq::Iq;

use super::stanzas::{is_pubsub_iq, NS_PUBSUB};

/// Check if an IQ is a PEP request.
///
/// A PEP request is a PubSub IQ sent to the user's own bare JID.
/// Returns true if:
/// - The IQ is a PubSub IQ
/// - The 'to' address is either absent (implicit self) or the user's bare JID
pub fn is_pep_request(iq: &Iq, user_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    // PEP: 'to' is either absent (implicit) or the user's own bare JID
    match &iq.to {
        None => true, // Implicit PEP (to self)
        Some(to_jid) => to_jid.to_bare() == *user_jid,
    }
}

/// Check if a PubSub IQ is addressed to a specific JID as a PEP service.
///
/// This is used to detect when a user is querying another user's PEP service.
pub fn is_pep_request_to(iq: &Iq, target_jid: &BareJid) -> bool {
    if !is_pubsub_iq(iq) {
        return false;
    }

    match &iq.to {
        Some(to_jid) => to_jid.to_bare() == *target_jid,
        None => false,
    }
}

/// Handler for PEP requests.
///
/// This struct contains the logic for processing PEP-specific operations.
pub struct PepHandler;

impl PepHandler {
    /// Check if a node name is a well-known PEP node.
    ///
    /// Well-known PEP nodes are defined by various XEPs and have
    /// special handling rules.
    pub fn is_well_known_node(node: &str) -> bool {
        // XEP-0402 Bookmarks
        node == "urn:xmpp:bookmarks:1" ||
        // XEP-0084 User Avatar
        node == "urn:xmpp:avatar:data" ||
        node == "urn:xmpp:avatar:metadata" ||
        // XEP-0172 User Nickname
        node == "http://jabber.org/protocol/nick" ||
        // XEP-0107 User Mood
        node == "http://jabber.org/protocol/mood" ||
        // XEP-0108 User Activity
        node == "http://jabber.org/protocol/activity" ||
        // XEP-0118 User Tune
        node == "http://jabber.org/protocol/tune" ||
        // XEP-0080 User Location
        node == "http://jabber.org/protocol/geoloc" ||
        // XEP-0277 Microblogging
        node == "urn:xmpp:microblog:0" ||
        // XEP-0384 OMEMO
        node.starts_with("eu.siacs.conversations.axolotl")
    }

    /// Get the default access model for a node.
    ///
    /// Some well-known nodes have specific default access models.
    pub fn default_access_model_for_node(node: &str) -> super::node::AccessModel {
        use super::node::AccessModel;

        // Bookmarks should be private (only owner can access)
        if node == "urn:xmpp:bookmarks:1" {
            return AccessModel::Whitelist;
        }

        // OMEMO device lists should be presence-based
        if node.starts_with("eu.siacs.conversations.axolotl") {
            return AccessModel::Open;
        }

        // Default PEP access model
        AccessModel::Presence
    }
}

/// Build the PEP service discovery identity.
pub fn build_pep_identity() -> crate::disco::Identity {
    crate::disco::Identity::new(
        "pubsub",
        "pep",
        Some("Personal Eventing Protocol"),
    )
}

/// Get features advertised by the PEP service.
pub fn pep_features() -> Vec<crate::disco::Feature> {
    use crate::disco::Feature;

    vec![
        Feature::new(NS_PUBSUB),
        Feature::new(&format!("{}#access-presence", NS_PUBSUB)),
        Feature::new(&format!("{}#auto-create", NS_PUBSUB)),
        Feature::new(&format!("{}#auto-subscribe", NS_PUBSUB)),
        Feature::new(&format!("{}#filtered-notifications", NS_PUBSUB)),
        Feature::new(&format!("{}#persistent-items", NS_PUBSUB)),
        Feature::new(&format!("{}#publish", NS_PUBSUB)),
        Feature::new(&format!("{}#retrieve-items", NS_PUBSUB)),
        Feature::new(&format!("{}#subscribe", NS_PUBSUB)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::iq::IqType;

    fn make_pubsub_iq(to: Option<&str>) -> Iq {
        let pubsub = Element::builder("pubsub", NS_PUBSUB)
            .append(Element::builder("items", NS_PUBSUB).attr("node", "test").build())
            .build();

        Iq {
            from: Some("user@example.com/resource".parse().expect("valid jid")),
            to: to.map(|s| s.parse().expect("valid jid")),
            id: "test-1".to_string(),
            payload: IqType::Get(pubsub),
        }
    }

    #[test]
    fn test_is_pep_request_implicit() {
        let iq = make_pubsub_iq(None);
        let user_jid: BareJid = "user@example.com".parse().expect("valid jid");

        assert!(is_pep_request(&iq, &user_jid));
    }

    #[test]
    fn test_is_pep_request_explicit_self() {
        let iq = make_pubsub_iq(Some("user@example.com"));
        let user_jid: BareJid = "user@example.com".parse().expect("valid jid");

        assert!(is_pep_request(&iq, &user_jid));
    }

    #[test]
    fn test_is_pep_request_to_other() {
        let iq = make_pubsub_iq(Some("other@example.com"));
        let user_jid: BareJid = "user@example.com".parse().expect("valid jid");

        // This is a PEP request to another user, not self
        assert!(!is_pep_request(&iq, &user_jid));
    }

    #[test]
    fn test_is_well_known_node() {
        assert!(PepHandler::is_well_known_node("urn:xmpp:bookmarks:1"));
        assert!(PepHandler::is_well_known_node("urn:xmpp:avatar:data"));
        assert!(PepHandler::is_well_known_node("eu.siacs.conversations.axolotl.devicelist"));
        assert!(!PepHandler::is_well_known_node("custom:node"));
    }

    #[test]
    fn test_default_access_model() {
        use super::super::node::AccessModel;

        // Bookmarks should be private
        assert_eq!(
            PepHandler::default_access_model_for_node("urn:xmpp:bookmarks:1"),
            AccessModel::Whitelist
        );

        // OMEMO should be open
        assert_eq!(
            PepHandler::default_access_model_for_node("eu.siacs.conversations.axolotl.devicelist"),
            AccessModel::Open
        );

        // Default is presence
        assert_eq!(
            PepHandler::default_access_model_for_node("some:custom:node"),
            AccessModel::Presence
        );
    }
}
