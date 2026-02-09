//! ATProto OAuth Implementation
//!
//! Implements the OAuth 2.0 authorization flow for ATProto/Bluesky authentication.
//!
//! # OAuth Flow
//!
//! 1. Generate PKCE code_verifier and code_challenge
//! 2. Discover authorization server from PDS
//! 3. Build authorization URL with required parameters
//! 4. User authenticates and is redirected back with authorization code
//! 5. Exchange authorization code for tokens
//!
//! # Security
//!
//! - Uses PKCE (Proof Key for Code Exchange) to prevent authorization code interception
//! - State parameter prevents CSRF attacks
//! - Tokens are stored encrypted in the database

use super::did::DidResolver;
use super::dpop::DpopKeyPair;
use super::AuthError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, instrument, warn};
use url::Url;

/// ATProto OAuth client
#[derive(Clone)]
pub struct AtprotoOAuth {
    http_client: Client,
    did_resolver: DidResolver,
    client_id: String,
    redirect_uri: String,
}

impl AtprotoOAuth {
    /// Create a new ATProto OAuth client
    ///
    /// # Arguments
    ///
    /// * `client_id` - The OAuth client ID (typically a URL to client metadata)
    /// * `redirect_uri` - The callback URL for OAuth redirects
    pub fn new(client_id: &str, redirect_uri: &str) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Waddle/1.0")
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http_client,
            did_resolver: DidResolver::new(),
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
        }
    }

    /// Create an OAuth client with a custom DID resolver (for testing)
    #[allow(dead_code)]
    pub fn with_did_resolver(
        client_id: &str,
        redirect_uri: &str,
        did_resolver: DidResolver,
    ) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Waddle/1.0")
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http_client,
            did_resolver,
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
        }
    }

    /// Get the DID resolver
    #[allow(dead_code)]
    pub fn did_resolver(&self) -> &DidResolver {
        &self.did_resolver
    }

    /// Start the OAuth authorization flow for a given handle
    ///
    /// Returns an `AuthorizationRequest` containing the authorization URL
    /// and all parameters needed to complete the flow.
    ///
    /// Bluesky OAuth requires:
    /// - PKCE (Proof Key for Code Exchange)
    /// - PAR (Pushed Authorization Requests)
    /// - DPoP (Demonstrating Proof of Possession)
    #[instrument(skip(self), fields(handle = %handle))]
    pub async fn start_authorization(
        &self,
        handle: &str,
    ) -> Result<AuthorizationRequest, AuthError> {
        self.start_authorization_with_redirect(handle, None).await
    }

    /// Start OAuth authorization flow with optional redirect_uri override
    ///
    /// This is used by the device flow which needs a different callback URL.
    pub async fn start_authorization_with_redirect(
        &self,
        handle: &str,
        redirect_uri_override: Option<&str>,
    ) -> Result<AuthorizationRequest, AuthError> {
        let redirect_uri = redirect_uri_override.unwrap_or(&self.redirect_uri);

        // Step 1: Resolve handle to DID
        debug!("Resolving handle to DID: {}", handle);
        let did = self.did_resolver.resolve_handle(handle).await?;
        debug!("Resolved DID: {}", did);

        // Step 2: Get DID document to find PDS
        debug!("Fetching DID document for: {}", did);
        let did_doc = self.did_resolver.resolve_did_document(&did).await?;

        let pds_url = did_doc.get_pds_endpoint().ok_or_else(|| {
            AuthError::OAuthDiscoveryFailed("No PDS endpoint found in DID document".to_string())
        })?;
        debug!("Found PDS endpoint: {}", pds_url);

        // Step 3: Discover OAuth authorization server from PDS
        let auth_server = self.discover_authorization_server(&pds_url).await?;
        debug!("Discovered authorization server: {:?}", auth_server);

        // Step 4: Generate PKCE parameters
        let (code_verifier, code_challenge) = generate_pkce();
        debug!("Generated PKCE challenge");

        // Step 5: Generate state parameter
        let state = generate_state();

        // Step 6: Generate DPoP keypair
        let dpop_keypair = DpopKeyPair::generate();
        debug!("Generated DPoP keypair");

        // Step 7: Get PAR endpoint (required for Bluesky)
        let par_endpoint = auth_server
            .pushed_authorization_request_endpoint
            .as_ref()
            .ok_or_else(|| {
                AuthError::OAuthDiscoveryFailed(
                    "Authorization server does not support PAR (required for Bluesky)".to_string(),
                )
            })?;

        // Step 8: Make PAR request with DPoP
        let authorization_url = self
            .make_par_request(
                par_endpoint,
                &auth_server.authorization_endpoint,
                &code_challenge,
                &state,
                &did,
                &dpop_keypair,
                redirect_uri,
            )
            .await?;

        Ok(AuthorizationRequest {
            authorization_url,
            state,
            code_verifier,
            did,
            handle: handle.to_string(),
            pds_url,
            token_endpoint: auth_server.token_endpoint,
            dpop_keypair,
            issuer: auth_server.issuer,
            redirect_uri: redirect_uri.to_string(),
        })
    }

    /// Make a Pushed Authorization Request (PAR)
    ///
    /// Returns the authorization URL to redirect the user to.
    async fn make_par_request(
        &self,
        par_endpoint: &str,
        authorization_endpoint: &str,
        code_challenge: &str,
        state: &str,
        did: &str,
        dpop_keypair: &DpopKeyPair,
        redirect_uri: &str,
    ) -> Result<String, AuthError> {
        debug!("Making PAR request to: {}", par_endpoint);

        // Build PAR request body
        let params = [
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", redirect_uri),
            ("scope", "atproto transition:generic"),
            ("state", state),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("login_hint", did),
        ];

        // First attempt without nonce (we'll get one back in the error)
        let dpop_proof = dpop_keypair.create_proof("POST", par_endpoint, None, None);

        let response = self
            .http_client
            .post(par_endpoint)
            .header("DPoP", &dpop_proof)
            .form(&params)
            .send()
            .await
            .map_err(|e| {
                AuthError::OAuthAuthorizationFailed(format!("PAR request failed: {}", e))
            })?;

        // Check for DPoP nonce error (expected on first request)
        if response.status().as_u16() == 400 || response.status().as_u16() == 401 {
            if let Some(nonce) = response.headers().get("DPoP-Nonce") {
                let nonce_str = nonce.to_str().unwrap_or("");
                debug!("Got DPoP nonce: {}", nonce_str);

                // Retry with nonce
                let dpop_proof_with_nonce =
                    dpop_keypair.create_proof("POST", par_endpoint, Some(nonce_str), None);

                let retry_response = self
                    .http_client
                    .post(par_endpoint)
                    .header("DPoP", &dpop_proof_with_nonce)
                    .form(&params)
                    .send()
                    .await
                    .map_err(|e| {
                        AuthError::OAuthAuthorizationFailed(format!(
                            "PAR retry request failed: {}",
                            e
                        ))
                    })?;

                return self
                    .handle_par_response(retry_response, authorization_endpoint)
                    .await;
            }
        }

        self.handle_par_response(response, authorization_endpoint)
            .await
    }

    /// Handle PAR response and build authorization URL
    async fn handle_par_response(
        &self,
        response: reqwest::Response,
        authorization_endpoint: &str,
    ) -> Result<String, AuthError> {
        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            warn!("PAR request failed: {} - {}", status, error_body);
            return Err(AuthError::OAuthAuthorizationFailed(format!(
                "PAR request failed with status {}: {}",
                status, error_body
            )));
        }

        let par_response: ParResponse = response.json().await.map_err(|e| {
            AuthError::OAuthAuthorizationFailed(format!("Failed to parse PAR response: {}", e))
        })?;

        info!(
            "PAR successful, got request_uri: {}",
            par_response.request_uri
        );

        // Build authorization URL with request_uri
        let mut auth_url = Url::parse(authorization_endpoint).map_err(|e| {
            AuthError::OAuthAuthorizationFailed(format!("Invalid authorization endpoint: {}", e))
        })?;

        auth_url
            .query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("request_uri", &par_response.request_uri);

        Ok(auth_url.to_string())
    }

    /// Discover the OAuth authorization server from a PDS
    ///
    /// Bluesky uses a two-step discovery process:
    /// 1. Fetch `/.well-known/oauth-protected-resource` from the PDS (Resource Server)
    /// 2. Extract the Authorization Server URL from `authorization_servers` array
    /// 3. Fetch `/.well-known/oauth-authorization-server` from the Authorization Server
    #[instrument(skip(self), fields(pds_url = %pds_url))]
    pub async fn discover_authorization_server(
        &self,
        pds_url: &str,
    ) -> Result<AuthorizationServerMetadata, AuthError> {
        let pds_base = pds_url.trim_end_matches('/');

        // Step 1: Fetch Protected Resource metadata from the PDS
        let resource_url = format!("{}/.well-known/oauth-protected-resource", pds_base);
        debug!(
            "Fetching protected resource metadata from: {}",
            resource_url
        );

        let response = self
            .http_client
            .get(&resource_url)
            .send()
            .await
            .map_err(|e| {
                AuthError::OAuthDiscoveryFailed(format!(
                    "Failed to fetch protected resource metadata: {}",
                    e
                ))
            })?;

        if !response.status().is_success() {
            return Err(AuthError::OAuthDiscoveryFailed(format!(
                "Protected resource metadata endpoint returned status {}",
                response.status()
            )));
        }

        let resource_meta: ProtectedResourceMetadata = response.json().await.map_err(|e| {
            AuthError::OAuthDiscoveryFailed(format!(
                "Failed to parse protected resource metadata: {}",
                e
            ))
        })?;

        // Step 2: Get the Authorization Server URL
        let auth_server_url = resource_meta.authorization_servers.first().ok_or_else(|| {
            AuthError::OAuthDiscoveryFailed(
                "No authorization servers found in protected resource metadata".to_string(),
            )
        })?;

        debug!("Found authorization server: {}", auth_server_url);

        // Step 3: Fetch Authorization Server metadata
        let auth_meta_url = format!(
            "{}/.well-known/oauth-authorization-server",
            auth_server_url.trim_end_matches('/')
        );
        debug!(
            "Fetching authorization server metadata from: {}",
            auth_meta_url
        );

        let response = self
            .http_client
            .get(&auth_meta_url)
            .send()
            .await
            .map_err(|e| {
                AuthError::OAuthDiscoveryFailed(format!(
                    "Failed to fetch authorization server metadata: {}",
                    e
                ))
            })?;

        if !response.status().is_success() {
            return Err(AuthError::OAuthDiscoveryFailed(format!(
                "Authorization server metadata endpoint returned status {}",
                response.status()
            )));
        }

        let metadata: AuthorizationServerMetadata = response.json().await.map_err(|e| {
            AuthError::OAuthDiscoveryFailed(format!(
                "Failed to parse authorization server metadata: {}",
                e
            ))
        })?;

        Ok(metadata)
    }

    /// Exchange an authorization code for tokens
    ///
    /// This is called after the user is redirected back with an authorization code.
    /// DPoP proof is required for Bluesky OAuth.
    /// The redirect_uri must match the one used in the authorization request.
    #[instrument(skip(self, code_verifier, dpop_keypair))]
    pub async fn exchange_code(
        &self,
        token_endpoint: &str,
        code: &str,
        code_verifier: &str,
        dpop_keypair: &DpopKeyPair,
        redirect_uri: &str,
    ) -> Result<TokenResponse, AuthError> {
        debug!("Exchanging authorization code for tokens");

        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &self.client_id),
            ("code_verifier", code_verifier),
        ];

        // First attempt without nonce
        let dpop_proof = dpop_keypair.create_proof("POST", token_endpoint, None, None);

        let response = self
            .http_client
            .post(token_endpoint)
            .header("DPoP", &dpop_proof)
            .form(&params)
            .send()
            .await
            .map_err(|e| AuthError::TokenExchangeFailed(format!("Token request failed: {}", e)))?;

        // Check for DPoP nonce error
        if response.status().as_u16() == 400 || response.status().as_u16() == 401 {
            if let Some(nonce) = response.headers().get("DPoP-Nonce") {
                let nonce_str = nonce.to_str().unwrap_or("");
                debug!("Got DPoP nonce for token exchange: {}", nonce_str);

                // Retry with nonce
                let dpop_proof_with_nonce =
                    dpop_keypair.create_proof("POST", token_endpoint, Some(nonce_str), None);

                let retry_response = self
                    .http_client
                    .post(token_endpoint)
                    .header("DPoP", &dpop_proof_with_nonce)
                    .form(&params)
                    .send()
                    .await
                    .map_err(|e| {
                        AuthError::TokenExchangeFailed(format!("Token retry request failed: {}", e))
                    })?;

                return self.handle_token_response(retry_response).await;
            }
        }

        self.handle_token_response(response).await
    }

    /// Handle token response
    async fn handle_token_response(
        &self,
        response: reqwest::Response,
    ) -> Result<TokenResponse, AuthError> {
        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            warn!("Token request failed: {} - {}", status, error_body);
            return Err(AuthError::TokenExchangeFailed(format!(
                "Token endpoint returned status {}: {}",
                status, error_body
            )));
        }

        let token_response: TokenResponse = response.json().await.map_err(|e| {
            AuthError::TokenExchangeFailed(format!("Failed to parse token response: {}", e))
        })?;

        debug!("Successfully exchanged code for tokens");
        Ok(token_response)
    }

    /// Refresh an access token using a refresh token
    #[instrument(skip(self, refresh_token))]
    pub async fn refresh_token(
        &self,
        token_endpoint: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse, AuthError> {
        debug!("Refreshing access token");

        let mut params = HashMap::new();
        params.insert("grant_type", "refresh_token");
        params.insert("refresh_token", refresh_token);
        params.insert("client_id", &self.client_id);

        let response = self
            .http_client
            .post(token_endpoint)
            .form(&params)
            .send()
            .await
            .map_err(|e| AuthError::TokenExchangeFailed(format!("Token refresh failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(AuthError::TokenExchangeFailed(format!(
                "Token refresh returned status {}: {}",
                status, error_body
            )));
        }

        let token_response: TokenResponse = response.json().await.map_err(|e| {
            AuthError::TokenExchangeFailed(format!("Failed to parse token response: {}", e))
        })?;

        debug!("Successfully refreshed token");
        Ok(token_response)
    }
}

