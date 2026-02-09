//! Service Discovery: disco#info handling.
//!
//! Implements XEP-0030 disco#info for querying entity capabilities.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Service Discovery info namespace (XEP-0030).
pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";

/// Parsed disco#info query.
#[derive(Debug, Clone)]
pub struct DiscoInfoQuery {
    /// Target JID (from IQ 'to' attribute)
    pub target: Option<String>,
    /// Optional node being queried
    pub node: Option<String>,
}

/// Identity element for disco#info response.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Category (e.g., "server", "conference")
    pub category: String,
    /// Type (e.g., "im", "text")
    pub type_: String,
    /// Optional name (human-readable)
    pub name: Option<String>,
}

impl Identity {
    /// Create a new identity.
    pub fn new(category: &str, type_: &str, name: Option<&str>) -> Self {
        Self {
            category: category.to_string(),
            type_: type_.to_string(),
            name: name.map(|s| s.to_string()),
        }
    }

    /// Server identity (category="server", type="im").
    pub fn server(name: Option<&str>) -> Self {
        Self::new("server", "im", name)
    }

    /// MUC service identity (category="conference", type="text").
    pub fn muc_service(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    /// MUC room identity (category="conference", type="text").
    pub fn muc_room(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    /// Upload service identity (category="store", type="file") for XEP-0363.
    pub fn upload_service(name: Option<&str>) -> Self {
        Self::new("store", "file", name)
    }

    /// PubSub service identity (category="pubsub", type="service") for XEP-0060.
    pub fn pubsub_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    /// PubSub leaf node identity (category="pubsub", type="leaf").
    pub fn pubsub_leaf(name: Option<&str>) -> Self {
        Self::new("pubsub", "leaf", name)
    }
}

/// Feature element for disco#info response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature(pub String);

impl Feature {
    /// Create a new feature.
    pub fn new(var: &str) -> Self {
        Self(var.to_string())
    }

    /// disco#info feature
    pub fn disco_info() -> Self {
        Self::new(DISCO_INFO_NS)
    }

    /// disco#items feature
    pub fn disco_items() -> Self {
        Self::new(super::items::DISCO_ITEMS_NS)
    }

    /// MUC feature
    pub fn muc() -> Self {
        Self::new("http://jabber.org/protocol/muc")
    }

    /// MAM feature (v2)
    pub fn mam() -> Self {
        Self::new("urn:xmpp:mam:2")
    }

    /// Stream Management feature
    pub fn stream_management() -> Self {
        Self::new("urn:xmpp:sm:3")
    }

    /// Roster feature
    pub fn roster() -> Self {
        Self::new("jabber:iq:roster")
    }

    /// Message Carbons feature
    pub fn carbons() -> Self {
        Self::new("urn:xmpp:carbons:2")
    }

    /// XEP-0115 Entity Capabilities feature
    pub fn caps() -> Self {
        Self::new("http://jabber.org/protocol/caps")
    }

    /// XEP-0054 vcard-temp feature
    pub fn vcard() -> Self {
        Self::new("vcard-temp")
    }

    /// XEP-0363 HTTP File Upload feature
    pub fn http_upload() -> Self {
        Self::new("urn:xmpp:http:upload:0")
    }

    /// XEP-0191 Blocking Command feature
    pub fn blocking() -> Self {
        Self::new("urn:xmpp:blocking")
    }

    /// XEP-0199 XMPP Ping feature
    pub fn ping() -> Self {
        Self::new("urn:xmpp:ping")
    }

    /// XEP-0352 Client State Indication feature
    pub fn csi() -> Self {
        Self::new("urn:xmpp:csi:0")
    }

    /// XEP-0410 MUC Self-Ping Optimization feature
    pub fn muc_self_ping_optimization() -> Self {
        Self::new("urn:xmpp:muc-selfping:0")
    }

    /// XEP-0060 PubSub feature
    pub fn pubsub() -> Self {
        Self::new("http://jabber.org/protocol/pubsub")
    }

    /// XEP-0163 Personal Eventing Protocol feature
    pub fn pep() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#pep")
    }

    /// PubSub auto-create feature (XEP-0060)
    pub fn pubsub_auto_create() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-create")
    }

