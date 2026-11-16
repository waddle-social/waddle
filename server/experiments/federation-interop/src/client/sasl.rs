//! SASL client mechanisms: PLAIN (RFC 4616), used only inside TLS, and
//! SCRAM-SHA-256 (RFC 7677) without channel binding.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// The initial response of PLAIN, base64-encoded.
pub fn plain(username: &str, password: &str) -> String {
    let mut message = Vec::with_capacity(username.len() + password.len() + 2);
    message.push(0);
    message.extend_from_slice(username.as_bytes());
    message.push(0);
    message.extend_from_slice(password.as_bytes());
    STANDARD.encode(message)
}

/// A SCRAM-SHA-256 exchange in progress.
pub struct Scram {
    client_first_bare: String,
    password: String,
}

impl Scram {
    pub fn new(username: &str, password: &str) -> Self {
        let nonce = STANDARD.encode(uuid::Uuid::new_v4().as_bytes());
        Self {
            client_first_bare: ["n=", username, ",r=", &nonce].concat(),
            password: password.to_owned(),
        }
    }

    /// The client-first message, base64-encoded.
    pub fn first(&self) -> String {
        STANDARD.encode(["n,,", &self.client_first_bare].concat())
    }

    /// The client-final message for the server's base64 challenge.
    pub fn respond(&self, challenge: &str) -> Result<String> {
        let server_first = String::from_utf8(STANDARD.decode(challenge.trim())?)?;
        let fields: BTreeMap<&str, &str> = server_first
            .split(',')
            .filter_map(|field| field.split_once('='))
            .collect();
        let nonce = fields.get("r").context("SCRAM nonce")?;
        let salt = STANDARD.decode(fields.get("s").context("SCRAM salt")?)?;
        let iterations: u32 = fields.get("i").context("SCRAM iterations")?.parse()?;
        let salted = hi(self.password.as_bytes(), &salt, iterations)?;
        let client_key = hmac(&salted, b"Client Key")?;
        let stored_key = Sha256::digest(&client_key);
        let without_proof = ["c=", &STANDARD.encode("n,,"), ",r=", nonce].concat();
        let auth_message = [
            self.client_first_bare.as_str(),
            server_first.as_str(),
            without_proof.as_str(),
        ]
        .join(",");
        let signature = hmac(&stored_key, auth_message.as_bytes())?;
        let proof: Vec<u8> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(key, signature)| key ^ signature)
            .collect();
        Ok(STANDARD.encode([without_proof.as_str(), ",p=", &STANDARD.encode(proof)].concat()))
    }
}

fn hmac(key: &[u8], message: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// PBKDF2-HMAC-SHA-256 with one output block, which is SCRAM's `Hi`.
fn hi(password: &[u8], salt: &[u8], iterations: u32) -> Result<Vec<u8>> {
    let mut first = salt.to_vec();
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut block = hmac(password, &first)?;
    let mut result = block.clone();
    for _ in 1..iterations {
        block = hmac(password, &block)?;
        for (output, byte) in result.iter_mut().zip(block.iter()) {
            *output ^= byte;
        }
    }
    Ok(result)
}
