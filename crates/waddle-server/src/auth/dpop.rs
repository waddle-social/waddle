//! DPoP (Demonstrating Proof of Possession) implementation for ATProto OAuth
//!
//! DPoP is mandatory for Bluesky OAuth. This module handles:
//! - ES256 (P-256) keypair generation
//! - DPoP proof JWT creation
//! - Nonce tracking

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::{signature::Signer, Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::rand_core::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// DPoP keypair for a single OAuth session
#[derive(Clone)]
pub struct DpopKeyPair {
    /// The signing (private) key
    signing_key: SigningKey,
    /// The verifying (public) key
    verifying_key: VerifyingKey,
    /// Current nonce from the authorization server (for auth requests)
    pub auth_nonce: Option<String>,
    /// Current nonce from the resource server (for PDS requests)
    pub resource_nonce: Option<String>,
}

impl std::fmt::Debug for DpopKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DpopKeyPair")
            .field("auth_nonce", &self.auth_nonce)
            .field("resource_nonce", &self.resource_nonce)
            .finish_non_exhaustive()
    }
}

impl DpopKeyPair {
    /// Generate a new DPoP keypair
    pub fn generate() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        Self {
            signing_key,
            verifying_key,
            auth_nonce: None,
            resource_nonce: None,
        }
    }

    /// Get the public key as a JWK
    pub fn public_jwk(&self) -> DpopJwk {
        // Get the public key point coordinates
        let public_key = self.verifying_key.to_encoded_point(false);
        let x = public_key.x().expect("x coordinate");
        let y = public_key.y().expect("y coordinate");

        DpopJwk {
            kty: "EC".to_string(),
            crv: "P-256".to_string(),
            x: URL_SAFE_NO_PAD.encode(x),
            y: URL_SAFE_NO_PAD.encode(y),
        }
    }

    /// Create a DPoP proof JWT for a request
    ///
    /// # Arguments
    /// * `method` - HTTP method (e.g., "POST", "GET")
    /// * `url` - Full request URL
    /// * `nonce` - Optional server-provided nonce
    /// * `access_token` - Optional access token (for ath claim when accessing resources)
    pub fn create_proof(
        &self,
        method: &str,
        url: &str,
        nonce: Option<&str>,
        access_token: Option<&str>,
    ) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        // Generate unique jti
        let jti: String = {
            let bytes: [u8; 16] = rand::rng().random();
            URL_SAFE_NO_PAD.encode(bytes)
        };

        // Build header
        let header = DpopHeader {
            typ: "dpop+jwt".to_string(),
            alg: "ES256".to_string(),
            jwk: self.public_jwk(),
        };

        // Build claims
        let mut claims = DpopClaims {
            jti,
            htm: method.to_uppercase(),
            htu: url.to_string(),
            iat: now,
            exp: Some(now + 300), // 5 minute expiry
            nonce: nonce.map(String::from),
            ath: None,
        };

        // Add access token hash if provided
        if let Some(token) = access_token {
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            let hash = hasher.finalize();
            claims.ath = Some(URL_SAFE_NO_PAD.encode(hash));
        }

        // Encode header and claims
        let header_b64 =
            URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).expect("serialize header"));
        let claims_b64 =
            URL_SAFE_NO_PAD.encode(serde_json::to_string(&claims).expect("serialize claims"));

        // Create signing input
        let signing_input = format!("{}.{}", header_b64, claims_b64);

        // Sign with ES256
        let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        format!("{}.{}", signing_input, sig_b64)
    }
}

/// DPoP JWT header
#[derive(Debug, Serialize, Deserialize)]
struct DpopHeader {
    typ: String,
    alg: String,
    jwk: DpopJwk,
}

/// DPoP public key in JWK format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DpopJwk {
    kty: String,
    crv: String,
    x: String,
    y: String,
}

/// DPoP JWT claims
#[derive(Debug, Serialize, Deserialize)]
struct DpopClaims {
    /// Unique token ID (must be unique per request)
    jti: String,
    /// HTTP method
    htm: String,
    /// HTTP URL
    htu: String,
    /// Issued at (UNIX timestamp)
    iat: u64,
    /// Expiration (UNIX timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    exp: Option<u64>,
    /// Server-provided nonce
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    /// Access token hash (for resource server requests)
    #[serde(skip_serializing_if = "Option::is_none")]
    ath: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let kp = DpopKeyPair::generate();
        let jwk = kp.public_jwk();

        assert_eq!(jwk.kty, "EC");
        assert_eq!(jwk.crv, "P-256");
        assert!(!jwk.x.is_empty());
        assert!(!jwk.y.is_empty());
    }

    #[test]
    fn test_proof_creation() {
        let kp = DpopKeyPair::generate();
        let proof = kp.create_proof("POST", "https://example.com/token", None, None);

        // JWT should have 3 parts
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);

        // Decode and verify header
        let header_json = URL_SAFE_NO_PAD.decode(parts[0]).expect("decode header");
        let header: DpopHeader = serde_json::from_slice(&header_json).expect("parse header");
        assert_eq!(header.typ, "dpop+jwt");
        assert_eq!(header.alg, "ES256");
    }

    #[test]
    fn test_proof_with_nonce() {
        let kp = DpopKeyPair::generate();
        let proof = kp.create_proof(
            "POST",
            "https://example.com/token",
            Some("test-nonce"),
            None,
        );

        let parts: Vec<&str> = proof.split('.').collect();
        let claims_json = URL_SAFE_NO_PAD.decode(parts[1]).expect("decode claims");
        let claims: DpopClaims = serde_json::from_slice(&claims_json).expect("parse claims");

        assert_eq!(claims.nonce, Some("test-nonce".to_string()));
    }

    #[test]
    fn test_proof_with_access_token() {
        let kp = DpopKeyPair::generate();
        let proof = kp.create_proof(
            "GET",
            "https://pds.example.com/xrpc/foo",
            None,
            Some("test-token"),
        );

        let parts: Vec<&str> = proof.split('.').collect();
        let claims_json = URL_SAFE_NO_PAD.decode(parts[1]).expect("decode claims");
        let claims: DpopClaims = serde_json::from_slice(&claims_json).expect("parse claims");

        assert!(claims.ath.is_some());
    }
}