/// Protected Resource metadata (from PDS)
///
/// Fetched from `/.well-known/oauth-protected-resource` on the PDS.
/// This tells us where to find the Authorization Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// The resource identifier (usually the PDS URL)
    pub resource: String,

    /// List of authorization server URLs
    pub authorization_servers: Vec<String>,

    /// Bearer token methods supported
    #[serde(default)]
    pub bearer_methods_supported: Vec<String>,

    /// Scopes supported by this resource server
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// Authorization server metadata (OAuth 2.0 / OpenID Connect discovery)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationServerMetadata {
    /// Issuer identifier
    pub issuer: String,

    /// Authorization endpoint URL
    pub authorization_endpoint: String,

    /// Token endpoint URL
    pub token_endpoint: String,

    /// Pushed authorization request endpoint (optional)
    #[serde(default)]
    pub pushed_authorization_request_endpoint: Option<String>,

    /// Token endpoint auth methods supported
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,

    /// Response types supported
    #[serde(default)]
    pub response_types_supported: Vec<String>,

    /// Grant types supported
    #[serde(default)]
    pub grant_types_supported: Vec<String>,

    /// Code challenge methods supported
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,

    /// Scopes supported
    #[serde(default)]
    pub scopes_supported: Vec<String>,

    /// DPoP signing algorithms supported
    #[serde(default)]
    pub dpop_signing_alg_values_supported: Vec<String>,
}

