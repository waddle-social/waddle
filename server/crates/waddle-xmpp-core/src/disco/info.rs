//! Service Discovery: disco#info handling.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::CoreError;

const DATA_FORMS_NS: &str = "jabber:x:data";
const SERVER_INFO_FORM_TYPE: &str = "urn:xmpp:serverinfo:0";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_FORUMS: &str = "urn:xmpp:forums:0";

/// Service Discovery info namespace (XEP-0030).
pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";

/// Parsed disco#info query.
#[derive(Debug, Clone)]
pub struct DiscoInfoQuery {
    pub target: Option<String>,
    pub node: Option<String>,
}

/// Identity element for disco#info response.
#[derive(Debug, Clone)]
pub struct Identity {
    pub category: String,
    pub type_: String,
    pub name: Option<String>,
}

impl Identity {
    pub fn new(category: &str, type_: &str, name: Option<&str>) -> Self {
        Self {
            category: category.to_string(),
            type_: type_.to_string(),
            name: name.map(str::to_string),
        }
    }

    pub fn server(name: Option<&str>) -> Self {
        Self::new("server", "im", name)
    }

    pub fn muc_service(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    pub fn muc_room(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    pub fn upload_service(name: Option<&str>) -> Self {
        Self::new("store", "file", name)
    }

    pub fn pubsub_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    pub fn pubsub_leaf(name: Option<&str>) -> Self {
        Self::new("pubsub", "leaf", name)
    }

    pub fn spaces_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    pub fn automation(name: Option<&str>) -> Self {
        Self::new("automation", "command-node", name)
    }
}

/// Feature element for disco#info response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature(pub String);

impl Feature {
    pub fn new(var: &str) -> Self {
        Self(var.to_string())
    }

    pub fn disco_info() -> Self {
        Self::new(DISCO_INFO_NS)
    }

    pub fn disco_items() -> Self {
        Self::new(super::items::DISCO_ITEMS_NS)
    }

    pub fn muc() -> Self {
        Self::new("http://jabber.org/protocol/muc")
    }

    pub fn mam() -> Self {
        Self::new("urn:xmpp:mam:2")
    }

    pub fn replies() -> Self {
        Self::new("urn:xmpp:reply:0")
    }

    pub fn fallback_indication() -> Self {
        Self::new("urn:xmpp:fallback:0")
    }

    pub fn threads() -> Self {
        Self::new("urn:xmpp:threads:0")
    }

    pub fn stream_management() -> Self {
        Self::new("urn:xmpp:sm:3")
    }

    pub fn roster() -> Self {
        Self::new("jabber:iq:roster")
    }

    pub fn carbons() -> Self {
        Self::new("urn:xmpp:carbons:2")
    }

    pub fn caps() -> Self {
        Self::new("http://jabber.org/protocol/caps")
    }

    pub fn roster_versioning() -> Self {
        Self::new("urn:xmpp:features:rosterver")
    }

    pub fn vcard() -> Self {
        Self::new("vcard-temp")
    }

    pub fn http_upload() -> Self {
        Self::new("urn:xmpp:http:upload:0")
    }

    pub fn socks5_bytestreams() -> Self {
        Self::new("http://jabber.org/protocol/bytestreams")
    }

    pub fn last_activity() -> Self {
        Self::new("jabber:iq:last")
    }

    pub fn blocking() -> Self {
        Self::new("urn:xmpp:blocking")
    }

    pub fn ping() -> Self {
        Self::new("urn:xmpp:ping")
    }

    pub fn entity_time() -> Self {
        Self::new("urn:xmpp:time")
    }

    pub fn software_version() -> Self {
        Self::new("jabber:iq:version")
    }

    pub fn ibb() -> Self {
        Self::new("http://jabber.org/protocol/ibb")
    }

    pub fn commands() -> Self {
        Self::new(NS_COMMANDS)
    }

    pub fn csi() -> Self {
        Self::new("urn:xmpp:csi:0")
    }

    pub fn muc_self_ping_optimization() -> Self {
        Self::new("urn:xmpp:muc-selfping:0")
    }

    pub fn pubsub() -> Self {
        Self::new("http://jabber.org/protocol/pubsub")
    }

    pub fn pep() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#pep")
    }

    pub fn pubsub_auto_create() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-create")
    }