    /// PubSub persistent-items feature (XEP-0060)
    pub fn pubsub_persistent_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#persistent-items")
    }

    /// PubSub publish feature (XEP-0060)
    pub fn pubsub_publish() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#publish")
    }

    /// PubSub retrieve-items feature (XEP-0060)
    pub fn pubsub_retrieve_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#retrieve-items")
    }

    /// XEP-0402 PEP Native Bookmarks feature
    pub fn bookmarks2() -> Self {
        Self::new("urn:xmpp:bookmarks:1")
    }

    /// XEP-0402 Bookmarks Compatibility feature
    pub fn bookmarks_compat() -> Self {
        Self::new("urn:xmpp:bookmarks:1#compat")
    }

    /// MUC room features (XEP-0045)
    pub fn muc_persistent() -> Self {
        Self::new("muc_persistent")
    }

    pub fn muc_open() -> Self {
        Self::new("muc_open")
    }

    pub fn muc_membersonly() -> Self {
        Self::new("muc_membersonly")
    }

    pub fn muc_semianonymous() -> Self {
        Self::new("muc_semianonymous")
    }

    pub fn muc_unmoderated() -> Self {
        Self::new("muc_unmoderated")
    }

    pub fn muc_moderated() -> Self {
        Self::new("muc_moderated")
    }

    /// PubSub subscribe feature (XEP-0060)
    pub fn pubsub_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#subscribe")
    }

    /// PubSub access-whitelist feature (XEP-0060/XEP-0223)
    pub fn pubsub_access_whitelist() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-whitelist")
    }

    /// PubSub access-presence feature (XEP-0060)
    pub fn pubsub_access_presence() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-presence")
    }

    /// PubSub auto-subscribe feature (XEP-0060)
    pub fn pubsub_auto_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-subscribe")
    }

    /// PubSub filtered-notifications feature (XEP-0060)
    pub fn pubsub_filtered_notifications() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#filtered-notifications")
    }

    /// Avatar metadata+notify feature (XEP-0084)
    pub fn avatar_metadata_notify() -> Self {
        Self::new("urn:xmpp:avatar:metadata+notify")
    }

    /// PEP vCard conversion feature (XEP-0398)
    pub fn pep_vcard_conversion() -> Self {
        Self::new("urn:xmpp:pep-vcard-conversion:0")
    }

    /// Private XML storage feature (XEP-0049)
    pub fn private_storage() -> Self {
        Self::new("jabber:iq:private")
    }
}

/// Check if an IQ is a disco#info query.
pub fn is_disco_info_query(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => elem.name() == "query" && elem.ns() == DISCO_INFO_NS,
        _ => false,
    }
}

/// Parse a disco#info query from an IQ stanza.
pub fn parse_disco_info_query(iq: &Iq) -> Result<DiscoInfoQuery, XmppError> {
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "query" && elem.ns() == DISCO_INFO_NS {
                elem
            } else {
                return Err(XmppError::bad_request(Some(
                    "Missing disco#info query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(XmppError::bad_request(Some(
                "disco#info must be IQ get".to_string(),
            )))
        }
    };

    let node = query_elem.attr("node").map(|s| s.to_string());
    let target = iq.to.as_ref().map(|j| j.to_string());

    debug!(target = ?target, node = ?node, "Parsed disco#info query");

    Ok(DiscoInfoQuery { target, node })
}

/// Build a disco#info response IQ.
///
/// The response includes identities and features for the queried entity.
pub fn build_disco_info_response(
    original_iq: &Iq,
    identities: &[Identity],
    features: &[Feature],
    node: Option<&str>,
) -> Iq {
    let mut query_builder = Element::builder("query", DISCO_INFO_NS);

    // Add node attribute if present
    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }

    // Add identities
    for identity in identities {
        let mut id_builder = Element::builder("identity", DISCO_INFO_NS)
            .attr("category", &identity.category)
            .attr("type", &identity.type_);

        if let Some(ref name) = identity.name {
            id_builder = id_builder.attr("name", name);
        }

        query_builder = query_builder.append(id_builder.build());
    }

    // Add features
    for feature in features {
        let feat_elem = Element::builder("feature", DISCO_INFO_NS)
            .attr("var", &feature.0)
            .build();
        query_builder = query_builder.append(feat_elem);
    }

    let query = query_builder.build();

    // Build response IQ
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(query)),
    }
}

