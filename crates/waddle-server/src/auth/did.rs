//! DID Resolution for ATProto
//!
//! This module handles:
//! - Handle to DID resolution (DNS TXT and .well-known fallback)
//! - DID document retrieval (did:plc from plc.directory, did:web from HTTPS)
//!
//! # ATProto Handle Resolution
//!
//! ATProto handles can be resolved to DIDs in two ways:
//! 1. DNS TXT record at `_atproto.{handle}` containing `did={did}`
//! 2. HTTPS request to `https://{handle}/.well-known/atproto-did` returning the DID
//!
//! # DID Document Retrieval
//!
//! - `did:plc:*` -> `https://plc.directory/{did}`
//! - `did:web:*` -> `https://{domain}/.well-known/did.json`

use super::AuthError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, instrument, warn};

/// Default PLC directory URL
pub const PLC_DIRECTORY_URL: &str = "https://plc.directory";

/// DID Resolver for ATProto handles and DIDs
#[derive(Clone)]
pub struct DidResolver {
    http_client: Client,
    plc_directory_url: String,
}

impl Default for DidResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DidResolver {
    /// Create a new DID resolver with default settings
    pub fn new() -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Waddle/1.0")
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http_client,
            plc_directory_url: PLC_DIRECTORY_URL.to_string(),
        }
    }

    /// Create a DID resolver with a custom PLC directory URL (for testing)
    #[allow(dead_code)]
    pub fn with_plc_directory(plc_directory_url: &str) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Waddle/1.0")
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http_client,
            plc_directory_url: plc_directory_url.to_string(),
        }
    }

    /// Resolve an ATProto handle to a DID
    ///
    /// Tries DNS TXT record first, then falls back to .well-known
    #[instrument(skip(self), fields(handle = %handle))]
    pub async fn resolve_handle(&self, handle: &str) -> Result<String, AuthError> {
        // Validate handle format
        if !is_valid_handle(handle) {
            return Err(AuthError::InvalidHandle(handle.to_string()));
        }

        // Try DNS TXT resolution first
        debug!("Attempting DNS TXT resolution for handle: {}", handle);
        match self.resolve_handle_via_dns(handle).await {
            Ok(did) => {
                debug!("Resolved handle via DNS TXT: {} -> {}", handle, did);
                return Ok(did);
            }
            Err(e) => {
                debug!("DNS TXT resolution failed, trying .well-known: {}", e);
            }
        }

        // Fall back to .well-known
        debug!(
            "Attempting .well-known resolution for handle: {}",
            handle
        );
        match self.resolve_handle_via_well_known(handle).await {
            Ok(did) => {
                debug!(
                    "Resolved handle via .well-known: {} -> {}",
                    handle, did
                );
                Ok(did)
            }
            Err(e) => {
                warn!(
                    "Failed to resolve handle {} via both DNS and .well-known",
                    handle
                );
                Err(e)
            }
        }
    }

    /// Resolve handle via DNS TXT record
    ///
    /// Looks for a TXT record at `_atproto.{handle}` with format `did={did}`
    async fn resolve_handle_via_dns(&self, handle: &str) -> Result<String, AuthError> {
        use hickory_resolver::Resolver;

        let dns_name = format!("_atproto.{}", handle);

        let resolver = Resolver::builder_tokio()
            .map_err(|e| AuthError::DnsError(format!("Failed to create DNS resolver: {}", e)))?
            .build();

        let response = resolver
            .txt_lookup(&dns_name)
            .await
            .map_err(|e| AuthError::DnsError(format!("DNS lookup failed: {}", e)))?;

        for txt in response.iter() {
            let txt_data = txt.to_string();
            // TXT record format: did=did:plc:xxx or did=did:web:xxx
            if let Some(did) = txt_data.strip_prefix("did=") {
                let did = did.trim().to_string();
                if is_valid_did(&did) {
                    return Ok(did);
                }
            }
        }

        Err(AuthError::DnsError(
            "No valid DID found in DNS TXT records".to_string(),
        ))
    }

    /// Resolve handle via .well-known/atproto-did
    async fn resolve_handle_via_well_known(&self, handle: &str) -> Result<String, AuthError> {
        let url = format!("https://{}/.well-known/atproto-did", handle);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| AuthError::HttpError(format!("Failed to fetch .well-known: {}", e)))?;

        if !response.status().is_success() {
            return Err(AuthError::DidResolutionFailed(format!(
                ".well-known returned status {}",
                response.status()
            )));
        }

        let did = response
            .text()
            .await
            .map_err(|e| AuthError::HttpError(format!("Failed to read response: {}", e)))?
            .trim()
            .to_string();

        if !is_valid_did(&did) {
            return Err(AuthError::InvalidDid(format!(
                "Invalid DID returned from .well-known: {}",
                did
            )));
        }

        Ok(did)
    }

    /// Fetch a DID document from the appropriate directory
    ///
    /// - `did:plc:*` -> PLC directory
    /// - `did:web:*` -> HTTPS .well-known
    #[instrument(skip(self), fields(did = %did))]
    pub async fn resolve_did_document(&self, did: &str) -> Result<DidDocument, AuthError> {
        if !is_valid_did(did) {
            return Err(AuthError::InvalidDid(did.to_string()));
        }

        if did.starts_with("did:plc:") {
            self.fetch_plc_did_document(did).await
        } else if did.starts_with("did:web:") {
            self.fetch_web_did_document(did).await
        } else {
            Err(AuthError::InvalidDid(format!(
                "Unsupported DID method: {}",
                did
            )))
        }
    }

    /// Fetch DID document from PLC directory
    async fn fetch_plc_did_document(&self, did: &str) -> Result<DidDocument, AuthError> {
        let url = format!("{}/{}", self.plc_directory_url, did);
        debug!("Fetching PLC DID document from: {}", url);

        let response = self.http_client.get(&url).send().await.map_err(|e| {
            AuthError::HttpError(format!("Failed to fetch PLC document: {}", e))
        })?;

        if !response.status().is_success() {
            return Err(AuthError::DidDocumentFetchFailed(format!(
                "PLC directory returned status {}",
                response.status()
            )));
        }

        let doc: DidDocument = response.json().await.map_err(|e| {
            AuthError::DidDocumentFetchFailed(format!("Failed to parse DID document: {}", e))
        })?;

        Ok(doc)
    }

    /// Fetch DID document for did:web
    async fn fetch_web_did_document(&self, did: &str) -> Result<DidDocument, AuthError> {
        // did:web:example.com -> https://example.com/.well-known/did.json
        // did:web:example.com:path -> https://example.com/path/did.json
        let domain_path = did
            .strip_prefix("did:web:")
            .ok_or_else(|| AuthError::InvalidDid("Invalid did:web format".to_string()))?;

        let url = if domain_path.contains(':') {
            // Has path segments
            let parts: Vec<&str> = domain_path.split(':').collect();
            let domain = parts[0];
            let path = parts[1..].join("/");
            format!("https://{}/{}/did.json", domain, path)
        } else {
            // Just domain
            format!("https://{}/.well-known/did.json", domain_path)
        };

        debug!("Fetching web DID document from: {}", url);

        let response = self.http_client.get(&url).send().await.map_err(|e| {
            AuthError::HttpError(format!("Failed to fetch web DID document: {}", e))
        })?;

        if !response.status().is_success() {
            return Err(AuthError::DidDocumentFetchFailed(format!(
                "Web DID endpoint returned status {}",
                response.status()
            )));
        }

        let doc: DidDocument = response.json().await.map_err(|e| {
            AuthError::DidDocumentFetchFailed(format!("Failed to parse DID document: {}", e))
        })?;

        Ok(doc)
    }
}

