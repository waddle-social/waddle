//! SCRAM-SHA-256 SASL mechanism implementation.
//!
//! Implements the server side of SCRAM-SHA-256 authentication per:
//! - RFC 5802: Salted Challenge Response Authentication Mechanism (SCRAM)
//! - RFC 7677: SCRAM-SHA-256 and SCRAM-SHA-256-PLUS
//!
//! SCRAM is a challenge-response authentication mechanism that provides:
//! - Mutual authentication (client and server verify each other)
//! - Channel binding support (SCRAM-SHA-256-PLUS)
//! - Password hash storage without plaintext
//!
//! ## Protocol Flow
//!
//! 1. Client sends client-first-message: `n,,n=user,r=clientnonce`
//! 2. Server sends server-first-message: `r=clientnonce+servernonce,s=salt,i=iterations`
//! 3. Client sends client-final-message: `c=channel,r=nonce,p=clientproof`
//! 4. Server verifies and sends server-final-message: `v=serversignature`

use crate::XmppError;
use base64::prelude::*;

mod crypto;
mod parser;

pub use crypto::{generate_salt, generate_scram_keys};
pub use parser::encode_sasl_name;

#[cfg(test)]
use crypto::hi;
use crypto::{generate_nonce, hmac_sha256, sha256};
use parser::{parse_client_final, parse_client_first};

#[cfg(test)]
mod tests;

/// Default number of PBKDF2 iterations for SCRAM-SHA-256.
/// RFC 7677 recommends at least 4096, we use 4096 as a reasonable default.
pub const DEFAULT_ITERATIONS: u32 = 4096;

/// State machine for SCRAM-SHA-256 server-side authentication.
///
/// This struct manages the multi-step SCRAM exchange:
/// 1. Process client-first-message, generate server-first-message
/// 2. Process client-final-message, verify proof, generate server-final-message
#[derive(Debug, Clone)]
pub struct ScramServer {
    /// The authentication state
    state: ScramState,
    /// Combined client-first-message-bare for auth message computation
    client_first_message_bare: String,
    /// Server-first-message for auth message computation
    server_first_message: String,
    /// Combined nonce (client + server)
    combined_nonce: String,
    /// The username extracted from client-first-message
    username: String,
    /// Salt used for this authentication (base64 encoded)
    salt_b64: String,
    /// Number of iterations for PBKDF2
    iterations: u32,
}

/// SCRAM authentication state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramState {
    /// Initial state, waiting for client-first-message
    Initial,
    /// Received client-first, waiting for client-final
    WaitingForClientFinal,
    /// Authentication complete (success or failure)
    Complete,
}

/// Result of processing client-first-message.
#[derive(Debug, Clone)]
pub struct ServerFirstMessage {
    /// The server-first-message to send to the client
    pub message: String,
    /// The extracted username (for password lookup)
    pub username: String,
}

/// Result of processing client-final-message.
#[derive(Debug, Clone)]
pub struct ServerFinalMessage {
    /// The server-final-message to send to the client (contains server signature)
    pub message: String,
}

impl ScramServer {
    /// Create a new SCRAM server instance with a random salt.
    pub fn new() -> Self {
        let salt = generate_salt();
        Self::with_salt(salt, DEFAULT_ITERATIONS)
    }

    /// Create a new SCRAM server instance with specific salt and iterations.
    ///
    /// Use this when you have a stored salt for the user (for consistent password verification).
    pub fn with_salt(salt: Vec<u8>, iterations: u32) -> Self {
        Self {
            state: ScramState::Initial,
            client_first_message_bare: String::new(),
            server_first_message: String::new(),
            combined_nonce: String::new(),
            username: String::new(),
            salt_b64: BASE64_STANDARD.encode(&salt),
            iterations,
        }
    }

    /// Create a SCRAM server with a base64-encoded salt.
    ///
    /// This is useful when loading a stored salt from the database.
    pub fn with_salt_b64(salt_b64: String, iterations: u32) -> Self {
        Self {
            state: ScramState::Initial,
            client_first_message_bare: String::new(),
            server_first_message: String::new(),
            combined_nonce: String::new(),
            username: String::new(),
            salt_b64,
            iterations,
        }
    }

    /// Get the current state of the SCRAM exchange.
    pub fn state(&self) -> &ScramState {
        &self.state
    }

    /// Get the username extracted from the client-first-message.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Get the salt (base64 encoded) being used.
    pub fn salt_b64(&self) -> &str {
        &self.salt_b64
    }

