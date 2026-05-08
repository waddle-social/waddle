use base64::prelude::*;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Length of generated nonce in bytes (will be base64 encoded).
const NONCE_LENGTH: usize = 24;

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
    rand::random::<[u8; 16]>().to_vec()
}

/// Generate a random nonce string.
pub(super) fn generate_nonce() -> String {
    let mut nonce_bytes = vec![0u8; NONCE_LENGTH];
    rand::rng().fill(&mut nonce_bytes[..]);
    BASE64_STANDARD.encode(&nonce_bytes)
}

/// Hi() function from RFC 5802 - PBKDF2-HMAC-SHA256.
pub(super) fn hi(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut output = vec![0u8; 32]; // SHA-256 output is 32 bytes
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut output);
    output
}

/// HMAC-SHA-256 helper.
pub(super) fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// SHA-256 hash helper.
pub(super) fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}
