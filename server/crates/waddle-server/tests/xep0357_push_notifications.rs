//! XEP-0357 / Web Push lifecycle integration tests for the
//! `waddle-server` side of PR-D1.
//!
//! Unit tests for the sealing primitives + env-var parsing live in
//! `src/push_service/vapid_storage.rs`. This file exercises the
//! `VapidStorage::load_or_provision` boot path end-to-end against an
//! in-memory database — fresh generation, persisted reuse, env-var
//! bootstrap without process environment mutation, and the AAD-kid binding that
//! makes a sealed blob inseparable from the `kid` it was sealed against
//! (the AAD includes `label || kid`, so cross-row blob swaps fail the
//! GCM tag check). `kid` is the table's primary key — there is no
//! autoincrement `id` column.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use std::env;
use std::process::Command;
use waddle_server::db::Database;
use waddle_server::push_service::vapid_storage::VapidStorage;

const ROOT_KEY: &[u8] = b"vapid-test-root-key-32-bytes-min";
const VAPID_ENV_VAR: &str = "WADDLE_VAPID_PRIVATE_KEY";

async fn fresh_db(name: &str) -> Database {
    Database::in_memory(name).await.expect("fresh in-memory db")
}

fn b64u(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Configure a fresh process before runtime/database threads exist. A lock in
/// this process cannot protect libc/third-party environment readers.
fn run_in_subprocess(test_name: &str, key: Option<&str>) -> bool {
    const CHILD_MARKER: &str = "WADDLE_VAPID_TEST_CHILD";
    if env::var(CHILD_MARKER).as_deref() == Ok(test_name) {
        return true;
    }
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_MARKER, test_name)
        .env_remove(VAPID_ENV_VAR);
    if let Some(key) = key {
        command.env(VAPID_ENV_VAR, key);
    }
    let output = command.output().expect("run VAPID lifecycle subprocess");
    assert!(
        output.status.success(),
        "VAPID lifecycle subprocess failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    false
}

#[tokio::test]
async fn fresh_boot_generates_and_persists_keypair() {
    if !run_in_subprocess("fresh_boot_generates_and_persists_keypair", None) {
        return;
    }
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
async fn env_var_bootstrap_persists_without_mutating_environment() {
    let scalar = [0xAEu8; 32];
    let encoded = b64u(&scalar);
    if !run_in_subprocess(
        "env_var_bootstrap_persists_without_mutating_environment",
        Some(&encoded),
    ) {
        return;
    }

    let db = fresh_db("vapid-env-bootstrap").await;

    let signer = VapidStorage::load_or_provision(db.clone(), ROOT_KEY)
        .await
        .expect("env-bootstrap path provisions");
    let kid = signer.current_kid();

    assert_eq!(
        env::var(VAPID_ENV_VAR).expect("bootstrap environment retained"),
        encoded,
    );

    // The read-only loader ignores bootstrap inputs and proves the env-supplied
    // key is durably available without clearing a process-global variable.
    let signer2 = VapidStorage::load_or_ephemeral(db, ROOT_KEY)
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
    if !run_in_subprocess(
        "env_var_malformed_does_not_brick_boot_silently",
        Some("not-base64url-and-also-wrong-length"),
    ) {
        return;
    }

    let db = fresh_db("vapid-env-malformed").await;
    let result = VapidStorage::load_or_provision(db, ROOT_KEY).await;
    assert!(result.is_err(), "malformed env scalar must surface as Err");
    // The bootstrap secret remains available on failure for diagnosis/retry.
    assert!(
        env::var(VAPID_ENV_VAR).is_ok(),
        "env var must NOT be removed on parse error"
    );
}

#[tokio::test]
async fn sign_and_verify_round_trip_under_loaded_key() {
    if !run_in_subprocess("sign_and_verify_round_trip_under_loaded_key", None) {
        return;
    }
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

/// A lost Phase C rebuild keeps canonical push preview and processing hints.
#[test]
fn recovery_envelope_preserves_push_body_and_hints() {
    use waddle_server::notification_outbox::{
        candidate_from_envelope, GroupchatCandidateIdentity, NotificationThreadId,
    };
    use waddle_xmpp::xep::{xep0334, xep0421::OccupantIdSecret};

    let owner = "juliet@example.com".parse().expect("owner");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let sender = room.with_resource_str("romeo").expect("occupant").into();
    let archive = waddle_xmpp_core::xep0359::StanzaId::new("frozen", room.clone().into());
    let secret = OccupantIdSecret::try_new(vec![7; 32]).expect("secret");
    let mut envelope = xmpp_parsers::message::Message::new(None);
    envelope.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "canonical preview".to_owned(),
    );
    envelope.bodies.insert(
        xmpp_parsers::message::Lang::from("fr"),
        "autre aperçu".to_owned(),
    );
    let rebuild = |message: &xmpp_parsers::message::Message| {
        candidate_from_envelope(
            message,
            GroupchatCandidateIdentity {
                owner: &owner,
                room: &room,
                sender: &sender,
                thread_id: NotificationThreadId::root(),
                archive_stanza_id: &archive,
                is_live_occupant: false,
                sender_can_broadcast_channel_mention: false,
            },
            &secret,
        )
        .expect("rebuild candidate")
    };
    assert_eq!(
        rebuild(&envelope).last_message_body(),
        Some("canonical preview")
    );
    for hint in [xep0334::Hint::NoStore, xep0334::Hint::NoPermanentStore] {
        envelope.payloads.push(
            xmpp_parsers::minidom::Element::builder(hint.element_name(), xep0334::NS_HINTS).build(),
        );
    }
    let candidate = rebuild(&envelope);
    assert_eq!(candidate.last_message_body(), None);
    assert!(candidate.no_store());
    assert!(candidate.no_permanent_store());
    assert!(!candidate.reaction());
    assert_eq!(candidate.archive_stanza_id(), &archive);
}
