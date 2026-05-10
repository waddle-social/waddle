//! Typed profile-source value for the OIDC → PEP bridge.

use thiserror::Error;
use url::Url;
use waddle_xmpp::XmppError;

use super::fetch::FetchError;
use crate::vcard::VCardError;

/// Per-axis intent for a single PEP profile sync call. PHOTO and FN
/// are independent — either subset can be set, removed, or skipped
/// on any single call.
#[derive(Debug, Clone)]
pub enum PhotoIntent {
    /// No PHOTO sync for this call.
    Skip,
    /// Set PHOTO from the bytes fetched at this URL.
    SetFromUrl(Url),
    /// Remove the OIDC-managed PHOTO if one exists. Honored only
    /// when `users.avatar_source = 'oidc'`; a user who self-published
    /// via wire XEP-0084 keeps their picture (the user-managed
    /// avatar guard).
    RemoveIfOidcOwned,
}

#[derive(Debug, Clone)]
pub enum NameIntent {
    /// No FN sync for this call.
    Skip,
    /// Set FN to this string.
    Set(String),
    /// Remove `<FN>` / `<fn>` from vcard-temp + vCard4. Name has no
    /// user-managed guard analogous to PHOTO — name is always
    /// OIDC-owned today.
    Remove,
}

/// Provenance + payload for a single profile-publish call.
///
/// The variant is open for future provenances (e.g. SCIM, admin
/// override) without changing the call sites that consume the helper.
#[derive(Debug, Clone)]
pub enum ProfileSource {
    Oidc {
        photo: PhotoIntent,
        name: NameIntent,
    },
}

impl ProfileSource {
    pub fn is_no_op(&self) -> bool {
        match self {
            ProfileSource::Oidc { photo, name } => {
                matches!(photo, PhotoIntent::Skip) && matches!(name, NameIntent::Skip)
            }
        }
    }

    /// Build a `ProfileSource::Oidc` from raw OIDC claim values.
    /// `avatar_url = Some` → set; `None` → remove-if-oidc-owned.
    /// `display_name = Some` → set; `None` → remove.
    ///
    /// Callers are responsible for filtering empty/whitespace claim
    /// strings to `None` before calling — the helper interprets a
    /// `Some("")` as a literal "set FN to empty string", which is
    /// almost certainly not what an IDP returning an empty `name`
    /// claim meant.
    pub fn from_oidc_claims(avatar_url: Option<Url>, display_name: Option<String>) -> Self {
        let photo = match avatar_url {
            Some(url) => PhotoIntent::SetFromUrl(url),
            None => PhotoIntent::RemoveIfOidcOwned,
        };
        let name = match display_name {
            Some(s) => NameIntent::Set(s),
            None => NameIntent::Remove,
        };
        ProfileSource::Oidc { photo, name }
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
    /// Failure looking up the `users.avatar_source` provenance flag
    /// — the user-managed avatar guard cannot be evaluated and the
    /// publish chain refuses to honor `RemoveIfOidcOwned`.
    #[error("avatar_source lookup failed: {0}")]
    AvatarSourceLookup(String),
}