    pub fn pubsub_persistent_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#persistent-items")
    }

    pub fn pubsub_publish() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#publish")
    }

    pub fn pubsub_retrieve_items() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#retrieve-items")
    }

    pub fn bookmarks2() -> Self {
        Self::new("urn:xmpp:bookmarks:1")
    }

    pub fn bookmarks_compat() -> Self {
        Self::new("urn:xmpp:bookmarks:1#compat")
    }

    pub fn carbons_rules() -> Self {
        Self::new("urn:xmpp:carbons:rules:0")
    }

    pub fn offline_messages() -> Self {
        Self::new("msgoffline")
    }

    pub fn server_info() -> Self {
        Self::new(SERVER_INFO_FORM_TYPE)
    }

    pub fn occupant_id() -> Self {
        Self::new("urn:xmpp:occupant-id:0")
    }

    pub fn hats() -> Self {
        Self::new("urn:xmpp:hats:0")
    }

    pub fn explicit_mentions() -> Self {
        Self::new("urn:xmpp:mentions:0")
    }

    pub fn channel_mentions() -> Self {
        Self::new("urn:xmpp:mentions:0#channel")
    }

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

    pub fn muc_nonanonymous() -> Self {
        Self::new("muc_nonanonymous")
    }

    pub fn muc_unmoderated() -> Self {
        Self::new("muc_unmoderated")
    }

    pub fn muc_moderated() -> Self {
        Self::new("muc_moderated")
    }

    pub fn pubsub_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#subscribe")
    }

    pub fn pubsub_access_whitelist() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-whitelist")
    }

    pub fn pubsub_access_presence() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#access-presence")
    }

    pub fn pubsub_auto_subscribe() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#auto-subscribe")
    }

    pub fn pubsub_filtered_notifications() -> Self {
        Self::new("http://jabber.org/protocol/pubsub#filtered-notifications")
    }

    pub fn avatar_metadata_notify() -> Self {
        Self::new("urn:xmpp:avatar:metadata+notify")
    }

    pub fn pep_vcard_conversion() -> Self {
        Self::new("urn:xmpp:pep-vcard-conversion:0")
    }

    pub fn private_storage() -> Self {
        Self::new("jabber:iq:private")
    }

    pub fn spaces() -> Self {
        Self::new("urn:xmpp:spaces:0")
    }

    pub fn forums() -> Self {
        Self::new(NS_FORUMS)
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
pub fn parse_disco_info_query(iq: &Iq) -> Result<DiscoInfoQuery, CoreError> {
    let query_elem = match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            if elem.name() == "query" && elem.ns() == DISCO_INFO_NS {
                elem
            } else {
                return Err(CoreError::bad_request(Some(
                    "Missing disco#info query element".to_string(),
                )));
            }
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "disco#info must be IQ get".to_string(),
            )))
        }
    };

    let node = query_elem.attr("node").map(str::to_string);
    let target = iq.to.as_ref().map(|j| j.to_string());

    debug!(target = ?target, node = ?node, "Parsed disco#info query");

    Ok(DiscoInfoQuery { target, node })
}

/// Build a disco#info response IQ.
pub fn build_disco_info_response(
    original_iq: &Iq,
    identities: &[Identity],
    features: &[Feature],
    node: Option<&str>,
) -> Iq {
    build_disco_info_response_with_extensions(original_iq, identities, features, node, &[])
}

/// Build a disco#info response IQ with extension payloads.
pub fn build_disco_info_response_with_extensions(
    original_iq: &Iq,
    identities: &[Identity],
    features: &[Feature],
    node: Option<&str>,
    extensions: &[Element],
) -> Iq {
    let mut query_builder = Element::builder("query", DISCO_INFO_NS);

    if let Some(n) = node {
        query_builder = query_builder.attr("node", n);
    }

    for identity in identities {
        let mut id_builder = Element::builder("identity", DISCO_INFO_NS)
            .attr("category", &identity.category)
            .attr("type", &identity.type_);

        if let Some(ref name) = identity.name {
            id_builder = id_builder.attr("name", name);
        }

        query_builder = query_builder.append(id_builder.build());
    }

    for feature in features {
        query_builder = query_builder.append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr("var", &feature.0)
                .build(),
        );
    }

    for extension in extensions {
        query_builder = query_builder.append(extension.clone());
    }

    let query = query_builder.build();

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(query)),
    }
}

/// Build a server-info data form with an abuse contact (XEP-0157/XEP-0485).
pub fn build_server_info_abuse_form(domain: &str) -> Element {
    let abuse_address = format!("mailto:abuse@{}", domain);

    Element::builder("x", DATA_FORMS_NS)
        .attr("type", "result")
        .append(data_form_field(
            "FORM_TYPE",
            Some("hidden"),
            SERVER_INFO_FORM_TYPE,
        ))
        .append(data_form_field(
            "abuse-addresses",
            Some("text-single"),
            &abuse_address,
        ))
        .build()
}

