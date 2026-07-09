//! Stable identity and deployment metadata for Gate 0 disco targets.
//!
//! Server discovery and the native capability collector share this model so a
//! target cannot acquire a different slug, JID template, or availability at
//! either side of the evidence boundary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoTarget {
    Server,
    MucService,
    UploadService,
    SpacesService,
    CommunityService,
    ExtensionsService,
    PushService,
    CallsMixer,
    RepresentativeMucRoom,
    AuthenticatedSelf,
}

impl DiscoTarget {
    pub const ALL: [Self; 10] = [
        Self::Server,
        Self::MucService,
        Self::UploadService,
        Self::SpacesService,
        Self::CommunityService,
        Self::ExtensionsService,
        Self::PushService,
        Self::CallsMixer,
        Self::RepresentativeMucRoom,
        Self::AuthenticatedSelf,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::MucService => "muc-service",
            Self::UploadService => "upload-service",
            Self::SpacesService => "spaces-service",
            Self::CommunityService => "community-service",
            Self::ExtensionsService => "extensions-service",
            Self::PushService => "push-service",
            Self::CallsMixer => "calls-mixer",
            Self::RepresentativeMucRoom => "representative-muc-room",
            Self::AuthenticatedSelf => "authenticated-self",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.slug() == slug)
    }

    pub const fn jid_template(self) -> &'static str {
        match self {
            Self::Server => "{xmpp_domain}",
            Self::MucService => "{muc_domain}",
            Self::UploadService => "upload.{xmpp_domain}",
            Self::SpacesService => "{spaces_domain}",
            Self::CommunityService => "community.{xmpp_domain}",
            Self::ExtensionsService => "extensions.{xmpp_domain}",
            Self::PushService => "push.{xmpp_domain}",
            Self::CallsMixer => "calls.{xmpp_domain}",
            Self::RepresentativeMucRoom => "{representative_muc_room}",
            Self::AuthenticatedSelf => "{authenticated_bare_jid}",
        }
    }

    pub const fn availability(self) -> DiscoTargetAvailability {
        match self {
            Self::CallsMixer => DiscoTargetAvailability::Configured,
            Self::RepresentativeMucRoom | Self::AuthenticatedSelf => {
                DiscoTargetAvailability::DynamicEntity
            }
            Self::Server
            | Self::MucService
            | Self::UploadService
            | Self::SpacesService
            | Self::CommunityService
            | Self::ExtensionsService
            | Self::PushService => DiscoTargetAvailability::Always,
        }
    }
}

impl std::fmt::Display for DiscoTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.slug())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoTargetAvailability {
    Always,
    Configured,
    DynamicEntity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_has_unique_stable_metadata() {
        let slugs = DiscoTarget::ALL.map(DiscoTarget::slug);
        let templates = DiscoTarget::ALL.map(DiscoTarget::jid_template);

        assert_eq!(
            slugs
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            slugs.len()
        );
        assert_eq!(
            templates
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            templates.len()
        );
        for target in DiscoTarget::ALL {
            assert_eq!(DiscoTarget::from_slug(target.slug()), Some(target));
            assert_eq!(target.to_string(), target.slug());
        }
    }
}
