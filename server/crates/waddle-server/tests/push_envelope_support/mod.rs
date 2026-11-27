use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes128Gcm;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use sha2::Sha256;

/// RFC 8291 receiver: recover the plaintext envelope with the device's keys.
pub fn decrypt(secret: &p256::SecretKey, auth: &[u8; 16], body: &[u8]) -> Vec<u8> {
    const WEBPUSH_INFO_PREFIX: &[u8] = b"WebPush: info\0";
    const CEK_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";
    const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";
    const AES128GCM_HEADER_LEN: usize = 16 + 4 + 1 + 65;

    assert!(body.len() > AES128GCM_HEADER_LEN, "aes128gcm record");
    let salt = &body[..16];
    assert_eq!(body[20], 65, "keyid is the sender's P-256 public key");
    let as_public_bytes = &body[21..AES128GCM_HEADER_LEN];
    let as_public = p256::PublicKey::from_encoded_point(
        &p256::EncodedPoint::from_bytes(as_public_bytes).expect("encoded point"),
    )
    .expect("sender public key");
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), as_public.as_affine());
    let mut key_info = WEBPUSH_INFO_PREFIX.to_vec();
    key_info.extend_from_slice(secret.public_key().to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(as_public_bytes);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .expect("ikm");
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    prk.expand(CEK_INFO, &mut cek).expect("cek");
    let mut nonce = [0u8; 12];
    prk.expand(NONCE_INFO, &mut nonce).expect("nonce");
    let padded = Aes128Gcm::new_from_slice(&cek)
        .expect("cipher")
        .decrypt((&nonce).into(), &body[AES128GCM_HEADER_LEN..])
        .expect("decrypts with the device's own keys");
    let end = padded
        .iter()
        .rposition(|byte| *byte != 0)
        .expect("padding delimiter");
    assert_eq!(padded[end], 2, "RFC 8188 last-record delimiter");
    padded[..end].to_vec()
}
