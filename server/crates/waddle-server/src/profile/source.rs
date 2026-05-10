//! Typed profile-source value for the OIDC → PEP bridge.

use thiserror::Error;
use url::Url;
use waddle_xmpp::XmppError;

use super::fetch::FetchError;
use crate::vcard::VCardError;

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
        /// in the removal flow.
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
    /// Failure from the PubSub storage layer — covers `publish_item`,
    /// `update_node_config`, and `get_items` calls for the avatar
    /// data, avatar metadata, and vCard4 nodes.
    #[error("pubsub storage error: {0}")]
    PubSub(#[from] XmppError),
    /// Failure from the vcard-temp store (`VCardStore::get` /
    /// `VCardStore::set`).
    #[error("vcard-temp store error: {0}")]
    VCardTemp(#[from] VCardError),
    /// Failure parsing a stored vCard4 item back into a typed
    /// element. The stored XML is either truncated or has been
    /// corrupted out-of-band; the OIDC publish refuses to overwrite
    /// it.
    #[error("vcard4 stored item is malformed: {0}")]
    VCard4Malformed(String),
}
