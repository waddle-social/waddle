//! RFC 8291 `aes128gcm` content encoding for Web Push.
//!
//! Hand-rolled on workspace primitives (`aes-gcm`, `hkdf`, `p256`,
//! `sha2`). All wire constants come from [`super::constants`]; inline
//! literals are forbidden.
//!
//! ## Invariants
//!
//! - **Nonce** MUST be HKDF-derived per RFC 8188 §2.2 (`info =
//!   "Content-Encoding: nonce\0"`), never random.
//! - **Ephemeral ECDH keypair** is fresh per `encrypt()` call, never
//!   stored, zeroized on drop.
//! - **Padding** is `plaintext || 0x02 || 0x00*` inside the single
//!   AES-GCM record, before encryption. Delimiter is `0x02` (last record).
//! - **`rs`** is the per-record max size = plaintext + padding + delimiter + AES-GCM tag.
//!   Body on wire = `AES128GCM_HEADER_LEN + actual_record_len ≤ WEB_PUSH_MAX_BODY_LEN`.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes128Gcm;
use hkdf::Hkdf;
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::RngExt;
use sha2::Sha256;
use zeroize::Zeroizing;

use super::constants::{
    AES128GCM_HEADER_LEN, AES128GCM_KEY_INFO, AES128GCM_NONCE_INFO, AES128GCM_PAD_DELIMITER_LEN,
    AES128GCM_TAG_LEN, HKDF_CEK_LEN, HKDF_NONCE_LEN, HKDF_PRK_LEN, WEBPUSH_INFO_PREFIX,
    WEB_PUSH_MAX_PLAINTEXT,
};
use super::types::{EncryptedPayload, SubscriptionKeys, WebPushCryptoError};

