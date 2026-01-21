//! DID to JID Mapping for XMPP Authentication
//!
//! This module handles conversion between ATProto DIDs and XMPP JIDs.
//!
//! # Identity Mapping
//!
//! ATProto DIDs are mapped to XMPP JIDs as follows:
//!
//! - `did:plc:identifier` → `identifier@domain`
//! - `did:web:example.com` → `web-example-com@domain`
//!
//! The JID localpart is derived from the DID identifier, with special handling
//! for `did:web` to ensure valid JID characters.
//!
//! # Examples
//!
//! ```rust
//! use waddle_server::auth::jid::{did_to_jid, jid_to_did};
//!
//! // did:plc conversion
//! let jid = did_to_jid("did:plc:abc123xyz789def", "waddle.social").unwrap();
//! assert_eq!(jid, "abc123xyz789def@waddle.social");
//!
//! // did:web conversion
//! let jid = did_to_jid("did:web:example.com", "waddle.social").unwrap();
//! assert_eq!(jid, "web-example-com@waddle.social");
//! ```

use super::AuthError;

/// Convert an ATProto DID to an XMPP JID
///
/// # Arguments
///
/// * `did` - The ATProto DID (did:plc:xxx or did:web:xxx)
/// * `domain` - The XMPP server domain (e.g., "waddle.social")
///
/// # Returns
///
/// The full JID string (localpart@domain)
///
/// # Errors
///
/// Returns `AuthError::InvalidDid` if the DID format is not recognized
pub fn did_to_jid(did: &str, domain: &str) -> Result<String, AuthError> {
    let localpart = did_to_jid_localpart(did)?;
    Ok(format!("{}@{}", localpart, domain))
}

/// Convert an ATProto DID to a JID localpart (without domain)
///
/// # Arguments
///
/// * `did` - The ATProto DID (did:plc:xxx or did:web:xxx)
///
/// # Returns
///
/// The JID localpart string
///
/// # Errors
///
/// Returns `AuthError::InvalidDid` if the DID format is not recognized
pub fn did_to_jid_localpart(did: &str) -> Result<String, AuthError> {
    if let Some(identifier) = did.strip_prefix("did:plc:") {
        // did:plc:identifier → identifier
        if identifier.is_empty() {
            return Err(AuthError::InvalidDid("Empty did:plc identifier".to_string()));
        }
        // Validate identifier (should be alphanumeric)
        if !identifier.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(AuthError::InvalidDid(format!(
                "Invalid did:plc identifier: {}",
                identifier
            )));
        }
        Ok(identifier.to_string())
    } else if let Some(domain_part) = did.strip_prefix("did:web:") {
        // did:web:example.com → web-example-com
        // did:web:sub.example.com → web-sub-example-com
        if domain_part.is_empty() {
            return Err(AuthError::InvalidDid("Empty did:web domain".to_string()));
        }
        // Replace dots and colons with hyphens for JID safety
        // Colons are used for path segments in did:web (e.g., did:web:example.com:path)
        let sanitized = domain_part
            .replace('.', "-")
            .replace(':', "-");
        Ok(format!("web-{}", sanitized))
    } else {
        Err(AuthError::InvalidDid(format!(
            "Unsupported DID method: {}",
            did
        )))
    }
}

/// Convert an XMPP JID back to an ATProto DID
///
/// # Arguments
///
/// * `jid` - The full JID string (localpart@domain) or bare localpart
///
/// # Returns
///
/// The ATProto DID string
///
/// # Errors
///
/// Returns `AuthError::InvalidDid` if the JID format cannot be converted back to a DID
pub fn jid_to_did(jid: &str) -> Result<String, AuthError> {
    // Extract localpart from JID (handle both bare@domain and just localpart)
    let localpart = jid
        .split('@')
        .next()
        .ok_or_else(|| AuthError::InvalidDid("Empty JID".to_string()))?;

    jid_localpart_to_did(localpart)
}

/// Convert a JID localpart back to an ATProto DID
///
/// # Arguments
///
/// * `localpart` - The JID localpart string
///
/// # Returns
///
/// The ATProto DID string
///
/// # Errors
///
/// Returns `AuthError::InvalidDid` if the localpart format cannot be converted back to a DID
pub fn jid_localpart_to_did(localpart: &str) -> Result<String, AuthError> {
    if localpart.is_empty() {
        return Err(AuthError::InvalidDid("Empty JID localpart".to_string()));
    }

    if let Some(web_part) = localpart.strip_prefix("web-") {
        // web-example-com → did:web:example.com
        // This is a heuristic reconstruction - we assume dots between segments
        // Note: This cannot perfectly reconstruct did:web with path segments
        // since did:web:example.com:path and did:web:example-com:path would
        // both become web-example-com-path
        let domain = web_part.replace('-', ".");
        Ok(format!("did:web:{}", domain))
    } else {
        // Assume did:plc for non-web prefixed localparts
        if !localpart.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(AuthError::InvalidDid(format!(
                "Invalid JID localpart for did:plc: {}",
                localpart
            )));
        }
        Ok(format!("did:plc:{}", localpart))
    }
}

