//! Service Discovery: disco#info handling.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::CoreError;

mod features;
#[cfg(test)]
mod tests;

pub use features::Feature;

const DATA_FORMS_NS: &str = "jabber:x:data";
const SERVER_INFO_FORM_TYPE: &str = "urn:xmpp:serverinfo:0";
const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_MUC_SELF_PING_OPTIMIZATION: &str = "http://jabber.org/protocol/muc#self-ping-optimization";

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
    pub lang: Option<String>,
    pub name: Option<String>,
}

impl Identity {
    pub fn new(category: &str, type_: &str, name: Option<&str>) -> Self {
        Self {
            category: category.to_string(),
            type_: type_.to_string(),
            lang: None,
            name: name.map(str::to_string),
        }
    }

    pub fn with_lang(mut self, lang: Option<&str>) -> Self {
        self.lang = lang.map(str::to_string);
        self
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

    pub fn command_list(name: Option<&str>) -> Self {
        Self::new("automation", "command-list", name)
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

        if let Some(ref lang) = identity.lang {
            id_builder = id_builder.attr("xml:lang", lang);
        }

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
        Feature::mam_extended(),
        Feature::waddle_mam_thread(),
        Feature::stanza_ids(),
        Feature::replies(),
        Feature::message_correction(),
        Feature::chat_markers(),
        Feature::receipts(),
        Feature::message_retraction(),
        Feature::reactions(),
        Feature::references(),
        Feature::fallback_indication(),
        Feature::threads(),
        Feature::stream_management(),
        Feature::roster(),
        Feature::carbons(),
        Feature::carbons_rules(),
        Feature::offline_messages(),
        Feature::vcard(),
        Feature::http_upload(),
        Feature::blocking(),
        Feature::last_activity(),
        Feature::ping(),
        Feature::entity_time(),
        Feature::software_version(),
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

/// Get features for the PubSub-backed Spaces service component.
///
/// This intentionally advertises only the PubSub features implemented by the
/// service. Full XEP-0503 service conformance additionally requires owner
/// subscription management, so the service does not advertise
/// `urn:xmpp:spaces:0` until that behavior exists.
pub fn spaces_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::pubsub_retrieve_items(),
        Feature::new("http://jabber.org/protocol/pubsub#subscribe"),
        Feature::new("http://jabber.org/protocol/pubsub#create-nodes"),
        Feature::new("http://jabber.org/protocol/pubsub#config-node"),
        Feature::new("http://jabber.org/protocol/pubsub#meta-data"),
        Feature::new("http://jabber.org/protocol/pubsub#delete-nodes"),
        Feature::new("http://jabber.org/protocol/pubsub#delete-items"),
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
        Feature::muc_self_ping_optimization(),
        Feature::mam(),
        Feature::mam_extended(),
        Feature::fulltext_mam(),
        Feature::waddle_mam_thread(),
        Feature::stanza_ids(),
        Feature::replies(),
        Feature::message_correction(),
        Feature::chat_markers(),
        Feature::chat_states(),
        Feature::message_retraction(),
        Feature::message_moderation(),
        Feature::reactions(),
        Feature::references(),
        Feature::fallback_indication(),
        Feature::threads(),
        Feature::vcard(),
        Feature::occupant_id(),
        Feature::hats(),
        Feature::explicit_mentions(),
        Feature::channel_mentions(),
        Feature::muc_nonanonymous(),
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

    let _ = forum;

    features
}