/// Get the standard server features.
pub fn server_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::mam(),
        Feature::stream_management(),
        Feature::roster(),
        Feature::carbons(),
        Feature::vcard(),
        Feature::http_upload(),
        Feature::blocking(),
        Feature::ping(),
        Feature::csi(),
        Feature::pubsub(),
        Feature::pep(),
        Feature::pubsub_auto_create(),
        Feature::pubsub_persistent_items(),
        Feature::pubsub_publish(),
        Feature::pubsub_retrieve_items(),
        Feature::pubsub_subscribe(),
        Feature::pubsub_access_whitelist(),
        Feature::pubsub_access_presence(),
        Feature::private_storage(),
        Feature::avatar_metadata_notify(),
        Feature::pep_vcard_conversion(),
    ]
}

/// Get features for the upload service component (XEP-0363).
pub fn upload_service_features() -> Vec<Feature> {
    vec![Feature::disco_info(), Feature::http_upload()]
}

/// Get features for the PubSub service component (XEP-0060).
pub fn pubsub_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::pubsub_auto_create(),
        Feature::pubsub_persistent_items(),
        Feature::pubsub_publish(),
        Feature::pubsub_retrieve_items(),
        Feature::pubsub_subscribe(),
        Feature::pubsub_access_whitelist(),
        Feature::pubsub_access_presence(),
    ]
}

/// Get the standard MUC service features.
pub fn muc_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::muc(),
        Feature::muc_self_ping_optimization(),
    ]
}

/// Get features for a MUC room based on configuration.
pub fn muc_room_features(persistent: bool, members_only: bool, moderated: bool) -> Vec<Feature> {
    let mut features = vec![
        Feature::disco_info(),
        Feature::muc(),
        Feature::mam(),
        Feature::muc_semianonymous(),
    ];

    if persistent {
        features.push(Feature::muc_persistent());
    }

    if members_only {
        features.push(Feature::muc_membersonly());
    } else {
        features.push(Feature::muc_open());
    }

    if moderated {
        features.push(Feature::muc_moderated());
    } else {
        features.push(Feature::muc_unmoderated());
    }

    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_disco_info_query() {
        let query_elem = Element::builder("query", DISCO_INFO_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(is_disco_info_query(&iq));
    }

    #[test]
    fn test_is_not_disco_info_query_wrong_ns() {
        let query_elem = Element::builder("query", "some:other:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-2".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        assert!(!is_disco_info_query(&iq));
    }

    #[test]
    fn test_is_not_disco_info_query_set() {
        let query_elem = Element::builder("query", DISCO_INFO_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "test-3".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(query_elem),
        };

        assert!(!is_disco_info_query(&iq));
    }

    #[test]
    fn test_build_disco_info_response() {
        let query_elem = Element::builder("query", DISCO_INFO_NS).build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("server.example.com".parse().unwrap()),
            id: "disco-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let identities = vec![Identity::server(Some("Test Server"))];
        let features = vec![Feature::disco_info(), Feature::disco_items()];

        let response = build_disco_info_response(&iq, &identities, &features, None);

        assert_eq!(response.id, "disco-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));
    }

    #[test]
    fn test_identity_constructors() {
        let server = Identity::server(Some("My Server"));
        assert_eq!(server.category, "server");
        assert_eq!(server.type_, "im");
        assert_eq!(server.name, Some("My Server".to_string()));

        let muc = Identity::muc_service(Some("MUC Service"));
        assert_eq!(muc.category, "conference");
        assert_eq!(muc.type_, "text");
    }

    #[test]
    fn test_server_features() {
        let features = server_features();
        assert!(features.contains(&Feature::disco_info()));
        assert!(features.contains(&Feature::disco_items()));
        assert!(features.contains(&Feature::mam()));
        assert!(features.contains(&Feature::stream_management()));
    }

    #[test]
    fn test_muc_room_features() {
        let features = muc_room_features(true, true, false);
        assert!(features.contains(&Feature::muc()));
        assert!(features.contains(&Feature::muc_persistent()));
        assert!(features.contains(&Feature::muc_membersonly()));
        assert!(features.contains(&Feature::muc_unmoderated()));
    }
}