/// DID Document structure (simplified for ATProto)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DidDocument {
    /// The DID this document is for
    pub id: String,

    /// Also known as (handles)
    #[serde(default, rename = "alsoKnownAs")]
    pub also_known_as: Vec<String>,

    /// Verification methods
    #[serde(default, rename = "verificationMethod")]
    pub verification_method: Vec<VerificationMethod>,

    /// Services (PDS endpoint, etc.)
    #[serde(default)]
    pub service: Vec<Service>,
}

impl DidDocument {
    /// Get the PDS (Personal Data Server) endpoint from the DID document
    pub fn get_pds_endpoint(&self) -> Option<String> {
        self.service
            .iter()
            .find(|s| s.service_type == "AtprotoPersonalDataServer")
            .map(|s| s.service_endpoint.clone())
    }

    /// Get the handle from alsoKnownAs
    #[allow(dead_code)]
    pub fn get_handle(&self) -> Option<String> {
        self.also_known_as
            .iter()
            .find(|h| h.starts_with("at://"))
            .map(|h| h.strip_prefix("at://").unwrap_or(h).to_string())
    }
}

/// Verification method in a DID document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationMethod {
    pub id: String,
    #[serde(rename = "type")]
    pub method_type: String,
    pub controller: String,
    #[serde(rename = "publicKeyMultibase")]
    pub public_key_multibase: Option<String>,
}