/// Extract the localpart from a full JID
///
/// # Arguments
///
/// * `jid` - The full JID string (localpart@domain/resource)
///
/// # Returns
///
/// The localpart portion of the JID
pub fn extract_jid_localpart(jid: &str) -> Option<&str> {
    // Strip resource first (localpart@domain/resource → localpart@domain)
    let without_resource = jid.split('/').next()?;
    // Then get localpart (localpart@domain → localpart)
    without_resource.split('@').next()
}

/// Extract the domain from a full JID
///
/// # Arguments
///
/// * `jid` - The full JID string (localpart@domain/resource)
///
/// # Returns
///
/// The domain portion of the JID
pub fn extract_jid_domain(jid: &str) -> Option<&str> {
    // Strip resource first (localpart@domain/resource → localpart@domain)
    let without_resource = jid.split('/').next()?;
    // Then get domain (localpart@domain → domain)
    let parts: Vec<&str> = without_resource.split('@').collect();
    if parts.len() >= 2 {
        Some(parts[1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_did_plc_to_jid() {
        let jid = did_to_jid("did:plc:ewvi7nxzy7mbhber23pb6il4", "waddle.social").unwrap();
        assert_eq!(jid, "ewvi7nxzy7mbhber23pb6il4@waddle.social");
    }

    #[test]
    fn test_did_plc_to_jid_localpart() {
        let localpart = did_to_jid_localpart("did:plc:abc123xyz789def").unwrap();
        assert_eq!(localpart, "abc123xyz789def");
    }

    #[test]
    fn test_did_web_to_jid() {
        let jid = did_to_jid("did:web:example.com", "waddle.social").unwrap();
        assert_eq!(jid, "web-example-com@waddle.social");
    }

    #[test]
    fn test_did_web_subdomain_to_jid() {
        let jid = did_to_jid("did:web:blog.example.com", "waddle.social").unwrap();
        assert_eq!(jid, "web-blog-example-com@waddle.social");
    }

    #[test]
    fn test_did_web_with_path_to_jid() {
        // did:web with path segments (colons become hyphens)
        let jid = did_to_jid("did:web:example.com:users:alice", "waddle.social").unwrap();
        assert_eq!(jid, "web-example-com-users-alice@waddle.social");
    }

    #[test]
    fn test_jid_to_did_plc() {
        let did = jid_to_did("abc123xyz789def@waddle.social").unwrap();
        assert_eq!(did, "did:plc:abc123xyz789def");
    }

    #[test]
    fn test_jid_localpart_to_did_plc() {
        let did = jid_localpart_to_did("ewvi7nxzy7mbhber23pb6il4").unwrap();
        assert_eq!(did, "did:plc:ewvi7nxzy7mbhber23pb6il4");
    }

    #[test]
    fn test_jid_to_did_web() {
        let did = jid_to_did("web-example-com@waddle.social").unwrap();
        assert_eq!(did, "did:web:example.com");
    }

    #[test]
    fn test_jid_localpart_to_did_web() {
        let did = jid_localpart_to_did("web-blog-example-com").unwrap();
        assert_eq!(did, "did:web:blog.example.com");
    }

    #[test]
    fn test_roundtrip_did_plc() {
        let original = "did:plc:ewvi7nxzy7mbhber23pb6il4";
        let jid = did_to_jid(original, "waddle.social").unwrap();
        let recovered = jid_to_did(&jid).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_roundtrip_did_web_simple() {
        let original = "did:web:example.com";
        let jid = did_to_jid(original, "waddle.social").unwrap();
        let recovered = jid_to_did(&jid).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_invalid_did_empty_plc() {
        let result = did_to_jid("did:plc:", "waddle.social");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_did_empty_web() {
        let result = did_to_jid("did:web:", "waddle.social");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_did_unsupported_method() {
        let result = did_to_jid("did:key:z6Mkfriq1MqLBoPWecGoDLjguo1sB9brj6wT3qZ5BxkKpuP6", "waddle.social");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_jid_empty() {
        let result = jid_to_did("");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_jid_localpart() {
        assert_eq!(extract_jid_localpart("alice@example.com"), Some("alice"));
        assert_eq!(extract_jid_localpart("alice@example.com/mobile"), Some("alice"));
        assert_eq!(extract_jid_localpart("alice"), Some("alice"));
    }

    #[test]
    fn test_extract_jid_domain() {
        assert_eq!(extract_jid_domain("alice@example.com"), Some("example.com"));
        assert_eq!(extract_jid_domain("alice@example.com/mobile"), Some("example.com"));
        assert_eq!(extract_jid_domain("alice"), None);
    }
}
