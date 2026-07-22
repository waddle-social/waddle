use super::*;

pub(crate) fn encrypted_file_to_js(
    enc: waddle_xmpp_client::xep::encrypted_file::EncryptedFile,
) -> WaddleEncryptedFile {
    WaddleEncryptedFile {
        cipher: enc.cipher.as_uri().to_string(),
        key_b64: enc.key_b64,
        iv_b64: enc.iv_b64,
        hashes: enc
            .hashes
            .into_iter()
            .map(|h| WaddleEncryptedFileHash {
                algo: h.algo,
                value_b64: h.value_b64,
            })
            .collect(),
        sources: enc.sources,
    }
}

/// Reasons `encrypted_file_from_js` may reject a payload from the JS side.
/// Carried as a typed error so native unit tests do not have to construct
/// `JsValue`s (wasm-bindgen panics on non-wasm32 targets); the WASM caller
/// converts via `Display` at the boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EncryptedFileFromJsError {
    #[error("encrypted attachment has unknown cipher: {0}")]
    UnknownCipher(String),
    #[error(
        "encrypted attachment has no sources; recipients would receive ciphertext with no decryption metadata"
    )]
    NoSources,
}

pub(crate) fn encrypted_file_from_js(
    enc: WaddleEncryptedFile,
) -> Result<waddle_xmpp_client::xep::encrypted_file::EncryptedFile, EncryptedFileFromJsError> {
    use waddle_xmpp_client::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash};
    let cipher = Cipher::from_uri(&enc.cipher)
        .ok_or_else(|| EncryptedFileFromJsError::UnknownCipher(enc.cipher.clone()))?;
    if enc.sources.is_empty() {
        return Err(EncryptedFileFromJsError::NoSources);
    }
    Ok(EncryptedFile {
        cipher,
        key_b64: enc.key_b64,
        iv_b64: enc.iv_b64,
        hashes: enc
            .hashes
            .into_iter()
            .map(|h| EncryptedHash {
                algo: h.algo,
                value_b64: h.value_b64,
            })
            .collect(),
        sources: enc.sources,
    })
}

/// Convert the file-level plaintext hashes from the JS boundary into
/// typed XEP-0300 values. Unknown algorithms are refused rather than
/// smuggled through as strings; native-testable like the envelope path.
pub(crate) fn file_hashes_from_js(
    hashes: Vec<WaddleEncryptedFileHash>,
) -> Result<Vec<waddle_xmpp_client::stickers::StickerHash>, String> {
    use waddle_xmpp_client::stickers::{HashAlgo, StickerHash};
    hashes
        .into_iter()
        .map(|hash| {
            let algo = HashAlgo::from_attr(&hash.algo)
                .ok_or_else(|| format!("unsupported hash algorithm '{}'", hash.algo))?;
            Ok(StickerHash {
                algo,
                value_b64: hash.value_b64,
            })
        })
        .collect()
}

#[cfg(test)]
mod encrypted_file_bridge_tests {
    use super::*;
    use waddle_xmpp_client::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash};

    fn sample_native() -> EncryptedFile {
        EncryptedFile {
            cipher: Cipher::Aes256GcmNoPadding,
            key_b64: "a2V5".to_string(),
            iv_b64: "aXY=".to_string(),
            hashes: vec![EncryptedHash {
                algo: "sha-256".to_string(),
                value_b64: "aGFzaA==".to_string(),
            }],
            sources: vec!["https://files.example.com/blob.enc".to_string()],
        }
    }

    #[test]
    fn round_trips_encrypted_file_through_js_bridge() {
        let native = sample_native();
        let js = encrypted_file_to_js(native.clone());
        assert_eq!(js.cipher, "urn:xmpp:ciphers:aes-256-gcm-nopadding:0");
        assert_eq!(js.sources, native.sources);
        let back = encrypted_file_from_js(js).expect("known cipher round-trips");
        assert_eq!(back, native);
    }

    #[test]
    fn rejects_unknown_cipher_at_js_boundary() {
        let invalid = WaddleEncryptedFile {
            cipher: "urn:xmpp:ciphers:rot13:0".to_string(),
            key_b64: "k".to_string(),
            iv_b64: "v".to_string(),
            hashes: Vec::new(),
            sources: vec!["https://x".to_string()],
        };
        assert!(encrypted_file_from_js(invalid).is_err());
    }

    #[test]
    fn rejects_empty_sources_at_js_boundary() {
        let invalid = WaddleEncryptedFile {
            cipher: "urn:xmpp:ciphers:aes-256-gcm-nopadding:0".to_string(),
            key_b64: "k".to_string(),
            iv_b64: "v".to_string(),
            hashes: Vec::new(),
            sources: Vec::new(),
        };
        assert!(encrypted_file_from_js(invalid).is_err());
    }

    #[test]
    fn converts_plaintext_file_hashes_into_typed_values() {
        use waddle_xmpp_client::stickers::HashAlgo;
        let hashes = file_hashes_from_js(vec![WaddleEncryptedFileHash {
            algo: "sha-256".to_string(),
            value_b64: "cGxhaW4taGFzaA==".to_string(),
        }])
        .expect("sha-256 converts");
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].algo, HashAlgo::Sha256);
        assert_eq!(hashes[0].value_b64, "cGxhaW4taGFzaA==");
    }

    #[test]
    fn refuses_unknown_plaintext_hash_algorithms() {
        let err = file_hashes_from_js(vec![WaddleEncryptedFileHash {
            algo: "crc32".to_string(),
            value_b64: "AAAA".to_string(),
        }])
        .expect_err("unknown algo refused");
        assert!(err.contains("crc32"));
    }
}
