use std::collections::BTreeSet;

use waddle_xmpp::disco::{
    community_service_features, muc_room_features, muc_service_features, push_service_features,
    spaces_service_features, upload_service_features, Feature,
};

use super::DiscoTarget;

/// Exact extension modules published by `server/deployment.cue`. These are
/// optional at runtime but must be present in the target-local claimable union
/// before their namespaces may enter privacy-minimized evidence.
pub(super) const CURATED_EXTENSION_NAMESPACES: [(&str, &str); 5] = [
    ("link-board", "urn:waddle:link-board:1"),
    ("ai-chatbot", "urn:waddle:ai-chatbot:1"),
    ("decision-polls", "urn:waddle:decision-polls:1"),
    ("github", "urn:waddle:web-integration:1"),
    ("stargate-quotes", "urn:waddle:stargate-quotes:1"),
];

/// Optional runtime additions on the server and component roots.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeFeatureOptions<'a> {
    pub calls_available: bool,
    pub isr_available: bool,
    pub extension_features: &'a [Feature],
}

/// The calls surface exists only when the same process owns an SFU service
/// and both IQ namespaces used by the implementation are registered. Checking
/// all three conditions prevents a half-wired dispatcher (or a test fixture)
/// from advertising a mixer that cannot complete a call.
pub(crate) fn calls_available(state: &crate::server::routes::websocket::WebSocketState) -> bool {
    calls_available_from_parts(
        state.deps.protocol.sfu.is_some(),
        state
            .deps
            .protocol
            .dispatcher
            .has_iq_handler("urn:xmpp:jingle:1"),
        state
            .deps
            .protocol
            .dispatcher
            .has_iq_handler("urn:xmpp:extdisco:2"),
    )
}

pub(super) const fn calls_available_from_parts(
    has_sfu: bool,
    has_jingle: bool,
    has_extdisco: bool,
) -> bool {
    has_sfu && has_jingle && has_extdisco
}

#[derive(Debug, Clone, Copy)]
pub struct MucRoomFeatureOptions {
    pub persistent: bool,
    pub members_only: bool,
    pub public_room: bool,
    pub moderated: bool,
    pub forum: bool,
    pub group_dm: bool,
    pub has_space_metadata: bool,
}

impl Default for MucRoomFeatureOptions {
    fn default() -> Self {
        Self {
            persistent: false,
            members_only: false,
            public_room: true,
            moderated: false,
            forum: false,
            group_dm: false,
            has_space_metadata: false,
        }
    }
}

pub fn server_target_features(options: RuntimeFeatureOptions<'_>) -> Vec<Feature> {
    let mut features = waddle_xmpp::disco::info::server_features();
    features.push(Feature::new("jabber:iq:search"));
    if options.isr_available {
        features.push(Feature::new(waddle_xmpp::isr::ISR_NS));
    }
    features.extend(options.extension_features.iter().cloned());
    if options.calls_available {
        features.extend(waddle_xmpp::disco::info::call_features());
    }
    unique_features(features)
}

pub fn muc_service_target_features(extension_features: &[Feature]) -> Vec<Feature> {
    let mut features = muc_service_features();
    features.push(Feature::replies());
    features.push(Feature::new(waddle_xmpp::xep::xep0433::NS_CHANNEL_SEARCH));
    features.extend(extension_features.iter().cloned());
    unique_features(features)
}

pub fn muc_room_target_features(
    options: MucRoomFeatureOptions,
    extension_features: &[Feature],
) -> Vec<Feature> {
    let mut features = muc_room_features(
        options.persistent,
        options.members_only,
        options.public_room,
        options.moderated,
        options.forum,
    );
    if options.group_dm {
        features.push(Feature::new(waddle_xmpp::admin::NS_GROUP_DM_FEATURE));
    }
    features.extend(extension_features.iter().cloned());
    if options.has_space_metadata {
        features.push(Feature::spaces());
    }
    unique_features(features)
}

pub fn extensions_service_target_features(extension_features: &[Feature]) -> Vec<Feature> {
    let mut features = vec![
        Feature::disco_info(),
        Feature::disco_items(),
        Feature::commands(),
        Feature::pubsub(),
        Feature::pubsub_retrieve_items(),
        Feature::new("urn:waddle:extension:1"),
    ];
    features.extend(extension_features.iter().cloned());
    unique_features(features)
}

pub fn calls_mixer_target_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::muji(),
        Feature::jingle(),
        Feature::jingle_rtp(),
        Feature::jingle_rtp_audio(),
        Feature::jingle_rtp_video(),
        Feature::waddle_livekit_transport(),
    ]
}

pub fn authenticated_self_target_features() -> Vec<Feature> {
    let mut features = vec![
        Feature::disco_info(),
        Feature::mam(),
        Feature::mam_extended(),
        Feature::fulltext_mam(),
        Feature::threads_query(),
    ];
    features.extend(waddle_xmpp::pubsub::pep_features());
    unique_features(features)
}