/// Encrypt `plaintext` for a Web Push `subscription`.
///
/// `bucket_size` is the plaintext + zero-padding length BEFORE the
/// delimiter byte is appended (so the record on the wire is
/// `bucket_size + 1 (delim) + 16 (tag)` bytes). Pass
/// [`super::constants::DM_PLAINTEXT_BUCKET`] for `DirectMessage*`,
/// [`super::constants::DEFAULT_PLAINTEXT_BUCKET`] otherwise.
///
/// `bucket_size` MUST be ≥ `plaintext.len()` and the resulting body
/// MUST NOT exceed [`super::constants::WEB_PUSH_MAX_BODY_LEN`].
pub fn encrypt(
    subscription: &SubscriptionKeys,
    plaintext: &[u8],
    bucket_size: usize,
) -> Result<EncryptedPayload, WebPushCryptoError> {
    if plaintext.len() > bucket_size {
        return Err(WebPushCryptoError::PayloadTooLarge {
            plaintext_len: plaintext.len(),
            limit: bucket_size,
        });
    }
    if bucket_size > WEB_PUSH_MAX_PLAINTEXT {
        return Err(WebPushCryptoError::PayloadTooLarge {
            plaintext_len: bucket_size,
            limit: WEB_PUSH_MAX_PLAINTEXT,
        });
    }

    // 1. Fresh ephemeral AS keypair (RFC 8291 §3.1). `EphemeralSecret` zeroes
    //    on drop; never stored, never copied.
    let as_secret = EphemeralSecret::random(&mut OsRng);
    let as_public = as_secret.public_key();
    let as_public_uncompressed = as_public.to_encoded_point(false);
    let as_public_bytes = as_public_uncompressed.as_bytes();
    debug_assert_eq!(
        as_public_bytes.len(),
        65,
        "P-256 uncompressed point must be 65 bytes"
    );

    let ua_public_uncompressed = subscription.p256dh.to_encoded_point(false);
    let ua_public_bytes = ua_public_uncompressed.as_bytes();
    debug_assert_eq!(
        ua_public_bytes.len(),
        65,
        "subscription p256dh must be 65 bytes"
    );

    // 2. ECDH(as_secret, ua_public). `SharedSecret` is zeroized on drop.
    let shared = as_secret.diffie_hellman(&subscription.p256dh);
    let dh = shared.raw_secret_bytes();

    // 3. PRK_key = HKDF-Extract(salt=auth_secret, ikm=dh)
    //    IKM = HKDF-Expand(PRK_key, "WebPush: info\0" || ua_public || as_public, HKDF_PRK_LEN)
    let prk_key = Hkdf::<Sha256>::new(Some(subscription.auth.as_bytes()), dh);
    let mut key_info = Vec::with_capacity(WEBPUSH_INFO_PREFIX.len() + 65 + 65);
    key_info.extend_from_slice(WEBPUSH_INFO_PREFIX);
    key_info.extend_from_slice(ua_public_bytes);
    key_info.extend_from_slice(as_public_bytes);
    let mut ikm = Zeroizing::new([0u8; HKDF_PRK_LEN]);
    prk_key
        .expand(&key_info, ikm.as_mut())
        .map_err(|_| WebPushCryptoError::HkdfFailed)?;

    // 4. Random 16-byte salt
    let mut salt = [0u8; 16];
    rand::rng().fill(&mut salt[..]);

    // 5. PRK = HKDF-Extract(salt=salt, ikm=IKM)
    let prk = Hkdf::<Sha256>::new(Some(&salt), ikm.as_ref());

    // 6. CEK = HKDF-Expand(PRK, "Content-Encoding: aes128gcm\0", HKDF_CEK_LEN)
    let mut cek = Zeroizing::new([0u8; HKDF_CEK_LEN]);
    prk.expand(AES128GCM_KEY_INFO, cek.as_mut())
        .map_err(|_| WebPushCryptoError::HkdfFailed)?;

    // 7. Nonce = HKDF-Expand(PRK, "Content-Encoding: nonce\0", HKDF_NONCE_LEN)
    //    Sequence number XOR is identity since we emit exactly one record (seq=0).
    let mut nonce_bytes = [0u8; HKDF_NONCE_LEN];
    prk.expand(AES128GCM_NONCE_INFO, &mut nonce_bytes)
        .map_err(|_| WebPushCryptoError::HkdfFailed)?;

    // 8. Pad plaintext per RFC 8188 §2.1: `plaintext || 0x02 || 0x00*`
    //    for the single/last record. Final length = `bucket_size + 1`.
    let mut record_plaintext = Vec::with_capacity(bucket_size + AES128GCM_PAD_DELIMITER_LEN);
    record_plaintext.extend_from_slice(plaintext);
    record_plaintext.push(0x02);
    record_plaintext.resize(bucket_size + AES128GCM_PAD_DELIMITER_LEN, 0x00);

    // 9. Encrypt: ciphertext = AES-128-GCM(key=CEK, nonce=nonce, plaintext+pad)
    let cipher =
        Aes128Gcm::new_from_slice(cek.as_ref()).map_err(|_| WebPushCryptoError::EncryptFailed)?;
    let ciphertext = cipher
        .encrypt(
            (&nonce_bytes).into(),
            Payload {
                msg: &record_plaintext,
                aad: &[],
            },
        )
        .map_err(|_| WebPushCryptoError::EncryptFailed)?;

    // 10. Assemble header: salt(16) || rs(4 BE) || idlen(1) || keyid(65)
    //     `rs` is the per-record max size (plaintext+pad+delim+tag).
    let record_len = ciphertext.len(); // = bucket_size + delim + tag
    debug_assert_eq!(
        record_len,
        bucket_size + AES128GCM_PAD_DELIMITER_LEN + AES128GCM_TAG_LEN
    );
    let rs: u32 = record_len as u32;

    let mut body = Vec::with_capacity(AES128GCM_HEADER_LEN + ciphertext.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&rs.to_be_bytes());
    body.push(as_public_bytes.len() as u8); // idlen
    body.extend_from_slice(as_public_bytes); // keyid (= as_public)
    body.extend_from_slice(&ciphertext);

    debug_assert_eq!(
        body.len(),
        AES128GCM_HEADER_LEN + record_len,
        "body shape: {} header + {} record",
        AES128GCM_HEADER_LEN,
        record_len
    );

    Ok(EncryptedPayload::new(body))
}

/// Decode the `rs` u32 field from an RFC 8188 header. Test-helper.
pub fn header_rs(body: &[u8]) -> Option<u32> {
    if body.len() < AES128GCM_HEADER_LEN {
        return None;
    }
    let mut rs_bytes = [0u8; 4];
    rs_bytes.copy_from_slice(&body[16..20]);
    Some(u32::from_be_bytes(rs_bytes))
}

/// Decode the `keyid` (= AS public key) from an RFC 8188 header. Test-helper.
pub fn header_keyid(body: &[u8]) -> Option<&[u8]> {
    if body.len() < AES128GCM_HEADER_LEN {
        return None;
    }
    let idlen = body[20] as usize;
    if body.len() < 21 + idlen {
        return None;
    }
    Some(&body[21..21 + idlen])
}