fn data_form_field(var: &str, field_type: Option<&str>, value: &str) -> Element {
    let mut builder = Element::builder("field", DATA_FORMS_NS).attr("var", var);

    if let Some(field_type) = field_type {
        builder = builder.attr("type", field_type);
    }

    builder
        .append(
            Element::builder("value", DATA_FORMS_NS)
                .append(value)
                .build(),
        )
        .build()
}

/// Get the standard server features.
pub fn server_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::caps(),
        Feature::roster_versioning(),
        Feature::mam(),
        Feature::replies(),
        Feature::fallback_indication(),
        Feature::threads(),
        Feature::stream_management(),
        Feature::roster(),
        Feature::carbons(),
        Feature::carbons_rules(),
        Feature::offline_messages(),
        Feature::vcard(),
        Feature::http_upload(),
        Feature::socks5_bytestreams(),
        Feature::blocking(),
        Feature::last_activity(),
        Feature::ping(),
        Feature::entity_time(),
        Feature::software_version(),
        Feature::server_info(),
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
        Feature::new(NS_COMMANDS),
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

/// Get features for the Spaces service component (XEP-0503).
pub fn spaces_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::spaces(),
        Feature::pubsub_retrieve_items(),
        Feature::new("http://jabber.org/protocol/pubsub#subscribe"),
        Feature::new("http://jabber.org/protocol/pubsub#create-nodes"),
        Feature::new("http://jabber.org/protocol/pubsub#config-node"),
        Feature::new("http://jabber.org/protocol/pubsub#meta-data"),
        Feature::new("http://jabber.org/protocol/pubsub#retract-items"),
        Feature::new("http://jabber.org/protocol/pubsub#multi-items"),
        Feature::new("http://jabber.org/protocol/pubsub#item-ids"),
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
pub fn muc_room_features(
    persistent: bool,
    members_only: bool,
    moderated: bool,
    forum: bool,
) -> Vec<Feature> {
    let mut features = vec![
        Feature::disco_info(),
        Feature::muc(),
        Feature::mam(),
        Feature::replies(),
        Feature::fallback_indication(),
        Feature::threads(),
        Feature::vcard(),
        Feature::occupant_id(),
        Feature::hats(),
        Feature::explicit_mentions(),
        Feature::channel_mentions(),
        Feature::muc_nonanonymous(),
    ];

    if forum {
        features.push(Feature::forums());
    }

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
    fn test_parse_disco_info_query() {
        let query_elem = Element::builder("query", DISCO_INFO_NS)
            .attr("node", "caps#hash")
            .build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "test-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let query = parse_disco_info_query(&iq).unwrap();
        assert_eq!(query.target.as_deref(), Some("example.com"));
        assert_eq!(query.node.as_deref(), Some("caps#hash"));
    }

    #[test]
    fn test_build_disco_info_response() {
        let query_elem = Element::builder("query", DISCO_INFO_NS).build();
        let iq = Iq {
            from: Some("user@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "disco-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(query_elem),
        };

        let response = build_disco_info_response(
            &iq,
            &[Identity::server(Some("Waddle"))],
            &[Feature::disco_info(), Feature::disco_items()],
            None,
        );

        assert_eq!(response.id, "disco-1");
        assert!(matches!(
            response.payload,
            xmpp_parsers::iq::IqType::Result(Some(_))
        ));
    }

    #[test]
    fn test_build_server_info_abuse_form() {
        let form = build_server_info_abuse_form("example.com");
        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), DATA_FORMS_NS);
        assert_eq!(form.attr("type"), Some("result"));
        let form_type = form
            .children()
            .find(|child| child.attr("var") == Some("FORM_TYPE"))
            .and_then(|child| child.get_child("value", DATA_FORMS_NS))
            .expect("FORM_TYPE field should be present");
        assert_eq!(form_type.text(), SERVER_INFO_FORM_TYPE);

        let abuse_addresses = form
            .children()
            .find(|child| child.attr("var") == Some("abuse-addresses"))
            .and_then(|child| child.get_child("value", DATA_FORMS_NS))
            .expect("abuse-addresses field should be present");
        assert_eq!(abuse_addresses.text(), "mailto:abuse@example.com");
    }

    #[test]
    fn test_server_features_include_core_features() {
        let features = server_features();
        assert!(features.contains(&Feature::disco_info()));
        assert!(features.contains(&Feature::carbons()));
        assert!(features.contains(&Feature::server_info()));
    }

    #[test]
    fn test_muc_room_features_forum_room() {
        let features = muc_room_features(true, true, false, true);
        assert!(features.contains(&Feature::forums()));
        assert!(features.contains(&Feature::muc_persistent()));
        assert!(features.contains(&Feature::muc_membersonly()));
        assert!(features.contains(&Feature::muc_unmoderated()));
    }
}
