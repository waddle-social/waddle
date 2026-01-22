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

use base64::prelude::*;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::XmppError;

/// Default number of PBKDF2 iterations for SCRAM-SHA-256.
/// RFC 7677 recommends at least 4096, we use 4096 as a reasonable default.
pub const DEFAULT_ITERATIONS: u32 = 4096;

/// Length of generated nonce in bytes (will be base64 encoded).
const NONCE_LENGTH: usize = 24;

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

/// Parsed client-first-message components.
#[derive(Debug, Clone)]
struct ClientFirstMessage {
    /// GS2 channel binding flag ('n', 'y', or 'p')
    gs2_cbind_flag: char,
    /// Optional authzid (reserved for future use)
    authzid: Option<String>,
    /// Username (authcid)
    username: String,
    /// Client nonce
    client_nonce: String,
    /// The bare message (without GS2 header) for auth message computation
    bare: String,
}

/// Parsed client-final-message components.
#[derive(Debug, Clone)]
struct ClientFinalMessage {
    /// Channel binding data (base64)
    channel_binding: String,
    /// Combined nonce
    nonce: String,
    /// Client proof (base64)
    proof: String,
    /// Message without proof for auth message computation
    without_proof: String,
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
    pub fn process_client_first(&mut self, client_first: &str) -> Result<ServerFirstMessage, XmppError> {
        if self.state != ScramState::Initial {
            return Err(XmppError::auth_failed("Invalid SCRAM state for client-first"));
        }

        // Parse client-first-message
        let parsed = parse_client_first(client_first)?;

        // We only support 'n' (no channel binding) for now
        if parsed.gs2_cbind_flag != 'n' {
            return Err(XmppError::auth_failed("Channel binding not supported"));
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
            return Err(XmppError::auth_failed("Invalid SCRAM state for client-final"));
        }

        // Parse client-final-message
        let parsed = parse_client_final(client_final)?;

        // Verify the nonce matches
        if parsed.nonce != self.combined_nonce {
            self.state = ScramState::Complete;
            return Err(XmppError::auth_failed("Nonce mismatch"));
        }

        // Compute AuthMessage = client-first-message-bare + "," + server-first-message + "," + client-final-message-without-proof
        let auth_message = format!(
            "{},{},{}",
            self.client_first_message_bare,
            self.server_first_message,
            parsed.without_proof
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

/// Generate SCRAM keys from a password and salt.
///
/// This is used to compute the keys that should be stored in the database.
/// The plaintext password should NEVER be stored.
///
/// # Arguments
/// * `password` - The user's plaintext password
/// * `salt` - The salt (should be random, at least 16 bytes)
/// * `iterations` - Number of PBKDF2 iterations (recommend >= 4096)
///
/// # Returns
/// * `(StoredKey, ServerKey)` tuple, both as raw bytes
pub fn generate_scram_keys(password: &str, salt: &[u8], iterations: u32) -> (Vec<u8>, Vec<u8>) {
    // SaltedPassword = Hi(Normalize(password), salt, i)
    let salted_password = hi(password.as_bytes(), salt, iterations);

    // ClientKey = HMAC(SaltedPassword, "Client Key")
    let client_key = hmac_sha256(&salted_password, b"Client Key");

    // StoredKey = H(ClientKey)
    let stored_key = sha256(&client_key);

    // ServerKey = HMAC(SaltedPassword, "Server Key")
    let server_key = hmac_sha256(&salted_password, b"Server Key");

    (stored_key, server_key)
}

/// Generate a random salt for SCRAM.
pub fn generate_salt() -> Vec<u8> {
    let mut salt = vec![0u8; 16];
    rand::rng().fill(&mut salt[..]);
    salt
}

/// Generate a random nonce string.
fn generate_nonce() -> String {
    let mut nonce_bytes = vec![0u8; NONCE_LENGTH];
    rand::rng().fill(&mut nonce_bytes[..]);
    BASE64_STANDARD.encode(&nonce_bytes)
}

/// Hi() function from RFC 5802 - PBKDF2-HMAC-SHA256.
fn hi(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut output = vec![0u8; 32]; // SHA-256 output is 32 bytes
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut output);
    output
}

/// HMAC-SHA-256 helper.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// SHA-256 hash helper.
fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Parse client-first-message.
///
/// Format: `gs2-header client-first-message-bare`
/// gs2-header: `[flag],authzid,`
/// client-first-message-bare: `n=username,r=nonce[,extensions]`
fn parse_client_first(message: &str) -> Result<ClientFirstMessage, XmppError> {
    // Split by comma, but we need to handle the GS2 header specially
    let parts: Vec<&str> = message.splitn(3, ',').collect();
    if parts.len() < 3 {
        return Err(XmppError::auth_failed("Invalid client-first-message format"));
    }

    // Parse GS2 header
    let gs2_cbind_flag = parts[0]
        .chars()
        .next()
        .ok_or_else(|| XmppError::auth_failed("Missing GS2 channel binding flag"))?;

    // Parse optional authzid (a=...)
    let authzid = if parts[1].starts_with("a=") {
        Some(parts[1][2..].to_string())
    } else if parts[1].is_empty() {
        None
    } else {
        return Err(XmppError::auth_failed("Invalid authzid format"));
    };

    // The rest is client-first-message-bare
    let bare = parts[2].to_string();

    // Parse the bare message for username and nonce
    let mut username = None;
    let mut client_nonce = None;

    for attr in bare.split(',') {
        if let Some(val) = attr.strip_prefix("n=") {
            // Decode username (RFC 5802 SASLprep and escaping)
            username = Some(decode_sasl_name(val)?);
        } else if let Some(val) = attr.strip_prefix("r=") {
            client_nonce = Some(val.to_string());
        }
        // Ignore other extensions
    }

    let username = username.ok_or_else(|| XmppError::auth_failed("Missing username in client-first-message"))?;
    let client_nonce = client_nonce.ok_or_else(|| XmppError::auth_failed("Missing nonce in client-first-message"))?;

    Ok(ClientFirstMessage {
        gs2_cbind_flag,
        authzid,
        username,
        client_nonce,
        bare,
    })
}

/// Parse client-final-message.
///
/// Format: `c=channel-binding,r=nonce,p=proof`
fn parse_client_final(message: &str) -> Result<ClientFinalMessage, XmppError> {
    let mut channel_binding = None;
    let mut nonce = None;
    let mut proof = None;

    // Find the proof part to separate it
    let proof_idx = message
        .rfind(",p=")
        .ok_or_else(|| XmppError::auth_failed("Missing proof in client-final-message"))?;

    let without_proof = &message[..proof_idx];

    for attr in message.split(',') {
        if let Some(val) = attr.strip_prefix("c=") {
            channel_binding = Some(val.to_string());
        } else if let Some(val) = attr.strip_prefix("r=") {
            nonce = Some(val.to_string());
        } else if let Some(val) = attr.strip_prefix("p=") {
            proof = Some(val.to_string());
        }
    }

    let channel_binding = channel_binding.ok_or_else(|| XmppError::auth_failed("Missing channel binding in client-final-message"))?;
    let nonce = nonce.ok_or_else(|| XmppError::auth_failed("Missing nonce in client-final-message"))?;
    let proof = proof.ok_or_else(|| XmppError::auth_failed("Missing proof in client-final-message"))?;

    Ok(ClientFinalMessage {
        channel_binding,
        nonce,
        proof,
        without_proof: without_proof.to_string(),
    })
}

/// Decode a SASL name (RFC 5802 escaping).
/// - `=2C` -> `,`
/// - `=3D` -> `=`
fn decode_sasl_name(name: &str) -> Result<String, XmppError> {
    let mut result = String::new();
    let mut chars = name.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '=' {
            let escape: String = chars.by_ref().take(2).collect();
            match escape.as_str() {
                "2C" => result.push(','),
                "3D" => result.push('='),
                _ => return Err(XmppError::auth_failed(format!("Invalid SASL name escape: ={}", escape))),
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Encode a SASL name (RFC 5802 escaping).
/// - `,` -> `=2C`
/// - `=` -> `=3D`
pub fn encode_sasl_name(name: &str) -> String {
    let mut result = String::new();
    for c in name.chars() {
        match c {
            ',' => result.push_str("=2C"),
            '=' => result.push_str("=3D"),
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test basic SCRAM key generation.
    #[test]
    fn test_generate_scram_keys() {
        let password = "password";
        let salt = b"salt1234salt1234"; // 16 bytes
        let iterations = 4096;

        let (stored_key, server_key) = generate_scram_keys(password, salt, iterations);

        // Keys should be 32 bytes (SHA-256 output)
        assert_eq!(stored_key.len(), 32);
        assert_eq!(server_key.len(), 32);

        // Keys should be deterministic
        let (stored_key2, server_key2) = generate_scram_keys(password, salt, iterations);
        assert_eq!(stored_key, stored_key2);
        assert_eq!(server_key, server_key2);
    }

    /// Test the full SCRAM exchange with a known password.
    #[test]
    fn test_scram_full_exchange() {
        // Setup: generate keys for a known password
        let password = "test-password";
        let salt = generate_salt();
        let iterations = 4096;
        let (stored_key, server_key) = generate_scram_keys(password, &salt, iterations);

        // Create server instance with the same salt
        let mut server = ScramServer::with_salt(salt.clone(), iterations);

        // Client sends client-first-message
        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let client_first = format!("n,,n=testuser,r={}", client_nonce);

        // Server processes and generates server-first-message
        let server_first = server.process_client_first(&client_first).unwrap();
        assert_eq!(server_first.username, "testuser");
        assert!(server_first.message.starts_with(&format!("r={}", client_nonce)));
        assert!(server_first.message.contains(",s="));
        assert!(server_first.message.contains(",i=4096"));

        // Extract the combined nonce for client-final
        let combined_nonce = &server.combined_nonce;

        // Client computes the proof
        // SaltedPassword = Hi(password, salt, i)
        let salted_password = hi(password.as_bytes(), &salt, iterations);

        // ClientKey = HMAC(SaltedPassword, "Client Key")
        let client_key = hmac_sha256(&salted_password, b"Client Key");

        // ClientSignature = HMAC(StoredKey, AuthMessage)
        let channel_binding = BASE64_STANDARD.encode("n,,");
        let client_final_without_proof = format!("c={},r={}", channel_binding, combined_nonce);
        let auth_message = format!(
            "n=testuser,r={},{},{}",
            client_nonce,
            server_first.message,
            client_final_without_proof
        );
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());

        // ClientProof = ClientKey XOR ClientSignature
        let client_proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();

        let client_proof_b64 = BASE64_STANDARD.encode(&client_proof);
        let client_final = format!("{},p={}", client_final_without_proof, client_proof_b64);

        // Server verifies and generates server-final-message
        let server_final = server
            .process_client_final(&client_final, &stored_key, &server_key)
            .unwrap();

        assert!(server_final.message.starts_with("v="));
        assert_eq!(server.state(), &ScramState::Complete);
    }

    /// Test parsing client-first-message.
    #[test]
    fn test_parse_client_first() {
        let msg = "n,,n=user,r=fyko+d2lbbFgONRv9qkxdawL";
        let parsed = parse_client_first(msg).unwrap();

        assert_eq!(parsed.gs2_cbind_flag, 'n');
        assert!(parsed.authzid.is_none());
        assert_eq!(parsed.username, "user");
        assert_eq!(parsed.client_nonce, "fyko+d2lbbFgONRv9qkxdawL");
        assert_eq!(parsed.bare, "n=user,r=fyko+d2lbbFgONRv9qkxdawL");
    }

    /// Test parsing client-first-message with authzid.
    #[test]
    fn test_parse_client_first_with_authzid() {
        let msg = "n,a=admin,n=user,r=nonce123";
        let parsed = parse_client_first(msg).unwrap();

        assert_eq!(parsed.gs2_cbind_flag, 'n');
        assert_eq!(parsed.authzid, Some("admin".to_string()));
        assert_eq!(parsed.username, "user");
        assert_eq!(parsed.client_nonce, "nonce123");
    }

    /// Test parsing client-final-message.
    #[test]
    fn test_parse_client_final() {
        let msg = "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=";
        let parsed = parse_client_final(msg).unwrap();

        assert_eq!(parsed.channel_binding, "biws");
        assert_eq!(
            parsed.nonce,
            "fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j"
        );
        assert_eq!(parsed.proof, "v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=");
        assert_eq!(
            parsed.without_proof,
            "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j"
        );
    }

    /// Test SASL name encoding/decoding.
    #[test]
    fn test_sasl_name_encoding() {
        assert_eq!(encode_sasl_name("user"), "user");
        assert_eq!(encode_sasl_name("user,name"), "user=2Cname");
        assert_eq!(encode_sasl_name("user=name"), "user=3Dname");
        assert_eq!(encode_sasl_name("a,b=c"), "a=2Cb=3Dc");
    }

    #[test]
    fn test_sasl_name_decoding() {
        assert_eq!(decode_sasl_name("user").unwrap(), "user");
        assert_eq!(decode_sasl_name("user=2Cname").unwrap(), "user,name");
        assert_eq!(decode_sasl_name("user=3Dname").unwrap(), "user=name");
        assert_eq!(decode_sasl_name("a=2Cb=3Dc").unwrap(), "a,b=c");
    }

    /// Test invalid SCRAM state transitions.
    #[test]
    fn test_invalid_state_transitions() {
        let mut server = ScramServer::new();

        // Can't process client-final before client-first
        let result = server.process_client_final("c=biws,r=nonce,p=proof", &[], &[]);
        assert!(result.is_err());

        // Process client-first to advance state
        let _ = server.process_client_first("n,,n=user,r=nonce123");

        // Can't process client-first again
        let mut server2 = server.clone();
        let result = server2.process_client_first("n,,n=user2,r=nonce456");
        assert!(result.is_err());
    }

    /// Test RFC 5802 test vector (adapted for SHA-256).
    /// Note: RFC 5802 uses SHA-1, but we test the same structure with SHA-256.
    #[test]
    fn test_rfc_structure() {
        // This tests that our implementation follows the RFC structure
        let password = "pencil";
        let salt = BASE64_STANDARD.decode("QSXCR+Q6sek8bf92").unwrap();
        let iterations = 4096;

        let (stored_key, server_key) = generate_scram_keys(password, &salt, iterations);

        // StoredKey and ServerKey should be 32 bytes for SHA-256
        assert_eq!(stored_key.len(), 32);
        assert_eq!(server_key.len(), 32);

        // Verify keys are deterministic
        let (stored_key2, server_key2) = generate_scram_keys(password, &salt, iterations);
        assert_eq!(stored_key, stored_key2);
        assert_eq!(server_key, server_key2);
    }

    /// Test nonce generation uniqueness.
    #[test]
    fn test_nonce_uniqueness() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();
        let nonce3 = generate_nonce();

        assert_ne!(nonce1, nonce2);
        assert_ne!(nonce2, nonce3);
        assert_ne!(nonce1, nonce3);

        // Nonces should be base64 encoded
        assert!(BASE64_STANDARD.decode(&nonce1).is_ok());
    }

    /// Test salt generation.
    #[test]
    fn test_salt_generation() {
        let salt1 = generate_salt();
        let salt2 = generate_salt();

        assert_eq!(salt1.len(), 16);
        assert_eq!(salt2.len(), 16);
        assert_ne!(salt1, salt2);
    }

    /// Test authentication failure with wrong password.
    #[test]
    fn test_wrong_password() {
        let correct_password = "correct-password";
        let wrong_password = "wrong-password";
        let salt = generate_salt();
        let iterations = 4096;

        // Generate keys for correct password
        let (stored_key, server_key) = generate_scram_keys(correct_password, &salt, iterations);

        // Generate keys for wrong password (simulating what client would compute)
        let (_, _) = generate_scram_keys(wrong_password, &salt, iterations);

        // Start SCRAM exchange
        let mut server = ScramServer::with_salt(salt.clone(), iterations);
        let client_nonce = "test-nonce";
        let client_first = format!("n,,n=user,r={}", client_nonce);
        let server_first = server.process_client_first(&client_first).unwrap();

        // Client computes proof with WRONG password
        let wrong_salted_password = hi(wrong_password.as_bytes(), &salt, iterations);
        let wrong_client_key = hmac_sha256(&wrong_salted_password, b"Client Key");
        let wrong_stored_key = sha256(&wrong_client_key);

        let channel_binding = BASE64_STANDARD.encode("n,,");
        let client_final_without_proof = format!("c={},r={}", channel_binding, server.combined_nonce);
        let auth_message = format!(
            "n=user,r={},{},{}",
            client_nonce,
            server_first.message,
            client_final_without_proof
        );
        let wrong_client_signature = hmac_sha256(&wrong_stored_key, auth_message.as_bytes());

        let wrong_client_proof: Vec<u8> = wrong_client_key
            .iter()
            .zip(wrong_client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();

        let wrong_proof_b64 = BASE64_STANDARD.encode(&wrong_client_proof);
        let client_final = format!("{},p={}", client_final_without_proof, wrong_proof_b64);

        // Server should reject
        let result = server.process_client_final(&client_final, &stored_key, &server_key);
        assert!(result.is_err());
    }
}