/// PAR (Pushed Authorization Request) response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParResponse {
    /// The request URI to use in the authorization URL
    pub request_uri: String,
    /// How long until the request expires (seconds)
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Request to start OAuth authorization
///
/// Contains all the information needed to redirect the user
/// and later complete the authorization flow.
#[derive(Debug, Clone)]
pub struct AuthorizationRequest {
    /// The URL to redirect the user to for authorization
    pub authorization_url: String,

    /// State parameter (must be verified in callback)
    pub state: String,

    /// PKCE code verifier (needed for token exchange)
    pub code_verifier: String,

    /// User's DID
    pub did: String,

    /// User's handle
    pub handle: String,

    /// PDS URL
    pub pds_url: String,

    /// Token endpoint URL
    pub token_endpoint: String,

    /// DPoP keypair for this session (used in token exchange)
    pub dpop_keypair: DpopKeyPair,

    /// Authorization server issuer
    pub issuer: String,

    /// Redirect URI used for this request (needed for token exchange)
    pub redirect_uri: String,
}

/// OAuth token response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    /// Access token
    pub access_token: String,

    /// Token type (usually "Bearer" or "DPoP")
    pub token_type: String,

    /// Expires in seconds
    #[serde(default)]
    pub expires_in: Option<u64>,

    /// Refresh token (optional)
    #[serde(default)]
    pub refresh_token: Option<String>,

    /// Scope granted
    #[serde(default)]
    pub scope: Option<String>,

    /// Subject (DID)
    #[serde(default)]
    pub sub: Option<String>,
}

