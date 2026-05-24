//! XEP-0357: Push Notifications — custom test suite (CLAUDE.md hard rule).
//!
//! Module-level unit tests live in `src/push/{constants,types,encrypt,vapid}.rs`.
//! This file holds the cross-module integration assertions and any
//! XEP-0357-shape feature-advertisement tests.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use url::Url;

use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::pubsub::pep::pep_features;
use waddle_xmpp::push::constants::{
    AES128GCM_HEADER_LEN, AES128GCM_PAD_DELIMITER_LEN, AES128GCM_TAG_LEN, DEFAULT_PLAINTEXT_BUCKET,
    DM_PLAINTEXT_BUCKET, WEB_PUSH_MAX_BODY_LEN, WEB_PUSH_MAX_PLAINTEXT, WEB_PUSH_MAX_RS,
};
use waddle_xmpp::push::encrypt::{encrypt, header_keyid, header_rs};
use waddle_xmpp::push::types::{Kid, SubscriptionKeys, VapidSub};
use waddle_xmpp::push::vapid::{aud_for, vapid_k_header, InProcessVapidSigner, VapidSigner};

const PUSH_FEATURE: &str = "urn:xmpp:push:0";

// ── XEP-0357 §10 feature advertisement ────────────────────────────────────────

#[test]
fn xep0357_pep_advertises_push_feature() {
    let features = pep_features();
    assert!(features.contains(&Feature::new(PUSH_FEATURE)));
}

#[test]
fn xep0357_push_feature_not_leaked_to_server_root_disco() {
    let features = server_features();
    assert!(!features.contains(&Feature::new(PUSH_FEATURE)));
}

#[test]
fn xep0357_push_feature_is_unique_in_pep_features() {
    let features = pep_features();
    let count = features.iter().filter(|f| f.0 == PUSH_FEATURE).count();
    assert_eq!(count, 1);
}

// ── RFC 8291 / RFC 8188 constants invariants (cross-checked here so a
//    regression in constants.rs surfaces in the dedicated XEP test suite) ─────

#[test]
fn rfc_8188_body_arithmetic() {
    // Body = header + record, record = plaintext_pad_delim_tag, rs is per-record max.
    assert_eq!(
        AES128GCM_HEADER_LEN + WEB_PUSH_MAX_RS as usize,
        WEB_PUSH_MAX_BODY_LEN
    );
    assert_eq!(
        WEB_PUSH_MAX_PLAINTEXT,
        WEB_PUSH_MAX_RS as usize - AES128GCM_TAG_LEN - AES128GCM_PAD_DELIMITER_LEN
    );
    // Conservative cap is 4096 bytes body.
    assert_eq!(WEB_PUSH_MAX_BODY_LEN, 4096);
    assert_eq!(WEB_PUSH_MAX_RS, 4010);
    assert_eq!(WEB_PUSH_MAX_PLAINTEXT, 3993);
}

// ── RFC 8291 encrypt — header-field round-trip and bucket determinism ────────

fn sample_subscription() -> SubscriptionKeys {
    let secret = p256::SecretKey::random(&mut OsRng);
    let pk = secret.public_key().to_encoded_point(false);
    let p256dh = URL_SAFE_NO_PAD.encode(pk.as_bytes());
    let auth = URL_SAFE_NO_PAD.encode([0xCDu8; 16]);
    SubscriptionKeys::from_base64url(&p256dh, &auth).expect("valid subscription")
}

#[test]
fn aes128gcm_header_rs_round_trips_to_record_length() {
    let sub = sample_subscription();
    let payload = encrypt(&sub, b"hello", DEFAULT_PLAINTEXT_BUCKET).expect("encrypts");
    let body = payload.as_slice();
    let rs = header_rs(body).expect("rs field present");
    // rs in the header == actual record length on the wire (no fragmentation).
    let record_len = body.len() - AES128GCM_HEADER_LEN;
    assert_eq!(rs as usize, record_len);
    // record_len = bucket + delim + tag for the chosen bucket size.
    assert_eq!(
        record_len,
        DEFAULT_PLAINTEXT_BUCKET + AES128GCM_PAD_DELIMITER_LEN + AES128GCM_TAG_LEN
    );
}

#[test]
fn dm_bucket_stays_under_web_push_max_body() {
    let sub = sample_subscription();
    // Exercise the max DM bucket plaintext.
    let pt = vec![0u8; DM_PLAINTEXT_BUCKET];
    let payload = encrypt(&sub, &pt, DM_PLAINTEXT_BUCKET).expect("encrypts");
    assert!(payload.as_slice().len() <= WEB_PUSH_MAX_BODY_LEN);
}

#[test]
fn encrypt_keyid_is_uncompressed_p256_point() {
    let sub = sample_subscription();
    let payload = encrypt(&sub, b"x", DEFAULT_PLAINTEXT_BUCKET).expect("encrypts");
    let keyid = header_keyid(payload.as_slice()).expect("keyid present");
    assert_eq!(keyid.len(), 65);
    assert_eq!(keyid[0], 0x04, "uncompressed-point prefix");
}

// ── RFC 8292 VAPID — end-to-end sign + verify under our own key ──────────────

#[test]
fn vapid_signer_produces_jwt_verifiable_under_own_public_key() {
    let secret = p256::SecretKey::random(&mut OsRng);
    let kid = Kid::new();
    let signer = InProcessVapidSigner::new(kid, secret).expect("signer");
    let aud = aud_for(&Url::parse("https://fcm.googleapis.com/fcm/send/abc").unwrap())
        .expect("valid endpoint");
    let sub = VapidSub::default_for_domain("example.com").expect("valid sub");
    let jwt = signer.sign(&aud, &sub).expect("sign");

    // Verify the JWT under our own public key.
    use p256::pkcs8::EncodePublicKey;
    let pk_pem = signer
        .current_public_key()
        .to_public_key_pem(Default::default())
        .expect("public key PEM");
    let decoding_key =
        jsonwebtoken::DecodingKey::from_ec_pem(pk_pem.as_bytes()).expect("decoding key");
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    validation.set_audience(&["https://fcm.googleapis.com"]);
    // Decode into untyped JSON to avoid coupling to internal claim struct shape.
    let _decoded =
        jsonwebtoken::decode::<serde_json::Value>(jwt.as_str(), &decoding_key, &validation)
            .expect("JWT verifies under our own public key");
}

#[test]
fn vapid_k_header_matches_signer_public_key() {
    let secret = p256::SecretKey::random(&mut OsRng);
    let signer = InProcessVapidSigner::new(Kid::new(), secret).expect("signer");
    let k = vapid_k_header(&signer.current_public_key());
    // Decoded bytes are exactly the uncompressed public-key point.
    let decoded = URL_SAFE_NO_PAD.decode(&k).expect("base64url decode");
    assert_eq!(decoded.len(), 65);
    assert_eq!(decoded[0], 0x04);
    let expected = signer
        .current_public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    assert_eq!(decoded, expected);
}
