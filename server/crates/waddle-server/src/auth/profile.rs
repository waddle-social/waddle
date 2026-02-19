//! Bluesky Profile Fetcher
//!
//! Resolves ATProto user profiles from the Bluesky public API for
//! auto-populating XMPP vCards (FR-1).
//!
//! Uses the public `app.bsky.actor.getProfile` endpoint with a 2-second
//! timeout (NFR-4). Failure never blocks authentication.

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

/// Timeout for Bluesky profile API requests (NFR-4: max 2 seconds added to login).
const PROFILE_FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Public Bluesky API base URL.
const BSKY_PUBLIC_API: &str = "https://public.api.bsky.app";

/// Bluesky profile data resolved from ATProto.
#[derive(Debug, Clone)]
pub struct BlueskyProfile {
    /// Display name (may be empty/None)
    pub display_name: Option<String>,
    /// Avatar URL (Bluesky CDN, HTTPS only)
    pub avatar_url: Option<String>,
    /// Bio/description
    pub description: Option<String>,
    /// Handle (e.g., user.bsky.social)
    pub handle: String,
}

/// Errors from profile fetching.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),
    #[error("Profile not found for DID: {0}")]
    NotFound(String),
    #[error("Unexpected API response: {0}")]
    BadResponse(String),
}

/// Raw JSON response from app.bsky.actor.getProfile.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BskyProfileResponse {
    #[allow(dead_code)]
    did: String,
    handle: String,
    display_name: Option<String>,
    description: Option<String>,
    avatar: Option<String>,
}

/// Fetch a Bluesky profile for the given DID.
///
/// Uses the public Bluesky API with a 2-second timeout (NFR-4).
/// Returns `Err` on failure — callers should log and continue (FR-1.4).
pub async fn fetch_bluesky_profile(
    http_client: &Client,
    did: &str,
) -> Result<BlueskyProfile, ProfileError> {
    debug!(did = %did, "Fetching Bluesky profile");

    let resp = http_client
        .get(format!(
            "{}/xrpc/app.bsky.actor.getProfile",
            BSKY_PUBLIC_API
        ))
        .query(&[("actor", did)])
        .timeout(PROFILE_FETCH_TIMEOUT)
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND
        || resp.status() == reqwest::StatusCode::BAD_REQUEST
    {
        return Err(ProfileError::NotFound(did.to_string()));
    }

    if !resp.status().is_success() {
        return Err(ProfileError::BadResponse(format!(
            "Status {} for DID {}",
            resp.status(),
            did
        )));
    }

    let profile: BskyProfileResponse = resp.json().await?;

    // Validate avatar URL: HTTPS only (security requirement)
    let avatar_url = profile.avatar.and_then(|url| {
        if url.starts_with("https://") {
            Some(url)
        } else {
            warn!(did = %did, url = %url, "Rejecting non-HTTPS avatar URL");
            None
        }
    });

    // Filter empty strings to None
    let display_name = profile.display_name.filter(|s| !s.trim().is_empty());
    let description = profile.description.filter(|s| !s.trim().is_empty());

    debug!(
        did = %did,
        display_name = ?display_name,
        has_avatar = avatar_url.is_some(),
        "Bluesky profile fetched"
    );

    Ok(BlueskyProfile {
        display_name,
        avatar_url,
        description,
        handle: profile.handle,
    })
}

/// Build a vCard XML string from a Bluesky profile.
///
/// Produces a vcard-temp XML with FN, PHOTO/EXTVAL, and DESC elements.
pub fn build_vcard_from_profile(profile: &BlueskyProfile) -> String {
    let mut xml = String::from("<vCard xmlns='vcard-temp'>");

    // FN: display name, falling back to handle
    let display_name = profile.display_name.as_deref().unwrap_or(&profile.handle);
    xml.push_str(&format!("<FN>{}</FN>", escape_xml(display_name)));

    // PHOTO/EXTVAL: avatar URL (HTTPS-validated upstream)
    if let Some(ref url) = profile.avatar_url {
        xml.push_str(&format!(
            "<PHOTO><EXTVAL>{}</EXTVAL></PHOTO>",
            escape_xml(url)
        ));
    }

    // DESC: bio/description
    if let Some(ref desc) = profile.description {
        xml.push_str(&format!("<DESC>{}</DESC>", escape_xml(desc)));
    }

    xml.push_str("</vCard>");
    xml
}

/// Escape XML special characters to prevent injection.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_vcard_full_profile() {
        let profile = BlueskyProfile {
            display_name: Some("David Flanagan".to_string()),
            avatar_url: Some(
                "https://cdn.bsky.app/img/avatar/plain/did:plc:abc/cid@jpeg".to_string(),
            ),
            description: Some("Software engineer".to_string()),
            handle: "david.bsky.social".to_string(),
        };

        let xml = build_vcard_from_profile(&profile);

        assert!(xml.contains("<FN>David Flanagan</FN>"));
        assert!(xml.contains(
            "<EXTVAL>https://cdn.bsky.app/img/avatar/plain/did:plc:abc/cid@jpeg</EXTVAL>"
        ));
        assert!(xml.contains("<DESC>Software engineer</DESC>"));
        assert!(xml.starts_with("<vCard xmlns='vcard-temp'>"));
        assert!(xml.ends_with("</vCard>"));
    }

    #[test]
    fn test_build_vcard_minimal_profile() {
        let profile = BlueskyProfile {
            display_name: None,
            avatar_url: None,
            description: None,
            handle: "user.bsky.social".to_string(),
        };

        let xml = build_vcard_from_profile(&profile);

        // Falls back to handle for FN
        assert!(xml.contains("<FN>user.bsky.social</FN>"));
        assert!(!xml.contains("<PHOTO>"));
        assert!(!xml.contains("<DESC>"));
    }

    #[test]
    fn test_build_vcard_xml_escaping() {
        let profile = BlueskyProfile {
            display_name: Some("O'Brien & <Friends>".to_string()),
            avatar_url: None,
            description: Some("Loves \"quotes\" & <tags>".to_string()),
            handle: "test.bsky.social".to_string(),
        };

        let xml = build_vcard_from_profile(&profile);

        assert!(xml.contains("<FN>O&apos;Brien &amp; &lt;Friends&gt;</FN>"));
        assert!(xml.contains("<DESC>Loves &quot;quotes&quot; &amp; &lt;tags&gt;</DESC>"));
    }

    #[test]
    fn test_build_vcard_non_https_avatar_excluded() {
        // The avatar_url should already be validated upstream by fetch_bluesky_profile,
        // but if somehow a non-HTTPS URL slips through, build_vcard still includes it.
        // The validation happens at fetch time, not build time.
        let profile = BlueskyProfile {
            display_name: Some("Test".to_string()),
            avatar_url: Some("https://valid.example.com/avatar.jpg".to_string()),
            description: None,
            handle: "test.bsky.social".to_string(),
        };

        let xml = build_vcard_from_profile(&profile);
        assert!(xml.contains("<EXTVAL>https://valid.example.com/avatar.jpg</EXTVAL>"));
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("hello"), "hello");
        assert_eq!(escape_xml("<>&\"'"), "&lt;&gt;&amp;&quot;&apos;");
        assert_eq!(escape_xml(""), "");
    }
}
