//! Implementation behind the parent module facade.

use super::debug;
use super::CoreError;
use super::Element;
use super::Feature;
use super::HashSet;
use super::Iq;

pub(in crate::disco::info) const SERVER_INFO_FORM_TYPE: &str = "urn:xmpp:serverinfo:0";
pub(in crate::disco::info) const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";
pub(in crate::disco::info) const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
pub(in crate::disco::info) const NS_MUC_SELF_PING_OPTIMIZATION: &str =
    "http://jabber.org/protocol/muc#self-ping-optimization";

/// Service Discovery info namespace (XEP-0030).
pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";

/// Parsed disco#info query.
#[derive(Debug, Clone)]
pub struct DiscoInfoQuery {
    /// Optional destination JID supplied by the request.
    pub target: Option<String>,
    /// PubSub or discovery node selected by the request.
    pub node: Option<String>,
}

/// Identity element for disco#info response.
#[derive(Debug, Clone)]
pub struct Identity {
    /// XEP-0030 identity category.
    pub category: String,
    /// Protocol identity subtype within the category.
    pub type_: String,
    /// Language tag for the advertised name.
    pub lang: Option<String>,
    /// Human-readable name advertised to clients.
    pub name: Option<String>,
}

impl Identity {
    /// Create a discovery identity with a category, subtype, optional name, and no language tag.
    #[must_use]
    pub fn new(category: &str, type_: &str, name: Option<&str>) -> Self {
        Self {
            category: category.to_string(),
            type_: type_.to_string(),
            lang: None,
            name: name.map(str::to_string),
        }
    }

    /// Set the lang and return the updated value.
    #[must_use]
    pub fn with_lang(mut self, lang: Option<&str>) -> Self {
        self.lang = lang.map(str::to_string);
        self
    }

    /// Create an identity advertising category `server` and type `im` with the supplied display name.
    #[must_use]
    pub fn server(name: Option<&str>) -> Self {
        Self::new("server", "im", name)
    }