/// Generate PKCE code_verifier and code_challenge
///
/// Returns (code_verifier, code_challenge) tuple.
/// The code_challenge is SHA-256 hash of code_verifier, base64url encoded.
pub fn generate_pkce() -> (String, String) {
    // Generate 32 random bytes for code_verifier
    let mut rng = rand::rng();
    let random_bytes: [u8; 32] = rng.random();
    let code_verifier = URL_SAFE_NO_PAD.encode(random_bytes);

    // Generate code_challenge = BASE64URL(SHA256(code_verifier))
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let hash = hasher.finalize();
    let code_challenge = URL_SAFE_NO_PAD.encode(hash);

    (code_verifier, code_challenge)
}

/// Generate a random state parameter for CSRF protection
pub fn generate_state() -> String {
    let mut rng = rand::rng();
    let random_bytes: [u8; 16] = rng.random();
    URL_SAFE_NO_PAD.encode(random_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_generation() {
        let (verifier, challenge) = generate_pkce();

        // Verify lengths
        assert_eq!(verifier.len(), 43); // 32 bytes base64url = 43 chars
        assert_eq!(challenge.len(), 43); // SHA256 hash base64url = 43 chars

        // Verify challenge is correct
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected_challenge);
    }

    #[test]
    fn test_state_generation() {
        let state1 = generate_state();
        let state2 = generate_state();

        // States should be different
        assert_ne!(state1, state2);

        // State should be 22 chars (16 bytes base64url)
        assert_eq!(state1.len(), 22);
    }

    #[test]
    fn test_pkce_uniqueness() {
        let (v1, c1) = generate_pkce();
        let (v2, c2) = generate_pkce();

        assert_ne!(v1, v2);
        assert_ne!(c1, c2);
    }
}