fn unique_features(features: Vec<Feature>) -> Vec<Feature> {
    let mut seen = BTreeSet::new();
    features
        .into_iter()
        .filter(|feature| seen.insert(feature.0.clone()))
        .collect()
}

/// Features that every successful observation of a target must contain.
pub fn required_target_features(target: DiscoTarget) -> Vec<Feature> {
    let features = match target {
        DiscoTarget::Server | DiscoTarget::RepresentativeMucRoom => {
            feature_intersection(&runtime_target_feature_variants(target))
        }
        DiscoTarget::MucService => muc_service_target_features(&[]),
        DiscoTarget::UploadService => upload_service_features(),
        DiscoTarget::SpacesService => spaces_service_features(),
        DiscoTarget::CommunityService => community_service_features(),
        DiscoTarget::ExtensionsService => extensions_service_target_features(&[]),
        DiscoTarget::PushService => push_service_features(),
        DiscoTarget::CallsMixer => calls_mixer_target_features(),
        DiscoTarget::AuthenticatedSelf => authenticated_self_target_features(),
    };
    unique_features(features)
}

/// Full target-local feature union that a live deployment may legitimately
/// advertise. Optional runtime modes are represented here without weakening
/// the required baseline.
pub fn claimable_target_features(target: DiscoTarget) -> Vec<Feature> {
    let variants = runtime_target_feature_variants(target);
    let mut features = if variants.is_empty() {
        required_target_features(target)
    } else {
        feature_union(&variants)
    };
    features.extend(independently_optional_target_features(target));
    unique_features(features)
}

/// Deterministic, target-local unions used by capability declarations.
pub fn manifest_target_features(target: DiscoTarget) -> Vec<Feature> {
    claimable_target_features(target)
}

/// Exact non-extension vectors a runtime-dependent target may return.
pub fn runtime_target_feature_variants(target: DiscoTarget) -> Vec<Vec<Feature>> {
    match target {
        DiscoTarget::Server => unique_feature_variants(
            [false, true]
                .into_iter()
                .flat_map(|calls_available| {
                    [false, true].into_iter().map(move |isr_available| {
                        server_target_features(RuntimeFeatureOptions {
                            calls_available,
                            isr_available,
                            extension_features: &[],
                        })
                    })
                })
                .collect(),
        ),
        DiscoTarget::RepresentativeMucRoom => muc_room_runtime_variants(),
        _ => Vec::new(),
    }
}

fn muc_room_runtime_variants() -> Vec<Vec<Feature>> {
    let mut variants = Vec::new();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for public_room in [false, true] {
                for moderated in [false, true] {
                    for forum in [false, true] {
                        for group_dm in [false, true] {
                            for has_space_metadata in [false, true] {
                                variants.push(muc_room_target_features(
                                    MucRoomFeatureOptions {
                                        persistent,
                                        members_only,
                                        public_room,
                                        moderated,
                                        forum,
                                        group_dm,
                                        has_space_metadata,
                                    },
                                    &[],
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    unique_feature_variants(variants)
}

fn feature_intersection(variants: &[Vec<Feature>]) -> Vec<Feature> {
    let Some(first) = variants.first() else {
        return Vec::new();
    };
    let common = variants.iter().skip(1).fold(
        first
            .iter()
            .map(|feature| feature.0.as_str())
            .collect::<BTreeSet<_>>(),
        |common, variant| {
            let variant = variant
                .iter()
                .map(|feature| feature.0.as_str())
                .collect::<BTreeSet<_>>();
            common.intersection(&variant).copied().collect()
        },
    );
    first
        .iter()
        .filter(|feature| common.contains(feature.0.as_str()))
        .cloned()
        .collect()
}

fn feature_union(variants: &[Vec<Feature>]) -> Vec<Feature> {
    unique_features(variants.iter().flatten().cloned().collect())
}

fn unique_feature_variants(variants: Vec<Vec<Feature>>) -> Vec<Vec<Feature>> {
    let mut seen = BTreeSet::new();
    variants
        .into_iter()
        .filter(|variant| {
            seen.insert(
                variant
                    .iter()
                    .map(|feature| feature.0.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<String>>(),
            )
        })
        .collect()
}

pub(super) fn independently_optional_target_features(target: DiscoTarget) -> Vec<Feature> {
    match target {
        DiscoTarget::Server
        | DiscoTarget::MucService
        | DiscoTarget::ExtensionsService
        | DiscoTarget::RepresentativeMucRoom => curated_extension_features(),
        _ => Vec::new(),
    }
}

pub(super) fn curated_extension_features() -> Vec<Feature> {
    CURATED_EXTENSION_NAMESPACES
        .iter()
        .map(|(_, namespace)| Feature::new(namespace))
        .collect()
}