    /// Get the iteration count.
    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    /// Process the client-first-message and generate server-first-message.
    ///
    /// # Arguments
    /// * `client_first` - The client-first-message (already base64 decoded)
    ///
    /// # Returns
    /// * `ServerFirstMessage` containing the message to send and the username for lookup
    pub fn process_client_first(
        &mut self,
        client_first: &str,
    ) -> Result<ServerFirstMessage, XmppError> {
        if self.state != ScramState::Initial {
            return Err(XmppError::auth_failed(
                "Invalid SCRAM state for client-first",
            ));
        }

        // Parse client-first-message
        let parsed = parse_client_first(client_first)?;

        // We only support 'n' (no channel binding) for now
        if parsed.gs2_cbind_flag != 'n' {
            return Err(XmppError::auth_failed("Channel binding not supported"));
        }

        // RFC 5802 §5.1: authzid support is optional; we do not implement
        // authorization-identity mapping, so reject rather than silently
        // authenticating as a different identity than requested.
        if parsed.authzid.is_some() {
            return Err(XmppError::auth_failed("SCRAM authzid is not supported"));
        }

        // Store username
        self.username = parsed.username.clone();

        // Store client-first-message-bare for auth message computation
        self.client_first_message_bare = parsed.bare.clone();

        // Generate server nonce and combine with client nonce
        let server_nonce = generate_nonce();
        self.combined_nonce = format!("{}{}", parsed.client_nonce, server_nonce);

        // Build server-first-message: r=nonce,s=salt,i=iterations
        self.server_first_message = format!(
            "r={},s={},i={}",
            self.combined_nonce, self.salt_b64, self.iterations
        );

        // Update state
        self.state = ScramState::WaitingForClientFinal;

        Ok(ServerFirstMessage {
            message: self.server_first_message.clone(),
            username: self.username.clone(),
        })
    }

    /// Process the client-final-message and verify the client proof.
    ///
    /// # Arguments
    /// * `client_final` - The client-final-message (already base64 decoded)
    /// * `stored_key` - The StoredKey for the user (from password storage)
    /// * `server_key` - The ServerKey for the user (from password storage)
    ///
    /// # Returns
    /// * `ServerFinalMessage` containing the server signature to send
    pub fn process_client_final(
        &mut self,
        client_final: &str,
        stored_key: &[u8],
        server_key: &[u8],
    ) -> Result<ServerFinalMessage, XmppError> {
        if self.state != ScramState::WaitingForClientFinal {
            return Err(XmppError::auth_failed(
                "Invalid SCRAM state for client-final",
            ));
        }

        // Parse client-final-message
        let parsed = parse_client_final(client_final)?;

        // RFC 5802 §5.1: the `c=` value MUST be the base64 encoding of the
        // GS2 header from client-first. process_client_first only accepts
        // the flag 'n' with no authzid, i.e. the header "n,,", whose
        // base64 encoding is the constant below.
        const GS2_HEADER_NO_CHANNEL_BINDING_B64: &str = "biws";
        if parsed.channel_binding != GS2_HEADER_NO_CHANNEL_BINDING_B64 {
            self.state = ScramState::Complete;
            return Err(XmppError::auth_failed("Channel binding mismatch"));
        }

        // Verify the nonce matches
        if parsed.nonce != self.combined_nonce {
            self.state = ScramState::Complete;
            return Err(XmppError::auth_failed("Nonce mismatch"));
        }

        // Compute AuthMessage = client-first-message-bare + "," + server-first-message + "," + client-final-message-without-proof
        let auth_message = format!(
            "{},{},{}",
            self.client_first_message_bare, self.server_first_message, parsed.without_proof
        );

        // Verify client proof
        // ClientSignature = HMAC(StoredKey, AuthMessage)
        let client_signature = hmac_sha256(stored_key, auth_message.as_bytes());

        // ClientKey = ClientProof XOR ClientSignature
        let client_proof = BASE64_STANDARD
            .decode(&parsed.proof)
            .map_err(|e| XmppError::auth_failed(format!("Invalid client proof base64: {}", e)))?;

        if client_proof.len() != client_signature.len() {
            self.state = ScramState::Complete;
            return Err(XmppError::auth_failed("Invalid client proof length"));
        }

        let client_key: Vec<u8> = client_proof
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();

        // Verify: H(ClientKey) == StoredKey
        let computed_stored_key = sha256(&client_key);
        if computed_stored_key != stored_key {
            self.state = ScramState::Complete;
            return Err(XmppError::auth_failed("Authentication failed"));
        }

        // Compute ServerSignature = HMAC(ServerKey, AuthMessage)
        let server_signature = hmac_sha256(server_key, auth_message.as_bytes());
        let server_signature_b64 = BASE64_STANDARD.encode(&server_signature);

        // Build server-final-message: v=signature
        let server_final = format!("v={}", server_signature_b64);

        self.state = ScramState::Complete;

        Ok(ServerFinalMessage {
            message: server_final,
        })
    }
}

impl Default for ScramServer {
    fn default() -> Self {
        Self::new()
    }
}
