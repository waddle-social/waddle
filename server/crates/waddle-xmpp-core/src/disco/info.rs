//! Service Discovery: disco#info handling.

use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::CoreError;

mod features;
#[cfg(test)]
mod tests;

pub use features::Feature;

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

    pub fn pubsub_push(name: Option<&str>) -> Self {
        Self::new("pubsub", "push", name)
    }

    pub fn pubsub_leaf(name: Option<&str>) -> Self {
        Self::new("pubsub", "leaf", name)
    }

    pub fn spaces_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    /// Identity for the community pubsub service that hosts the
    /// XEP-0472 social feed and XEP-0501 stories nodes. Same shape
    /// as a generic pubsub service; the name is what shows up in a
    /// disco#items enumeration of the deployment.
    pub fn community_service(name: Option<&str>) -> Self {
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
    match iq {
        Iq::Get { payload, .. } => payload.name() == "query" && payload.ns() == DISCO_INFO_NS,
        _ => false,
    }
}

/// Parse a disco#info query from an IQ stanza.
pub fn parse_disco_info_query(iq: &Iq) -> Result<DiscoInfoQuery, CoreError> {
    let query_elem = match iq {
        Iq::Get { payload, .. } => {
            if payload.name() == "query" && payload.ns() == DISCO_INFO_NS {
                payload
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
    let target = iq.to().map(|j| j.to_string());

    debug!(target = ?target, node = ?node, "Parsed disco#info query");

    Ok(DiscoInfoQuery { target, node })
}

/// XEP-0004/XEP-0128 jabber:x:data namespace. A disco#info response
/// MAY include one or more `<x xmlns="jabber:x:data" type="result">`
/// extension forms (XEP-0128); their FORM_TYPE values participate in
/// the XEP-0115 §5.1 verification string. Failing to feed them into
/// the recomputation breaks hash verification for clients that emit
/// software-info or capabilities forms.
const NS_DATA_FORMS: &str = "jabber:x:data";

/// Parsed disco#info response payload.
///
/// Surfaces identities, features, and any XEP-0128 `<x/>` extension
/// elements verbatim so XEP-0115 §5.4 hash recomputation can run
/// against the full input that produced the advertised `ver`.
///
/// `ill_formed` is true when the response violates one of XEP-0115
/// §5.4 step 2.4's well-formedness rules — duplicate identities,
/// duplicate features, or multi-FORM_TYPE extension forms. The
/// parser still surfaces what it could read so callers can log
/// diagnostics, but XEP-0115 verification MUST treat such a reply
/// as invalid (do not cache).
#[derive(Debug, Clone)]
pub struct DiscoInfoResponse {
    pub node: Option<String>,
    pub identities: Vec<Identity>,
    pub features: Vec<Feature>,
    /// XEP-0128 service-discovery extension forms preserved verbatim
    /// (typically `<x xmlns="jabber:x:data" type="result">…</x>`).
    pub extensions: Vec<Element>,
    pub ill_formed: bool,
}

/// Parse the `<query/>` element of a disco#info IQ result into typed
/// identities + features + verbatim XEP-0128 extension forms.
pub fn parse_disco_info_response(query: &Element) -> Result<DiscoInfoResponse, CoreError> {
    if query.name() != "query" || query.ns() != DISCO_INFO_NS {
        return Err(CoreError::bad_request(Some(
            "disco#info response payload is not a <query/> element".to_string(),
        )));
    }

    let node = query.attr("node").map(str::to_string);
    let mut identities = Vec::new();
    let mut features = Vec::new();
    let mut extensions = Vec::new();
    // §5.4 step 2.4 well-formedness tracking: duplicate identity
    // tuples (category, type, xml:lang, name), duplicate feature
    // vars, or multi-FORM_TYPE forms invalidate the entire response.
    let mut seen_identities: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();
    let mut seen_features: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ill_formed = false;

    for child in query.children() {
        let child_ns = child.ns();
        match (child.name(), child_ns.as_str()) {
            ("identity", ns) if ns == DISCO_INFO_NS => {
                let category = child
                    .attr("category")
                    .ok_or_else(|| {
                        CoreError::bad_request(Some(
                            "disco#info <identity/> missing category".to_string(),
                        ))
                    })?
                    .to_string();
                let type_ = child
                    .attr("type")
                    .ok_or_else(|| {
                        CoreError::bad_request(Some(
                            "disco#info <identity/> missing type".to_string(),
                        ))
                    })?
                    .to_string();
                let lang = child.attr("xml:lang").map(str::to_string);
                let name = child.attr("name").map(str::to_string);
                let key = (
                    category.clone(),
                    type_.clone(),
                    lang.clone().unwrap_or_default(),
                    name.clone().unwrap_or_default(),
                );
                if !seen_identities.insert(key) {
                    ill_formed = true;
                }
                identities.push(Identity {
                    category,
                    type_,
                    lang,
                    name,
                });
            }
            ("feature", ns) if ns == DISCO_INFO_NS => {
                let var = child.attr("var").ok_or_else(|| {
                    CoreError::bad_request(Some("disco#info <feature/> missing var".to_string()))
                })?;
                if !seen_features.insert(var.to_string()) {
                    ill_formed = true;
                }
                features.push(Feature::new(var));
            }
            ("x", NS_DATA_FORMS) => {
                if has_multiple_form_types(child) {
                    ill_formed = true;
                }
                extensions.push(child.clone());
            }
            _ => {}
        }
    }

    Ok(DiscoInfoResponse {
        node,
        identities,
        features,
        extensions,
        ill_formed,
    })
}

/// XEP-0115 §5.4 step 2.4: a `<x type="result"/>` form with more than
/// one FORM_TYPE field — or one FORM_TYPE field carrying more than
/// one `<value/>` — is malformed. Either case invalidates the whole
/// disco#info response.
fn has_multiple_form_types(form: &Element) -> bool {
    let mut form_type_fields = 0;
    let mut form_type_values = 0;
    for field in form.children().filter(|c| {
        c.name() == "field" && c.ns() == NS_DATA_FORMS && c.attr("var") == Some("FORM_TYPE")
    }) {
        form_type_fields += 1;
        for value in field
            .children()
            .filter(|c| c.name() == "value" && c.ns() == NS_DATA_FORMS)
        {
            form_type_values += 1;
            let _ = value;
        }
    }
    form_type_fields > 1 || form_type_values > 1
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
        query_builder = query_builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), n);
    }

    for identity in identities {
        let mut id_builder = Element::builder("identity", DISCO_INFO_NS)
            .attr(
                minidom::rxml::xml_ncname!("category").to_owned(),
                &identity.category,
            )
            .attr(
                minidom::rxml::xml_ncname!("type").to_owned(),
                &identity.type_,
            );

        if let Some(ref lang) = identity.lang {
            id_builder = id_builder.attr_ns(
                minidom::rxml::Namespace::XML,
                minidom::rxml::xml_ncname!("lang").to_owned(),
                lang,
            );
        }

        if let Some(ref name) = identity.name {
            id_builder = id_builder.attr(minidom::rxml::xml_ncname!("name").to_owned(), name);
        }

        query_builder = query_builder.append(id_builder.build());
    }

    for feature in features {
        query_builder = query_builder.append(
            Element::builder("feature", DISCO_INFO_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), &feature.0)
                .build(),
        );
    }

    for extension in extensions {
        query_builder = query_builder.append(extension.clone());
    }

    let query = query_builder.build();

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(query),
    }
}

