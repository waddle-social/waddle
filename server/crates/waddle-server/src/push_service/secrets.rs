//! Sealing cipher for Push Service provider credentials at rest.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use waddle_xmpp::XmppError;
use zeroize::ZeroizeOnDrop;

type HmacSha256 = Hmac<Sha256>;

pub(super) const SEALED_PROVIDER_VALUE_PREFIX: &str = "waddle-push-secret-v1";

/// Sealing cipher for device provider credentials at rest. Keys are
/// HMAC-SHA256 derivations of the boot-time root key and are
/// zeroized on drop so a process panic / shutdown does not leave the
/// HMAC root-derived keys in heap residue. Wrap in `Arc` for cheap
/// sharing across the publish-job fan-out; do not `.clone()` directly
/// — that would duplicate the unzeroized byte buffers.
#[derive(ZeroizeOnDrop)]
pub(crate) struct PushSecretCipher {
    enc_key: Vec<u8>,
    mac_key: Vec<u8>,
}

impl PushSecretCipher {
    pub(super) fn new(root_key: &[u8]) -> Self {
        Self {
            enc_key: derive_secret_key(root_key, b"waddle:push-service:provider-secret:enc:v1"),
            mac_key: derive_secret_key(root_key, b"waddle:push-service:provider-secret:mac:v1"),
        }
    }

    pub(super) fn seal_optional(&self, value: Option<String>) -> Result<Option<String>, XmppError> {
        value.map(|value| self.seal(value.as_bytes())).transpose()
    }

    pub(super) fn open_optional(&self, value: Option<String>) -> Result<Option<String>, XmppError> {
        value.map(|value| self.open(&value)).transpose()
    }

    pub(super) fn seal(&self, plaintext: &[u8]) -> Result<String, XmppError> {
        let nonce: [u8; 16] = rand::random();
        let ciphertext = xor_keystream(&self.enc_key, &nonce, plaintext);
        let tag = provider_secret_tag(&self.mac_key, &nonce, &ciphertext);
        Ok(format!(
            "{SEALED_PROVIDER_VALUE_PREFIX}:{}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext),
            URL_SAFE_NO_PAD.encode(tag)
        ))
    }

    pub(super) fn open(&self, stored: &str) -> Result<String, XmppError> {
        let mut parts = stored.split(':');
        let prefix = parts.next();
        let nonce = parts.next();
        let ciphertext = parts.next();
        let tag = parts.next();
        if prefix != Some(SEALED_PROVIDER_VALUE_PREFIX) || parts.next().is_some() {
            return Err(XmppError::internal(
                "Push Service provider secret is not sealed",
            ));
        }
        let nonce = nonce
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing nonce"))
            .and_then(decode_sealed_part)?;
        let ciphertext = ciphertext
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing ciphertext"))
            .and_then(decode_sealed_part)?;
        let tag = tag
            .ok_or_else(|| XmppError::internal("Push Service provider secret missing tag"))
            .and_then(decode_sealed_part)?;
        let nonce: [u8; 16] = nonce.try_into().map_err(|_| {
            XmppError::internal("Push Service provider secret nonce has invalid length")
        })?;
        let expected = provider_secret_tag(&self.mac_key, &nonce, &ciphertext);
        if !constant_time_eq(&expected, &tag) {
            return Err(XmppError::internal(
                "Push Service provider secret authentication failed",
            ));
        }
        let plaintext = xor_keystream(&self.enc_key, &nonce, &ciphertext);
        String::from_utf8(plaintext).map_err(|error| {
            XmppError::internal(format!(
                "Push Service provider secret is invalid UTF-8: {error}"
            ))
        })
    }
}

fn derive_secret_key(root_key: &[u8], label: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(root_key).expect("HMAC supports any key length");
    mac.update(label);
    mac.finalize().into_bytes().to_vec()
}

fn xor_keystream(key: &[u8], nonce: &[u8; 16], input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut counter = 0_u64;
    while output.len() < input.len() {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key length");
        mac.update(nonce);
        mac.update(&counter.to_be_bytes());
        let block = mac.finalize().into_bytes();
        for byte in block {
            if output.len() == input.len() {
                break;
            }
            output.push(input[output.len()] ^ byte);
        }
        counter += 1;
    }
    output
}

fn provider_secret_tag(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key length");
    mac.update(SEALED_PROVIDER_VALUE_PREFIX.as_bytes());
    mac.update(nonce);
    mac.update(ciphertext);
    mac.finalize().into_bytes().to_vec()
}

fn decode_sealed_part(part: &str) -> Result<Vec<u8>, XmppError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|error| XmppError::internal(format!("Invalid sealed provider secret: {error}")))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}
