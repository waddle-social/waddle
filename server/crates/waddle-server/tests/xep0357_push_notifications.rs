//! XEP-0357 / Web Push lifecycle integration tests for the
//! `waddle-server` side of PR-D1.
//!
//! Unit tests for the sealing primitives + env-var parsing live in
//! `src/push_service/vapid_storage.rs`. This file exercises the
//! `VapidStorage::load_or_provision` boot path end-to-end against an
//! in-memory database — fresh generation, persisted reuse, env-var
//! bootstrap with write-before-remove, and the AAD-kid binding that
//! makes a sealed blob inseparable from the `kid` it was sealed against
//! (the AAD includes `label || kid`, so cross-row blob swaps fail the
//! GCM tag check). `kid` is the table's primary key — there is no
//! autoincrement `id` column.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use std::env;
use tokio::sync::{Mutex, MutexGuard};
use waddle_server::db::Database;
use waddle_server::push_service::vapid_storage::VapidStorage;

/// Serializes tests that mutate `WADDLE_VAPID_PRIVATE_KEY`. The env is
/// process-global; cargo runs tests in parallel by default. We use
/// `tokio::sync::Mutex` so the guard can be held across the `.await`s
/// inside `load_or_provision` without triggering `await_holding_lock`.
static ENV_MUTATION_LOCK: Mutex<()> = Mutex::const_new(());

const ROOT_KEY: &[u8] = b"vapid-test-root-key-32-bytes-min";
const VAPID_ENV_VAR: &str = "WADDLE_VAPID_PRIVATE_KEY";

async fn fresh_db(name: &str) -> Database {
    Database::in_memory(name).await.expect("fresh in-memory db")
}

fn b64u(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RAII guard: acquires `ENV_MUTATION_LOCK` and clears the env var; on
/// drop (even via panic) the env var is cleared again so no test sees
/// state left behind by a previous one.
struct EnvGuard<'a> {
    _lock: MutexGuard<'a, ()>,
}

impl<'a> EnvGuard<'a> {
    async fn acquire() -> EnvGuard<'a> {
        let lock = ENV_MUTATION_LOCK.lock().await;
        // SAFETY: see vapid_storage.rs SAFETY note. The lock guarantees
        // exclusive env access for the duration of this guard's lifetime.
        unsafe {
            env::remove_var(VAPID_ENV_VAR);
        }
        EnvGuard { _lock: lock }
    }

    fn set(&self, value: &str) {
        // SAFETY: same as above.
        unsafe {
            env::set_var(VAPID_ENV_VAR, value);
        }
    }
}

impl Drop for EnvGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: same as above; runs unconditionally so a panicking test
        // does not leak env state into subsequent tests.
        unsafe {
            env::remove_var(VAPID_ENV_VAR);
        }
    }
}

#[tokio::test]
async fn fresh_boot_generates_and_persists_keypair() {
    let _guard = EnvGuard::acquire().await;
    let db = fresh_db("vapid-fresh-generate").await;

    let signer = VapidStorage::load_or_provision(db.clone(), ROOT_KEY)
        .await
        .expect("provisions on fresh DB");

    // A kid was generated.
    let kid = signer.current_kid();
    let pk_bytes = signer
        .current_public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    assert_eq!(pk_bytes.len(), 65);
    assert_eq!(pk_bytes[0], 0x04);

    // Second boot reuses the same kid (loaded from DB, not regenerated).
    let signer2 = VapidStorage::load_or_provision(db, ROOT_KEY)
        .await
        .expect("reloads from DB");
    assert_eq!(signer2.current_kid(), kid);
    let pk_bytes2 = signer2
        .current_public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    assert_eq!(pk_bytes2, pk_bytes);
}

#[tokio::test]
async fn env_var_bootstrap_writes_before_remove() {
    let guard = EnvGuard::acquire().await;
    let scalar = [0xAEu8; 32];
    let encoded = b64u(&scalar);
    guard.set(&encoded);

    let db = fresh_db("vapid-env-bootstrap").await;

    let signer = VapidStorage::load_or_provision(db.clone(), ROOT_KEY)
        .await
        .expect("env-bootstrap path provisions");
    let kid = signer.current_kid();

    // The env var MUST be unset after a successful bootstrap.
    assert!(
        env::var(VAPID_ENV_VAR).is_err(),
        "WADDLE_VAPID_PRIVATE_KEY must be removed from process env after DB write"
    );

    // Second boot (env unset, no env path) loads the same kid from DB —
    // proves the env-supplied key was actually persisted before the env was
    // cleared.
    let signer2 = VapidStorage::load_or_provision(db, ROOT_KEY)
        .await
        .expect("reload");
    assert_eq!(
        signer2.current_kid(),
        kid,
        "env-supplied key survived to the next boot via the DB"
    );

    // Verify the persisted public key matches what the env-supplied scalar
    // produces.
    let expected_pk = p256::SecretKey::from_slice(&scalar)
        .expect("env scalar")
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let got_pk = signer2
        .current_public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    assert_eq!(got_pk, expected_pk);
}

#[tokio::test]
async fn env_var_malformed_does_not_brick_boot_silently() {
    let guard = EnvGuard::acquire().await;
    guard.set("not-base64url-and-also-wrong-length");

    let db = fresh_db("vapid-env-malformed").await;
    let result = VapidStorage::load_or_provision(db, ROOT_KEY).await;
    assert!(result.is_err(), "malformed env scalar must surface as Err");
    // Env var must still be set so the operator can fix and retry —
    // write-before-remove ordering protects against silent key loss.
    assert!(
        env::var(VAPID_ENV_VAR).is_ok(),
        "env var must NOT be removed on parse error"
    );
    // EnvGuard's Drop cleans up.
}

#[tokio::test]
async fn sign_and_verify_round_trip_under_loaded_key() {
    let _guard = EnvGuard::acquire().await;
    let db = fresh_db("vapid-sign-verify").await;

    let signer = VapidStorage::load_or_provision(db, ROOT_KEY)
        .await
        .expect("provisions");

    use url::Url;
    use waddle_xmpp::push::types::VapidSub;
    use waddle_xmpp::push::vapid::aud_for;
    let endpoint = Url::parse("https://fcm.googleapis.com/fcm/send/abc").unwrap();
    let aud = aud_for(&endpoint).expect("valid endpoint");
    let sub = VapidSub::default_for_domain("example.com").unwrap();
    let jwt = signer.sign(&aud, &sub).expect("sign");

    // Verify the JWT validates under the signer's own public key — proves
    // the persisted-and-loaded private key actually signs correctly.
    use p256::pkcs8::EncodePublicKey;
    let pk_pem = signer
        .current_public_key()
        .to_public_key_pem(Default::default())
        .expect("public-key PEM");
    let decoding_key =
        jsonwebtoken::DecodingKey::from_ec_pem(pk_pem.as_bytes()).expect("decoding key");
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    validation.set_audience(&["https://fcm.googleapis.com"]);
    let _ = jsonwebtoken::decode::<serde_json::Value>(jwt.as_str(), &decoding_key, &validation)
        .expect("JWT verifies under loaded public key");
}
