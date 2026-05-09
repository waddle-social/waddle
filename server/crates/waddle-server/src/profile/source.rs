//! Typed profile-source value for the OIDC → PEP bridge.

use thiserror::Error;
use url::Url;

use super::fetch::FetchError;

/// Provenance + payload for a single profile-publish call.
///
/// The variant is open for future provenances (e.g. SCIM, admin
/// override) without changing the call sites that consume the helper.
/// `Oidc { None, None }` is the explicit no-op shape.
#[derive(Debug, Clone)]
pub enum ProfileSource {
    Oidc {
        /// Typed avatar URL parsed once at the OIDC boundary. Per the
        /// typed-payloads hard rule the helper does NOT accept &str
        /// here.
        avatar_url: Option<Url>,
        /// Free-form unicode display name. `None` means "no FN sync
        /// for this call". The bridge does NOT clear an existing FN
        /// just because it isn't supplied — explicit removal lives
        /// in the removal flow (PR 4).
        display_name: Option<String>,
    },
}

impl ProfileSource {
    pub fn is_no_op(&self) -> bool {
        match self {
            ProfileSource::Oidc {
                avatar_url,
                display_name,
            } => avatar_url.is_none() && display_name.is_none(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProfileSyncError {
    #[error("avatar fetch failed: {0}")]
    Fetch(#[from] FetchError),
    #[error("pubsub publish failed: {0}")]
    PubSubPublish(String),
    #[error("vcard-temp read/write failed: {0}")]
    VCardTemp(String),
    #[error("vcard4 PEP item read/write failed: {0}")]
    VCard4(String),
    #[error("self-presence broadcast failed: {0}")]
    PresenceBroadcast(String),
}
