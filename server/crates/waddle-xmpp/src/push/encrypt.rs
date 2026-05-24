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
    AES128GCM_HEADER_LEN, AES128GCM_KEY_INFO, AES128GCM_LAST_RECORD_DELIM, AES128GCM_NONCE_INFO,
    AES128GCM_PAD_DELIMITER_LEN, AES128GCM_SALT_LEN, AES128GCM_TAG_LEN, HKDF_CEK_LEN,
    HKDF_NONCE_LEN, HKDF_PRK_LEN, P256_UNCOMPRESSED_POINT_LEN, WEBPUSH_INFO_PREFIX,
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
    debug_assert_eq!(as_public_bytes.len(), P256_UNCOMPRESSED_POINT_LEN);

    let ua_public_uncompressed = subscription.p256dh.to_encoded_point(false);
    let ua_public_bytes = ua_public_uncompressed.as_bytes();
    debug_assert_eq!(ua_public_bytes.len(), P256_UNCOMPRESSED_POINT_LEN);

    // 2. ECDH(as_secret, ua_public). `SharedSecret` is zeroized on drop.
    let shared = as_secret.diffie_hellman(&subscription.p256dh);
    let dh = shared.raw_secret_bytes();

    // 3. PRK_key = HKDF-Extract(salt=auth_secret, ikm=dh)
    //    IKM = HKDF-Expand(PRK_key, "WebPush: info\0" || ua_public || as_public, HKDF_PRK_LEN)
    let prk_key = Hkdf::<Sha256>::new(Some(subscription.auth.as_bytes()), dh);
    let mut key_info =
        Vec::with_capacity(WEBPUSH_INFO_PREFIX.len() + 2 * P256_UNCOMPRESSED_POINT_LEN);
    key_info.extend_from_slice(WEBPUSH_INFO_PREFIX);
    key_info.extend_from_slice(ua_public_bytes);
    key_info.extend_from_slice(as_public_bytes);
    let mut ikm = Zeroizing::new([0u8; HKDF_PRK_LEN]);
    prk_key
        .expand(&key_info, ikm.as_mut())
        .map_err(|_| WebPushCryptoError::HkdfFailed)?;

    // 4. Random salt (RFC 8188 §2.1: 16 bytes).
    let mut salt = [0u8; AES128GCM_SALT_LEN];
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
    //
    //    Wrapped in `Zeroizing` so the cleartext push body (chat message,
    //    sender JID, etc.) zeroes out of heap when this scope ends. The
    //    `with_capacity` matches the final length exactly, so no
    //    reallocation occurs and no un-zeroized intermediate buffer is
    //    freed to the allocator.
    let mut record_plaintext = Zeroizing::new(Vec::with_capacity(
        bucket_size + AES128GCM_PAD_DELIMITER_LEN,
    ));
    record_plaintext.extend_from_slice(plaintext);
    record_plaintext.push(AES128GCM_LAST_RECORD_DELIM);
    record_plaintext.resize(bucket_size + AES128GCM_PAD_DELIMITER_LEN, 0x00);

    // 9. Encrypt: ciphertext = AES-128-GCM(key=CEK, nonce=nonce, plaintext+pad)
    let cipher =
        Aes128Gcm::new_from_slice(cek.as_ref()).map_err(|_| WebPushCryptoError::EncryptFailed)?;
    let ciphertext = cipher
        .encrypt(
            (&nonce_bytes).into(),
            Payload {
                msg: record_plaintext.as_slice(),
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

// `header_rs` / `header_keyid` parsing helpers are intentionally NOT in
// the public API surface — they're test-only and would otherwise become
// implicit boundary types. Defined inside the inline `#[cfg(test)]` mod
// below; tests that need them in other crates can re-implement the
// parsing trivially against the named header-offset constants.

#[cfg(test)]
mod tests {
    use super::super::constants::{
        AES128GCM_IDLEN_FIELD_LEN, AES128GCM_RS_LEN, AES128GCM_TAG_LEN, DEFAULT_PLAINTEXT_BUCKET,
        DM_PLAINTEXT_BUCKET, WEB_PUSH_MAX_BODY_LEN,
    };
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    fn header_rs(body: &[u8]) -> Option<u32> {
        use super::super::constants::AES128GCM_RS_LEN;
        if body.len() < AES128GCM_HEADER_LEN {
            return None;
        }
        let rs_start = AES128GCM_SALT_LEN;
        let rs_end = rs_start + AES128GCM_RS_LEN;
        let mut rs_bytes = [0u8; AES128GCM_RS_LEN];
        rs_bytes.copy_from_slice(&body[rs_start..rs_end]);
        Some(u32::from_be_bytes(rs_bytes))
    }

    fn header_keyid(body: &[u8]) -> Option<&[u8]> {
        use super::super::constants::{AES128GCM_IDLEN_FIELD_LEN, AES128GCM_RS_LEN};
        if body.len() < AES128GCM_HEADER_LEN {
            return None;
        }
        let idlen_offset = AES128GCM_SALT_LEN + AES128GCM_RS_LEN;
        let keyid_offset = idlen_offset + AES128GCM_IDLEN_FIELD_LEN;
        let idlen = body[idlen_offset] as usize;
        if body.len() < keyid_offset + idlen {
            return None;
        }
        Some(&body[keyid_offset..keyid_offset + idlen])
    }

    /// RFC 8291 §5 full known-answer test: the spec's worked example pins
    /// every input (UA private/public, AS private/public, auth secret,
    /// salt, plaintext) AND publishes the expected on-the-wire body
    /// bytes. Asserts the entire deterministic pipeline end-to-end:
    ///
    ///   1. HKDF derivation produces the RFC's `IKM`, `CEK`, `nonce`.
    ///   2. AES-128-GCM encrypt with the RFC's padded plaintext produces
    ///      a ciphertext matching the RFC's published body, byte-for-byte.
    ///   3. Header assembly (`salt || rs || idlen || as_public`) +
    ///      ciphertext reproduces the full RFC body verbatim.
    ///
    /// Catches the entire class of derivation/serialization bugs that an
    /// in-process round-trip cannot detect — e.g., `ua_public` /
    /// `as_public` swapped in `key_info`, off-by-one padding, wrong
    /// AES-GCM nonce or info string, or a header-field order mismatch.
    ///
    /// `encrypt()` itself generates a fresh AS keypair and salt on every
    /// call, so this KAT reaches into the same primitives directly with
    /// the RFC-fixed inputs. The randomness paths are covered by
    /// `salt_changes_per_call` / `ephemeral_keypair_changes_per_call`.
    #[test]
    fn rfc_8291_section_5_known_answer_vector() {
        use super::super::constants::AES128GCM_RS_LEN;
        use p256::elliptic_curve::sec1::FromEncodedPoint;
        use p256::EncodedPoint;
        use sha2::Sha256;

        // ---- RFC 8291 §5 inputs ------------------------------------------
        let ua_public_b64 = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth_b64 = "BTBZMqHH6r4Tts7J_aSIgg";
        let as_private_b64 = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
        let as_public_b64 = "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
        let salt_b64 = "DGv6ra1nlYgDCS1FRnbzlw";
        let plaintext = b"When I grow up, I want to be a watermelon";
        // RFC 8291 §5 expected encrypted body — concatenation of the three
        // lines as published in the RFC text.
        let expected_body_b64 = concat!(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml",
            "mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT",
            "pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
        );

        let ua_public_bytes = URL_SAFE_NO_PAD.decode(ua_public_b64).unwrap();
        let ua_pub_point = EncodedPoint::from_bytes(&ua_public_bytes).unwrap();
        let ua_public = p256::PublicKey::from_encoded_point(&ua_pub_point).unwrap();
        let auth = URL_SAFE_NO_PAD.decode(auth_b64).unwrap();
        let as_private_scalar = URL_SAFE_NO_PAD.decode(as_private_b64).unwrap();
        let as_private = p256::SecretKey::from_slice(&as_private_scalar).unwrap();
        let as_public_bytes = URL_SAFE_NO_PAD.decode(as_public_b64).unwrap();
        let salt = URL_SAFE_NO_PAD.decode(salt_b64).unwrap();
        let expected_body = URL_SAFE_NO_PAD.decode(expected_body_b64).unwrap();

        // ---- HKDF derivation ---------------------------------------------
        let shared = elliptic_curve::ecdh::diffie_hellman(
            as_private.to_nonzero_scalar(),
            ua_public.as_affine(),
        );
        let dh = shared.raw_secret_bytes();

        let prk_key = Hkdf::<Sha256>::new(Some(&auth), dh);
        let mut key_info =
            Vec::with_capacity(WEBPUSH_INFO_PREFIX.len() + 2 * P256_UNCOMPRESSED_POINT_LEN);
        key_info.extend_from_slice(WEBPUSH_INFO_PREFIX);
        key_info.extend_from_slice(&ua_public_bytes);
        key_info.extend_from_slice(&as_public_bytes);
        let mut ikm = [0u8; HKDF_PRK_LEN];
        prk_key.expand(&key_info, &mut ikm).unwrap();

        let prk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
        let mut cek = [0u8; HKDF_CEK_LEN];
        prk.expand(AES128GCM_KEY_INFO, &mut cek).unwrap();
        let mut nonce = [0u8; HKDF_NONCE_LEN];
        prk.expand(AES128GCM_NONCE_INFO, &mut nonce).unwrap();

        // RFC 8291 §5 published CEK and nonce (base64url no-pad). The RFC
        // example also lists an `IKM` value but in a 16-byte truncated form
        // (the full HKDF-Expand output is 32 bytes per RFC 8291 §3.4);
        // CEK and nonce are exact and transitively cover any IKM bug.
        let expected_cek = URL_SAFE_NO_PAD.decode("oIhVW04MRdy2XN9CiKLxTg").unwrap();
        let expected_nonce = URL_SAFE_NO_PAD.decode("4h_95klXJ5E_qnoN").unwrap();
        assert_eq!(&cek[..], &expected_cek[..], "CEK mismatch");
        assert_eq!(&nonce[..], &expected_nonce[..], "nonce mismatch");

        // ---- Pad to the RFC's actual record length, AES-GCM seal --------
        // RFC 8188 §2.1: `rs` in the header is the configured per-record
        // *maximum* (the RFC example uses 4096); the last record may be
        // shorter. Derive the actual padded-plaintext length from the
        // RFC's published body so we reproduce the reference encoder's
        // padding choice exactly:
        //     padded_plaintext_len = body.len() − header_len − tag_len.
        let record_plaintext_len = expected_body.len() - AES128GCM_HEADER_LEN - AES128GCM_TAG_LEN;
        let mut padded = Vec::with_capacity(record_plaintext_len);
        padded.extend_from_slice(plaintext);
        padded.push(AES128GCM_LAST_RECORD_DELIM);
        padded.resize(record_plaintext_len, 0x00);

        let cipher = Aes128Gcm::new_from_slice(&cek).unwrap();
        let ciphertext = cipher
            .encrypt((&nonce).into(), padded.as_slice())
            .expect("AES-128-GCM encrypt");

        // Read the RFC's `rs` field for header reassembly (4096 in the
        // example — this is the configured max, not the actual record
        // length).
        let rs_field_start = AES128GCM_SALT_LEN;
        let rs_field_end = rs_field_start + AES128GCM_RS_LEN;
        let mut rs_bytes = [0u8; AES128GCM_RS_LEN];
        rs_bytes.copy_from_slice(&expected_body[rs_field_start..rs_field_end]);

        // ---- Assemble header || ciphertext, byte-equal the RFC body -----
        let mut body = Vec::with_capacity(AES128GCM_HEADER_LEN + ciphertext.len());
        body.extend_from_slice(&salt);
        body.extend_from_slice(&rs_bytes);
        body.push(as_public_bytes.len() as u8);
        body.extend_from_slice(&as_public_bytes);
        body.extend_from_slice(&ciphertext);
        assert_eq!(
            body, expected_body,
            "encrypted body must match RFC 8291 §5 expected output byte-for-byte"
        );
    }

    /// UA-side RFC 8291 decryption — used to assert that the real
    /// `encrypt()` function produces output a conformant receiver
    /// could actually decrypt. A bug in `encrypt()` itself (wrong
    /// `key_info` order, wrong AES-GCM info string, swapped public-
    /// key bytes, off-by-one padding) would surface as a decrypt
    /// failure here. The earlier `rfc_8291_section_5_known_answer_vector`
    /// test reimplements the pipeline inline; this one drives the
    /// production code path end-to-end.
    fn ua_side_decrypt(
        ua_private: &p256::SecretKey,
        auth_secret: &[u8; 16],
        body: &[u8],
    ) -> Vec<u8> {
        use p256::elliptic_curve::sec1::FromEncodedPoint;
        use p256::EncodedPoint;
        use sha2::Sha256;
        assert!(
            body.len() > AES128GCM_HEADER_LEN + AES128GCM_TAG_LEN,
            "body too short to be a valid aes128gcm record"
        );
        let salt = &body[..AES128GCM_SALT_LEN];
        let idlen = body[AES128GCM_SALT_LEN + AES128GCM_RS_LEN] as usize;
        assert_eq!(idlen, P256_UNCOMPRESSED_POINT_LEN, "idlen must be 65");
        let keyid_start =
            AES128GCM_SALT_LEN + AES128GCM_RS_LEN + AES128GCM_IDLEN_FIELD_LEN;
        let as_public_bytes = &body[keyid_start..keyid_start + idlen];
        let ciphertext = &body[AES128GCM_HEADER_LEN..];
        let as_pub_point = EncodedPoint::from_bytes(as_public_bytes).expect("as public");
        let as_public = p256::PublicKey::from_encoded_point(&as_pub_point)
            .expect("as public valid");

        let shared = elliptic_curve::ecdh::diffie_hellman(
            ua_private.to_nonzero_scalar(),
            as_public.as_affine(),
        );
        let dh = shared.raw_secret_bytes();

        let ua_public_encoded = ua_private.public_key().to_encoded_point(false);
        let ua_public_bytes = ua_public_encoded.as_bytes();
        let prk_key = Hkdf::<Sha256>::new(Some(auth_secret), dh);
        let mut key_info =
            Vec::with_capacity(WEBPUSH_INFO_PREFIX.len() + 2 * P256_UNCOMPRESSED_POINT_LEN);
        key_info.extend_from_slice(WEBPUSH_INFO_PREFIX);
        key_info.extend_from_slice(ua_public_bytes);
        key_info.extend_from_slice(as_public_bytes);
        let mut ikm = [0u8; HKDF_PRK_LEN];
        prk_key.expand(&key_info, &mut ikm).expect("prk_key expand");

        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let mut cek = [0u8; HKDF_CEK_LEN];
        prk.expand(AES128GCM_KEY_INFO, &mut cek).expect("cek");
        let mut nonce = [0u8; HKDF_NONCE_LEN];
        prk.expand(AES128GCM_NONCE_INFO, &mut nonce).expect("nonce");

        let cipher = Aes128Gcm::new_from_slice(&cek).expect("cipher");
        let padded = cipher
            .decrypt((&nonce).into(), ciphertext)
            .expect("decrypt");

        // Strip RFC 8188 §2 padding: `plaintext || 0x02 || 0x00*` for
        // the last record.
        let mut end = padded.len();
        while end > 0 && padded[end - 1] == 0 {
            end -= 1;
        }
        assert!(end > 0, "padding had no delimiter");
        assert_eq!(
            padded[end - 1],
            AES128GCM_LAST_RECORD_DELIM,
            "expected last-record delimiter 0x02 before trailing zeros"
        );
        padded[..end - 1].to_vec()
    }

    /// Drive the production `encrypt()` function with the RFC 8291 §5
    /// UA keypair and assert the output is decryptable back to the
    /// original plaintext. Catches any future bug in `encrypt()` that
    /// would silently ship an undecryptable body (wrong info-string
    /// order, swapped public-key bytes, etc.) — the inline KAT above
    /// only validates the inline reimplementation.
    #[test]
    fn encrypt_output_decrypts_under_rfc_8291_section_5_ua_keys() {
        use p256::elliptic_curve::sec1::FromEncodedPoint;
        use p256::EncodedPoint;
        let ua_private_b64 = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
        let ua_public_b64 = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth_b64 = "BTBZMqHH6r4Tts7J_aSIgg";

        let ua_private_bytes = URL_SAFE_NO_PAD.decode(ua_private_b64).unwrap();
        let ua_private = p256::SecretKey::from_slice(&ua_private_bytes).expect("ua private");
        let ua_public_bytes = URL_SAFE_NO_PAD.decode(ua_public_b64).unwrap();
        let ua_pub_point = EncodedPoint::from_bytes(&ua_public_bytes).unwrap();
        let ua_public = p256::PublicKey::from_encoded_point(&ua_pub_point).unwrap();
        let auth_bytes = URL_SAFE_NO_PAD.decode(auth_b64).unwrap();
        let auth: [u8; 16] = auth_bytes.try_into().expect("16-byte auth");

        let subscription = SubscriptionKeys {
            p256dh: ua_public,
            auth: std::sync::Arc::new(crate::push::types::AuthSecret::from_bytes(auth)),
        };
        let plaintext = b"When I grow up, I want to be a watermelon";
        let encrypted = encrypt(&subscription, plaintext, DEFAULT_PLAINTEXT_BUCKET)
            .expect("encrypt() succeeds with the RFC 8291 §5 UA key");
        let decrypted = ua_side_decrypt(&ua_private, &auth, encrypted.as_slice());
        assert_eq!(
            decrypted, plaintext,
            "encrypt() output must decrypt back to original plaintext under the UA private key — \
             a regression here means encrypt() is producing undecryptable bodies"
        );
    }

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
