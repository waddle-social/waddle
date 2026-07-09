use serde::Serialize;
use waddle_xmpp::disco::Identity;

use super::DiscoTarget;

pub fn target_identities(target: DiscoTarget) -> Vec<Identity> {
    target_identities_with_name(target, None)
}

/// Build runtime identities while allowing a dynamic room display name.
pub fn target_identities_with_name(
    target: DiscoTarget,
    dynamic_name: Option<&str>,
) -> Vec<Identity> {
    target_identity_contracts(target)
        .iter()
        .map(|identity| {
            Identity::new(
                identity.category.as_str(),
                identity.type_.as_str(),
                dynamic_name.or(identity.name.map(DiscoIdentityName::as_str)),
            )
        })
        .collect()
}

/// Stable XEP-0030 identity fields. Names are omitted for entities whose
/// display name comes from live room/account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiscoIdentity {
    pub category: DiscoIdentityCategory,
    #[serde(rename = "type")]
    pub type_: DiscoIdentityType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<DiscoIdentityName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoIdentityCategory {
    Account,
    Conference,
    Pubsub,
    Server,
    Store,
}

impl DiscoIdentityCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Conference => "conference",
            Self::Pubsub => "pubsub",
            Self::Server => "server",
            Self::Store => "store",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoIdentityType {
    AudioVideo,
    File,
    Im,
    Pep,
    Push,
    Registered,
    Service,
    Text,
}

impl DiscoIdentityType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AudioVideo => "audio-video",
            Self::File => "file",
            Self::Im => "im",
            Self::Pep => "pep",
            Self::Push => "push",
            Self::Registered => "registered",
            Self::Service => "service",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DiscoIdentityName {
    #[serde(rename = "Community")]
    Community,
    #[serde(rename = "HTTP File Upload")]
    HttpFileUpload,
    #[serde(rename = "Push Service")]
    PushService,
    #[serde(rename = "Spaces")]
    Spaces,
    #[serde(rename = "Waddle")]
    Waddle,
    #[serde(rename = "Waddle Chatrooms")]
    WaddleChatrooms,
    #[serde(rename = "Waddle Extensions")]
    WaddleExtensions,
    #[serde(rename = "Waddle Group Call Mixer")]
    WaddleGroupCallMixer,
}

impl DiscoIdentityName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Community => "Community",
            Self::HttpFileUpload => "HTTP File Upload",
            Self::PushService => "Push Service",
            Self::Spaces => "Spaces",
            Self::Waddle => "Waddle",
            Self::WaddleChatrooms => "Waddle Chatrooms",
            Self::WaddleExtensions => "Waddle Extensions",
            Self::WaddleGroupCallMixer => "Waddle Group Call Mixer",
        }
    }
}

pub const fn target_identity_contracts(target: DiscoTarget) -> &'static [DiscoIdentity] {
    match target {
        DiscoTarget::Server => &SERVER_IDENTITIES,
        DiscoTarget::MucService => &MUC_SERVICE_IDENTITIES,
        DiscoTarget::UploadService => &UPLOAD_IDENTITIES,
        DiscoTarget::SpacesService => &SPACES_IDENTITIES,
        DiscoTarget::CommunityService => &COMMUNITY_IDENTITIES,
        DiscoTarget::ExtensionsService => &EXTENSIONS_IDENTITIES,
        DiscoTarget::PushService => &PUSH_IDENTITIES,
        DiscoTarget::CallsMixer => &CALLS_IDENTITIES,
        DiscoTarget::RepresentativeMucRoom => &MUC_ROOM_IDENTITIES,
        DiscoTarget::AuthenticatedSelf => &AUTHENTICATED_SELF_IDENTITIES,
    }
}

const SERVER_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Server,
    type_: DiscoIdentityType::Im,
    name: Some(DiscoIdentityName::Waddle),
}];
const MUC_SERVICE_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Conference,
    type_: DiscoIdentityType::Text,
    name: Some(DiscoIdentityName::WaddleChatrooms),
}];
const MUC_ROOM_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Conference,
    type_: DiscoIdentityType::Text,
    name: None,
}];
const UPLOAD_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Store,
    type_: DiscoIdentityType::File,
    name: Some(DiscoIdentityName::HttpFileUpload),
}];
const SPACES_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Pubsub,
    type_: DiscoIdentityType::Service,
    name: Some(DiscoIdentityName::Spaces),
}];
const COMMUNITY_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Pubsub,
    type_: DiscoIdentityType::Service,
    name: Some(DiscoIdentityName::Community),
}];
const EXTENSIONS_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Pubsub,
    type_: DiscoIdentityType::Service,
    name: Some(DiscoIdentityName::WaddleExtensions),
}];
const PUSH_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Pubsub,
    type_: DiscoIdentityType::Push,
    name: Some(DiscoIdentityName::PushService),
}];
const CALLS_IDENTITIES: [DiscoIdentity; 1] = [DiscoIdentity {
    category: DiscoIdentityCategory::Conference,
    type_: DiscoIdentityType::AudioVideo,
    name: Some(DiscoIdentityName::WaddleGroupCallMixer),
}];
const AUTHENTICATED_SELF_IDENTITIES: [DiscoIdentity; 2] = [
    DiscoIdentity {
        category: DiscoIdentityCategory::Account,
        type_: DiscoIdentityType::Registered,
        name: None,
    },
    DiscoIdentity {
        category: DiscoIdentityCategory::Pubsub,
        type_: DiscoIdentityType::Pep,
        name: None,
    },
];