    /// Create an identity advertising category `conference` and type `text` with the supplied display name.
    #[must_use]
    pub fn muc_service(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    /// Create an identity advertising category `conference` and type `text` with the supplied display name.
    #[must_use]
    pub fn muc_room(name: Option<&str>) -> Self {
        Self::new("conference", "text", name)
    }

    /// XEP-0272 / av-conferences ProtoXEP convention: the SFU mixer
    /// JID advertises a `<identity category='conference'
    /// type='audio-video'/>` so a strict client can disco#info the
    /// mixer and discover Muji support before sending a
    /// session-initiate.
    #[must_use]
    pub fn audio_video_conference(name: Option<&str>) -> Self {
        Self::new("conference", "audio-video", name)
    }

    /// Create an identity advertising category `store` and type `file` with the supplied display name.
    #[must_use]
    pub fn upload_service(name: Option<&str>) -> Self {
        Self::new("store", "file", name)
    }

    /// Create an identity advertising category `pubsub` and type `service` with the supplied display name.
    #[must_use]
    pub fn pubsub_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    /// Create an identity advertising category `pubsub` and type `push` with the supplied display name.
    #[must_use]
    pub fn pubsub_push(name: Option<&str>) -> Self {
        Self::new("pubsub", "push", name)
    }

    /// Create an identity advertising category `pubsub` and type `leaf` with the supplied display name.
    #[must_use]
    pub fn pubsub_leaf(name: Option<&str>) -> Self {
        Self::new("pubsub", "leaf", name)
    }

    /// Create an identity advertising category `pubsub` and type `service` with the supplied display name.
    #[must_use]
    pub fn spaces_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    /// Identity for the community pubsub service that hosts the
    /// XEP-0472 social feed and XEP-0501 stories nodes. Same shape
    /// as a generic pubsub service; the name is what shows up in a
    /// disco#items enumeration of the deployment.
    #[must_use]
    pub fn community_service(name: Option<&str>) -> Self {
        Self::new("pubsub", "service", name)
    }

    /// Create an identity advertising category `automation` and type `command-node` with the supplied display name.
    #[must_use]
    pub fn automation(name: Option<&str>) -> Self {
        Self::new("automation", "command-node", name)
    }

    /// XEP-0030 registry `client/bot`: an automated client.
    #[must_use]
    pub fn client_bot(name: Option<&str>) -> Self {
        Self::new("client", "bot", name)
    }

    /// Create an identity advertising category `automation` and type `command-list` with the supplied display name.
    #[must_use]
    pub fn command_list(name: Option<&str>) -> Self {
        Self::new("automation", "command-list", name)
    }
}

/// Check if an IQ is a disco#info query.
#[must_use]
pub fn is_disco_info_query(iq: &Iq) -> bool {
    match iq {
        Iq::Get { payload, .. } => payload.name() == "query" && payload.ns() == DISCO_INFO_NS,
        _ => false,
    }
}

/// Parse a disco#info query from an IQ stanza.
///
/// # Errors
///
/// Returns `CoreError::BadRequest` unless this is an IQ-get containing a disco#info query.
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
pub(in crate::disco::info) const NS_DATA_FORMS: &str = "jabber:x:data";

/// Parsed disco#info response payload.
///
/// Surfaces identities, features, and any XEP-0128 `<x/>` extension
/// elements verbatim so XEP-0115 §5.4 hash recomputation can run
/// against the full input that produced the advertised `ver`.
///
/// `ill_formed` is true when the response violates one of XEP-0115
/// §5.4 step 2.4's well-formedness rules — duplicate identities,
/// duplicate features, multi-FORM_TYPE extension forms, or repeated
/// FORM_TYPE values across extension forms. The
/// parser still surfaces what it could read so callers can log
/// diagnostics, but XEP-0115 verification MUST treat such a reply
/// as invalid (do not cache).
#[derive(Debug, Clone)]
pub struct DiscoInfoResponse {
    /// PubSub or discovery node selected by the request.
    pub node: Option<String>,
    /// Service identities advertised in the discovery response.
    pub identities: Vec<Identity>,
    /// Protocol feature namespaces advertised by the entity.
    pub features: Vec<Feature>,
    /// XEP-0128 service-discovery extension forms preserved verbatim
    /// (typically `<x xmlns="jabber:x:data" type="result">…</x>`).
    pub extensions: Vec<Element>,
    /// Whether duplicate identities/features or invalid FORM_TYPE cardinality make this response unsuitable for capability verification.
    pub ill_formed: bool,
}

/// Parse the `<query/>` element of a disco#info IQ result into typed
/// identities + features + verbatim XEP-0128 extension forms.
///
/// # Errors
///
/// Returns `CoreError::BadRequest` for the wrong query root or identities/features missing their required attributes.
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
    let mut seen_identities: HashSet<(String, String, String, String)> = HashSet::new();
    let mut seen_features: HashSet<String> = HashSet::new();
    let mut seen_form_types: HashSet<String> = HashSet::new();
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
                } else if let Some(form_type) = caps_form_type(child) {
                    if !seen_form_types.insert(form_type) {
                        ill_formed = true;
                    }
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
pub(in crate::disco::info) fn has_multiple_form_types(form: &Element) -> bool {
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

/// Return the FORM_TYPE used by a well-formed XEP-0128 result form.
/// Forms with an absent, non-hidden, or valueless FORM_TYPE do not
/// participate in XEP-0115 verification and are ignored per §5.4.
pub(in crate::disco::info) fn caps_form_type(form: &Element) -> Option<String> {
    if form.attr("type") != Some("result") {
        return None;
    }

    let field = form.children().find(|child| {
        child.name() == "field"
            && child.ns() == NS_DATA_FORMS
            && child.attr("var") == Some("FORM_TYPE")
    })?;
    if field.attr("type") != Some("hidden") {
        return None;
    }

    field
        .children()
        .find(|child| child.name() == "value" && child.ns() == NS_DATA_FORMS)
        .map(Element::text)
        .filter(|value| !value.trim().is_empty())
}

/// Build a disco#info response IQ.
#[must_use]
pub fn build_disco_info_response(
    original_iq: &Iq,
    identities: &[Identity],
    features: &[Feature],
    node: Option<&str>,
) -> Iq {
    build_disco_info_response_with_extensions(original_iq, identities, features, node, &[])
}

/// Build a disco#info response IQ with extension payloads.
#[must_use]
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

    // XEP-0115 §5.4 step 2.4 well-formedness: a disco#info response with
    // duplicate identity tuples or duplicate feature vars is ill-formed and
    // caps-verifying clients MUST discard it (#1259). Dedupe here so no
    // call site can compose an ill-formed response; first occurrence wins,
    // preserving caller ordering.
    let mut seen_identities: HashSet<(&str, &str, Option<&str>, Option<&str>)> = HashSet::new();
    let mut seen_features: HashSet<&str> = HashSet::new();

    for identity in identities {
        if !seen_identities.insert((
            identity.category.as_str(),
            identity.type_.as_str(),
            identity.lang.as_deref(),
            identity.name.as_deref(),
        )) {
            continue;
        }
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
        if !seen_features.insert(feature.0.as_str()) {
            continue;
        }
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
#[must_use]
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
        // XEP-0184 receipts are deliberately NOT advertised: the disco
        // feature announces "I will generate acks", which is the
        // receiving client's job. The server only routes `<request/>`
        // and `<received/>` payloads verbatim (#1247 removed the
        // server-side receipt fabricator).
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
#[must_use]
pub fn upload_service_features() -> Vec<Feature> {
    vec![Feature::disco_info(), Feature::http_upload()]
}

/// Get features for the PubSub service component (XEP-0060).
#[must_use]
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
///
/// Includes the XEP-0050 `http://jabber.org/protocol/commands` feature
/// so the chat / mobile client can discover that device registration
/// flows through ad-hoc commands rather than via a custom-namespace
/// IQ. The two registered command nodes (`register-device` and
/// `disable-device`) are surfaced via disco#items on
/// `push.<domain>?node=http://jabber.org/protocol/commands`.
#[must_use]
pub fn push_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::pubsub(),
        Feature::pubsub_publish(),
        Feature::pubsub_access_whitelist(),
        Feature::pubsub_publish_only_affiliation(),
        Feature::push(),
        Feature::commands(),
    ]
}

/// Get features for the PubSub-backed Spaces service component.
#[must_use]
pub fn spaces_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::spaces(),
        Feature::pubsub(),
        Feature::pubsub_retrieve_items(),
        Feature::new("http://jabber.org/protocol/pubsub#subscribe"),
        Feature::new("http://jabber.org/protocol/pubsub#publish"),
        Feature::new("http://jabber.org/protocol/pubsub#persistent-items"),
        Feature::new("http://jabber.org/protocol/pubsub#create-nodes"),
        Feature::new("http://jabber.org/protocol/pubsub#config-node"),
        Feature::new("http://jabber.org/protocol/pubsub#meta-data"),
        Feature::new("http://jabber.org/protocol/pubsub#delete-nodes"),
        Feature::new("http://jabber.org/protocol/pubsub#delete-items"),
        Feature::new("http://jabber.org/protocol/pubsub#retract-items"),
        Feature::new("http://jabber.org/protocol/pubsub#item-ids"),
        Feature::new("http://jabber.org/protocol/pubsub#multi-items"),
        Feature::new("http://jabber.org/protocol/pubsub#manage-subscriptions"),
        Feature::new("http://jabber.org/protocol/pubsub#modify-affiliations"),
        Feature::new("http://jabber.org/protocol/pubsub#retrieve-affiliations"),
    ]
}