/// Get the standard server features.
pub fn server_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::caps(),
        Feature::roster_versioning(),
        Feature::stanza_ids(),
        Feature::replies(),
        Feature::message_correction(),
        Feature::chat_markers(),
        Feature::receipts(),
        Feature::message_retraction(),
        Feature::message_retraction_tombstone(),
        Feature::reactions(),
        Feature::references(),
        Feature::fallback_indication(),
        Feature::stream_management(),
        Feature::roster(),
        Feature::carbons(),
        Feature::carbons_rules(),
        Feature::offline_messages(),
        Feature::vcard(),
        Feature::vcard4(),
        Feature::http_upload(),
        Feature::blocking(),
        Feature::last_activity(),
        Feature::ping(),
        Feature::entity_time(),
        Feature::software_version(),
        Feature::private_storage(),
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

/// Get features for an XEP-0357 Push Service component.
pub fn push_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::pubsub_publish(),
        Feature::pubsub_access_whitelist(),
        Feature::pubsub_publish_only_affiliation(),
        Feature::push(),
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

/// Get features for the community pubsub service component
/// (`community.<domain>`). Hosts the XEP-0472 social feed node and
/// the XEP-0501 stories node; kept separate from the spaces service
/// so the spaces disco#items enumeration only surfaces real
/// community-space bookmarks.
pub fn community_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::pubsub_retrieve_items(),
        Feature::new("http://jabber.org/protocol/pubsub#subscribe"),
        Feature::new("http://jabber.org/protocol/pubsub#publish"),
        Feature::new("http://jabber.org/protocol/pubsub#persistent-items"),
        Feature::new("http://jabber.org/protocol/pubsub#meta-data"),
        Feature::new("http://jabber.org/protocol/pubsub#item-ids"),
        // XEP-0472 Pubsub Social Feed — community feed node lives
        // here at `urn:xmpp:pubsub-social-feed:0`.
        Feature::social_feed(),
        // XEP-0501 Pubsub Stories — community stories node lives
        // here at `urn:xmpp:stories:0`.
        Feature::stories(),
        // xCal community events node — payload is iCalendar in XML
        // (`urn:ietf:params:xml:ns:xcal`) per the XSF ProtoXEP
        // "Calendaring Extensions to Publish-Subscribe".
        Feature::xcal(),
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
        Feature::message_retraction_tombstone(),
        Feature::message_moderation(),
        Feature::reactions(),
        Feature::references(),
        Feature::fallback_indication(),
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