/// Service endpoint in a DID document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    #[serde(rename = "type")]
    pub service_type: String,
    #[serde(rename = "serviceEndpoint")]
    pub service_endpoint: String,
}

/// Validate ATProto handle format
///
/// Valid handles:
/// - Must have at least 2 segments separated by dots
/// - Each segment must be alphanumeric (with hyphens allowed, not at start/end)
/// - TLD must be at least 2 characters
fn is_valid_handle(handle: &str) -> bool {
    if handle.is_empty() || handle.len() > 253 {
        return false;
    }

    let segments: Vec<&str> = handle.split('.').collect();
    if segments.len() < 2 {
        return false;
    }

    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() || segment.len() > 63 {
            return false;
        }

        // Check for valid characters
        if !segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }

        // Can't start or end with hyphen
        if segment.starts_with('-') || segment.ends_with('-') {
            return false;
        }

        // TLD must be at least 2 chars and not all digits
        if i == segments.len() - 1 {
            if segment.len() < 2 || segment.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
        }
    }

    true
}

/// Validate DID format
///
/// Valid DIDs for ATProto:
/// - `did:plc:<identifier>`
/// - `did:web:<domain>`
fn is_valid_did(did: &str) -> bool {
    if did.starts_with("did:plc:") {
        // did:plc identifier is 24 chars base32
        let identifier = did.strip_prefix("did:plc:").unwrap();
        !identifier.is_empty()
            && identifier.len() <= 64
            && identifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric())
    } else if did.starts_with("did:web:") {
        let domain = did.strip_prefix("did:web:").unwrap();
        !domain.is_empty() && domain.len() <= 253
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_handles() {
        assert!(is_valid_handle("user.bsky.social"));
        assert!(is_valid_handle("alice.example.com"));
        assert!(is_valid_handle("test-user.bsky.social"));
        assert!(is_valid_handle("a.co"));
    }

    #[test]
    fn test_invalid_handles() {
        assert!(!is_valid_handle("")); // Empty
        assert!(!is_valid_handle("single")); // No dot
        assert!(!is_valid_handle(".bsky.social")); // Starts with dot
        assert!(!is_valid_handle("user.")); // Ends with dot
        assert!(!is_valid_handle("-user.bsky.social")); // Starts with hyphen
        assert!(!is_valid_handle("user-.bsky.social")); // Ends with hyphen
        assert!(!is_valid_handle("user.bsky.1")); // TLD is single char
        assert!(!is_valid_handle("user.bsky.123")); // TLD is all digits
    }

    #[test]
    fn test_valid_dids() {
        assert!(is_valid_did("did:plc:ewvi7nxzy7mbhber23pb6il4"));
        assert!(is_valid_did("did:web:example.com"));
        assert!(is_valid_did("did:web:blog.example.com"));
    }

    #[test]
    fn test_invalid_dids() {
        assert!(!is_valid_did("")); // Empty
        assert!(!is_valid_did("not-a-did")); // No did: prefix
        assert!(!is_valid_did("did:other:something")); // Unsupported method
        assert!(!is_valid_did("did:plc:")); // Empty identifier
        assert!(!is_valid_did("did:web:")); // Empty domain
    }

    #[test]
    fn test_did_document_pds_endpoint() {
        let doc = DidDocument {
            id: "did:plc:test123".to_string(),
            also_known_as: vec!["at://user.bsky.social".to_string()],
            verification_method: vec![],
            service: vec![Service {
                id: "#atproto_pds".to_string(),
                service_type: "AtprotoPersonalDataServer".to_string(),
                service_endpoint: "https://bsky.social".to_string(),
            }],
        };

        assert_eq!(
            doc.get_pds_endpoint(),
            Some("https://bsky.social".to_string())
        );
    }

    #[test]
    fn test_did_document_handle() {
        let doc = DidDocument {
            id: "did:plc:test123".to_string(),
            also_known_as: vec!["at://alice.bsky.social".to_string()],
            verification_method: vec![],
            service: vec![],
        };

        assert_eq!(doc.get_handle(), Some("alice.bsky.social".to_string()));
    }
}