#[cfg(test)]
mod tests {
    use super::super::constants::{
        DEFAULT_PLAINTEXT_BUCKET, DM_PLAINTEXT_BUCKET, WEB_PUSH_MAX_BODY_LEN,
    };
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    fn sample_subscription() -> SubscriptionKeys {
        let secret = p256::SecretKey::random(&mut OsRng);
        let pk = secret.public_key().to_encoded_point(false);
        let p256dh = URL_SAFE_NO_PAD.encode(pk.as_bytes());
        let auth = URL_SAFE_NO_PAD.encode([0xAAu8; 16]);
        SubscriptionKeys::from_base64url(&p256dh, &auth).expect("valid")
    }

    #[test]
    fn encrypt_produces_well_formed_header() {
        let sub = sample_subscription();
        let payload = encrypt(&sub, b"hello", DEFAULT_PLAINTEXT_BUCKET).expect("encrypts");
        let body = payload.as_slice();
        assert!(body.len() > AES128GCM_HEADER_LEN);
        let rs = header_rs(body).expect("rs");
        let keyid = header_keyid(body).expect("keyid");
        assert_eq!(keyid.len(), 65);
        assert_eq!(keyid[0], 0x04);
        // rs equals the actual record length emitted = bucket + delim + tag
        assert_eq!(
            rs as usize,
            DEFAULT_PLAINTEXT_BUCKET + AES128GCM_PAD_DELIMITER_LEN + AES128GCM_TAG_LEN
        );
    }

    #[test]
    fn body_length_is_deterministic_per_bucket() {
        let sub = sample_subscription();
        let short = encrypt(&sub, b"hi", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        let long = encrypt(&sub, &[0u8; 200], DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        // Same bucket → same body length on the wire.
        assert_eq!(short.as_slice().len(), long.as_slice().len());
    }

    #[test]
    fn dm_bucket_size_is_larger_than_default() {
        let sub = sample_subscription();
        let dm = encrypt(&sub, b"x", DM_PLAINTEXT_BUCKET).expect("ok");
        let default = encrypt(&sub, b"x", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        assert!(dm.as_slice().len() > default.as_slice().len());
    }

    #[test]
    fn body_fits_under_web_push_max_body() {
        let sub = sample_subscription();
        let dm = encrypt(&sub, &vec![0u8; DM_PLAINTEXT_BUCKET], DM_PLAINTEXT_BUCKET).expect("ok");
        assert!(dm.as_slice().len() <= WEB_PUSH_MAX_BODY_LEN);
    }

    #[test]
    fn ephemeral_keypair_changes_per_call() {
        let sub = sample_subscription();
        let a = encrypt(&sub, b"same", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        let b = encrypt(&sub, b"same", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        // Same plaintext, same subscription — different ephemeral key → different keyid in header.
        let ka = header_keyid(a.as_slice()).expect("keyid a");
        let kb = header_keyid(b.as_slice()).expect("keyid b");
        assert_ne!(ka, kb, "ephemeral key must vary across encrypt() calls");
    }

    #[test]
    fn salt_changes_per_call() {
        let sub = sample_subscription();
        let a = encrypt(&sub, b"same", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        let b = encrypt(&sub, b"same", DEFAULT_PLAINTEXT_BUCKET).expect("ok");
        let salt_a = &a.as_slice()[..16];
        let salt_b = &b.as_slice()[..16];
        assert_ne!(salt_a, salt_b, "salt must vary across encrypt() calls");
    }

    #[test]
    fn rejects_plaintext_larger_than_bucket() {
        let sub = sample_subscription();
        let err = encrypt(&sub, &vec![0u8; 257], DEFAULT_PLAINTEXT_BUCKET).unwrap_err();
        assert!(matches!(err, WebPushCryptoError::PayloadTooLarge { .. }));
    }

    #[test]
    fn rejects_oversize_bucket() {
        let sub = sample_subscription();
        // Bucket larger than WEB_PUSH_MAX_PLAINTEXT is rejected even for empty plaintext.
        let err = encrypt(&sub, b"", WEB_PUSH_MAX_PLAINTEXT + 1).unwrap_err();
        assert!(matches!(err, WebPushCryptoError::PayloadTooLarge { .. }));
    }
}