/// Get features for the community pubsub service component
/// (`community.<domain>`). Hosts the XEP-0472 social feed node and
/// the XEP-0501 stories node; kept separate from the spaces service
/// so the spaces disco#items enumeration only surfaces real
/// community-space bookmarks.
#[must_use]
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
        // here at `urn:xmpp:pubsub-social-feed:1`.
        Feature::social_feed(),
        // XEP-0501 Pubsub Stories — community stories node lives
        // here at `urn:xmpp:pubsub-social-feed:stories:0`.
        Feature::stories(),
        // xCal community events node — payload is iCalendar in XML
        // (`urn:ietf:params:xml:ns:xcal`) per the XSF ProtoXEP
        // "Calendaring Extensions to Publish-Subscribe".
        Feature::xcal(),
    ]
}

/// Get the standard MUC service features.
#[must_use]
pub fn muc_service_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::muc(),
        // XEP-0045 §7.4: reflected groupchat messages keep the sender's
        // original `id` (enforced in the room dispatch path), so the
        // service advertises stable-id support (#1265 item 14).
        Feature::muc_stable_id(),
    ]
}

/// Lifetime advertised for a MUC room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPersistence {
    /// The room is temporary.
    Temporary,
    /// The room is persistent.
    Persistent,
}

impl RoomPersistence {
    /// Convert the stored room configuration flag into its advertised mode.
    #[must_use]
    pub fn from_persistent_flag(persistent: bool) -> Self {
        if persistent {
            Self::Persistent
        } else {
            Self::Temporary
        }
    }
}

/// Admission policy advertised for a MUC room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomMembership {
    /// The room is open.
    Open,
    /// Joining the room requires membership.
    MembersOnly,
}

impl RoomMembership {
    /// Convert the stored room configuration flag into its advertised mode.
    #[must_use]
    pub fn from_members_only_flag(members_only: bool) -> Self {
        if members_only {
            Self::MembersOnly
        } else {
            Self::Open
        }
    }
}

/// Discovery visibility advertised for a MUC room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomVisibility {
    /// The room is hidden.
    Hidden,
    /// The room is public.
    Public,
}

impl RoomVisibility {
    /// Convert the stored room configuration flag into its advertised mode.
    #[must_use]
    pub fn from_public_room_flag(public_room: bool) -> Self {
        if public_room {
            Self::Public
        } else {
            Self::Hidden
        }
    }
}

/// Get features for a MUC room based on configuration.
#[must_use]
pub fn muc_room_features(
    persistence: RoomPersistence,
    membership: RoomMembership,
    visibility: RoomVisibility,
    moderation: crate::types::Moderation,
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
        // XEP-0513 §292 + §303: advertising `urn:xmpp:mentions:0` and
        // `urn:xmpp:mentions:0#channel` is binding — once advertised,
        // the room's `<query xmlns='urn:xmpp:mentions:0'/>` IQ MUST
        // return a form with `mentions#count` + `mentions#individual`
        // (always required) and `mentions#channel` (if and only if
        // `#channel` is advertised). Slices 3a/3b enforce a hardcoded
        // server policy (`mentions#count = 5`, `mentions#individual
        // = participants`, `mentions#channel = moderators`) at T0
        // candidate classification; slice 3c wires the §295 IQ
        // surface so the form on the wire matches the policy in
        // effect. Per CLAUDE.md XEP conformance hard rule, advertising
        // the namespaces only becomes safe once the IQ form is
        // implemented — see
        // `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/mentions_permissions.rs`
        // (the §295 IQ handler in the `waddle-server` crate) for the
        // handler that backs these advertised features.
        //
        // TODO(#525-followup): §303 also SHOULDs that the
        // `mentions#*` form fields be mirrored into the room's
        // `muc#roominfo` extension form (xeps/xep-0513.xml line 303).
        // Deferred — adding fields to muc#roominfo is a separate
        // slice; the dedicated §295 IQ form is the primary discovery
        // path and is conformant on its own.
        Feature::explicit_mentions(),
        Feature::channel_mentions(),
        Feature::muc_nonanonymous(),
        // Waddle rooms have no password support, so every room is
        // truthfully `muc_unsecured` (#1265 item 13; XEP-0045 §6.4).
        Feature::muc_unsecured(),
        // XEP-0045 §7.4: reflected message ids are preserved
        // (#1265 item 14).
        Feature::muc_stable_id(),
    ];

    if persistence == RoomPersistence::Persistent {
        features.push(Feature::muc_persistent());
    } else {
        features.push(Feature::muc_temporary());
    }

    if membership == RoomMembership::MembersOnly {
        features.push(Feature::muc_membersonly());
    } else {
        features.push(Feature::muc_open());
    }

    if visibility == RoomVisibility::Public {
        features.push(Feature::muc_public());
    } else {
        features.push(Feature::muc_hidden());
    }

    if moderation == crate::types::Moderation::Moderated {
        features.push(Feature::muc_moderated());
    } else {
        features.push(Feature::muc_unmoderated());
    }

    features
}
