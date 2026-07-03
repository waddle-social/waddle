use super::*;
use parser::decode_sasl_name;

fn test_secret() -> String {
    format!("{:x}{:x}", rand::random::<u64>(), rand::random::<u64>())
}

/// Test basic SCRAM key generation.
#[test]
fn test_generate_scram_keys() {
    let password = test_secret();
    let salt = b"salt1234salt1234"; // 16 bytes
    let iterations = 4096;

    let (stored_key, server_key) = generate_scram_keys(&password, salt, iterations);

    // Keys should be 32 bytes (SHA-256 output)
    assert_eq!(stored_key.len(), 32);
    assert_eq!(server_key.len(), 32);

    // Keys should be deterministic
    let (stored_key2, server_key2) = generate_scram_keys(&password, salt, iterations);
    assert_eq!(stored_key, stored_key2);
    assert_eq!(server_key, server_key2);
}

/// Test the full SCRAM exchange with a known password.
#[test]
fn test_scram_full_exchange() {
    // Setup: generate keys for a known password
    let password = test_secret();
    let salt = generate_salt();
    let iterations = 4096;
    let (stored_key, server_key) = generate_scram_keys(&password, &salt, iterations);

    // Create server instance with the same salt
    let mut server = ScramServer::with_salt(salt.clone(), iterations);

    // Client sends client-first-message
    let client_nonce = "rOprNGfwEbeRWgbNEkqO";
    let client_first = format!("n,,n=testuser,r={}", client_nonce);

    // Server processes and generates server-first-message
    let server_first = server.process_client_first(&client_first).unwrap();
    assert_eq!(server_first.username, "testuser");
    assert!(server_first
        .message
        .starts_with(&format!("r={}", client_nonce)));
    assert!(server_first.message.contains(",s="));
    assert!(server_first.message.contains(",i=4096"));

    // Extract the combined nonce for client-final
    let combined_nonce = &server.combined_nonce;

    // Client computes the proof
    // SaltedPassword = Hi(password, salt, i)
    let salted_password = hi(password.as_bytes(), &salt, iterations);

    // ClientKey = HMAC(SaltedPassword, "Client Key")
    let client_key = hmac_sha256(&salted_password, b"Client Key");

    // ClientSignature = HMAC(StoredKey, AuthMessage)
    let channel_binding = BASE64_STANDARD.encode("n,,");
    let client_final_without_proof = format!("c={},r={}", channel_binding, combined_nonce);
    let auth_message = format!(
        "n=testuser,r={},{},{}",
        client_nonce, server_first.message, client_final_without_proof
    );
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());

    // ClientProof = ClientKey XOR ClientSignature
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();

    let client_proof_b64 = BASE64_STANDARD.encode(&client_proof);
    let client_final = format!("{},p={}", client_final_without_proof, client_proof_b64);

    // Server verifies and generates server-final-message
    let server_final = server
        .process_client_final(&client_final, &stored_key, &server_key)
        .unwrap();

    assert!(server_final.message.starts_with("v="));
    assert_eq!(server.state(), &ScramState::Complete);
}

/// Test parsing client-first-message.
#[test]
fn test_parse_client_first() {
    let msg = "n,,n=user,r=fyko+d2lbbFgONRv9qkxdawL";
    let parsed = parse_client_first(msg).unwrap();

    assert_eq!(parsed.gs2_cbind_flag, 'n');
    assert!(parsed.authzid.is_none());
    assert_eq!(parsed.username, "user");
    assert_eq!(parsed.client_nonce, "fyko+d2lbbFgONRv9qkxdawL");
    assert_eq!(parsed.bare, "n=user,r=fyko+d2lbbFgONRv9qkxdawL");
}

/// Test parsing client-first-message with authzid.
#[test]
fn test_parse_client_first_with_authzid() {
    let msg = "n,a=admin,n=user,r=nonce123";
    let parsed = parse_client_first(msg).unwrap();

    assert_eq!(parsed.gs2_cbind_flag, 'n');
    assert_eq!(parsed.authzid, Some("admin".to_string()));
    assert_eq!(parsed.username, "user");
    assert_eq!(parsed.client_nonce, "nonce123");
}

/// Test parsing client-final-message.
#[test]
fn test_parse_client_final() {
    let msg = "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=";
    let parsed = parse_client_final(msg).unwrap();

    assert_eq!(parsed.channel_binding, "biws");
    assert_eq!(parsed.nonce, "fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j");
    assert_eq!(parsed.proof, "v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=");
    assert_eq!(
        parsed.without_proof,
        "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j"
    );
}

/// Test SASL name encoding/decoding.
#[test]
fn test_sasl_name_encoding() {
    assert_eq!(encode_sasl_name("user"), "user");
    assert_eq!(encode_sasl_name("user,name"), "user=2Cname");
    assert_eq!(encode_sasl_name("user=name"), "user=3Dname");
    assert_eq!(encode_sasl_name("a,b=c"), "a=2Cb=3Dc");
}

#[test]
fn test_sasl_name_decoding() {
    assert_eq!(decode_sasl_name("user").unwrap(), "user");
    assert_eq!(decode_sasl_name("user=2Cname").unwrap(), "user,name");
    assert_eq!(decode_sasl_name("user=3Dname").unwrap(), "user=name");
    assert_eq!(decode_sasl_name("a=2Cb=3Dc").unwrap(), "a,b=c");
}

/// RFC 5802 §5.1: the server rejects a client-first-message carrying an
/// authzid — authorization-identity mapping is not implemented, so
/// accepting one would silently authenticate a different identity.
#[test]
fn test_client_first_with_authzid_is_rejected() {
    let mut server = ScramServer::new();
    let result = server.process_client_first("n,a=admin,n=user,r=nonce123");
    assert!(result.is_err());
}

/// RFC 5802 §5.1: the `c=` value in client-final MUST be the base64
/// encoding of the GS2 header from client-first ("n,," → "biws").
/// A mismatching value must fail authentication even with a valid proof.
#[test]
fn test_client_final_channel_binding_mismatch_is_rejected() {
    let password = test_secret();
    let salt = generate_salt();
    let iterations = 4096;
    let (stored_key, server_key) = generate_scram_keys(&password, &salt, iterations);

    let mut server = ScramServer::with_salt(salt.clone(), iterations);
    let client_nonce = "test-nonce";
    let client_first = format!("n,,n=user,r={}", client_nonce);
    let server_first = server.process_client_first(&client_first).unwrap();

    // Compute a VALID proof, but over a tampered channel-binding value
    // ("eSws" is base64("y,,") — a gs2 header the server never saw).
    let salted_password = hi(password.as_bytes(), &salt, iterations);
    let client_key = hmac_sha256(&salted_password, b"Client Key");
    let client_final_without_proof = format!("c=eSws,r={}", server.combined_nonce);
    let auth_message = format!(
        "n=user,r={},{},{}",
        client_nonce, server_first.message, client_final_without_proof
    );
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    let client_final = format!(
        "{},p={}",
        client_final_without_proof,
        BASE64_STANDARD.encode(&client_proof)
    );

    let result = server.process_client_final(&client_final, &stored_key, &server_key);
    assert!(result.is_err());
    assert_eq!(server.state(), &ScramState::Complete);
}

/// Test invalid SCRAM state transitions.
#[test]
fn test_invalid_state_transitions() {
    let mut server = ScramServer::new();

    // Can't process client-final before client-first
    let result = server.process_client_final("c=biws,r=nonce,p=proof", &[], &[]);
    assert!(result.is_err());

    // Process client-first to advance state
    let _ = server.process_client_first("n,,n=user,r=nonce123");

    // Can't process client-first again
    let mut server2 = server.clone();
    let result = server2.process_client_first("n,,n=user2,r=nonce456");
    assert!(result.is_err());
}

/// Test RFC 5802 test vector (adapted for SHA-256).
/// Note: RFC 5802 uses SHA-1, but we test the same structure with SHA-256.
#[test]
fn test_rfc_structure() {
    // This tests that our implementation follows the RFC structure
    let password = test_secret();
    let salt = BASE64_STANDARD.decode("QSXCR+Q6sek8bf92").unwrap();
    let iterations = 4096;

    let (stored_key, server_key) = generate_scram_keys(&password, &salt, iterations);

    // StoredKey and ServerKey should be 32 bytes for SHA-256
    assert_eq!(stored_key.len(), 32);
    assert_eq!(server_key.len(), 32);

    // Verify keys are deterministic
    let (stored_key2, server_key2) = generate_scram_keys(&password, &salt, iterations);
    assert_eq!(stored_key, stored_key2);
    assert_eq!(server_key, server_key2);
}

/// Test nonce generation uniqueness.
#[test]
fn test_nonce_uniqueness() {
    let nonce1 = generate_nonce();
    let nonce2 = generate_nonce();
    let nonce3 = generate_nonce();

    assert_ne!(nonce1, nonce2);
    assert_ne!(nonce2, nonce3);
    assert_ne!(nonce1, nonce3);

    // Nonces should be base64 encoded
    assert!(BASE64_STANDARD.decode(&nonce1).is_ok());
}

/// Test salt generation.
#[test]
fn test_salt_generation() {
    let salt1 = generate_salt();
    let salt2 = generate_salt();

    assert_eq!(salt1.len(), 16);
    assert_eq!(salt2.len(), 16);
    assert_ne!(salt1, salt2);
}

/// Test authentication failure with wrong password.
#[test]
fn test_wrong_password() {
    let correct_password = test_secret();
    let wrong_password = test_secret();
    let salt = generate_salt();
    let iterations = 4096;

    // Generate keys for correct password
    let (stored_key, server_key) = generate_scram_keys(&correct_password, &salt, iterations);

    // Generate keys for wrong password (simulating what client would compute)
    let (_, _) = generate_scram_keys(&wrong_password, &salt, iterations);

    // Start SCRAM exchange
    let mut server = ScramServer::with_salt(salt.clone(), iterations);
    let client_nonce = "test-nonce";
    let client_first = format!("n,,n=user,r={}", client_nonce);
    let server_first = server.process_client_first(&client_first).unwrap();

    // Client computes proof with WRONG password
    let wrong_salted_password = hi(wrong_password.as_bytes(), &salt, iterations);
    let wrong_client_key = hmac_sha256(&wrong_salted_password, b"Client Key");
    let wrong_stored_key = sha256(&wrong_client_key);

    let channel_binding = BASE64_STANDARD.encode("n,,");
    let client_final_without_proof = format!("c={},r={}", channel_binding, server.combined_nonce);
    let auth_message = format!(
        "n=user,r={},{},{}",
        client_nonce, server_first.message, client_final_without_proof
    );
    let wrong_client_signature = hmac_sha256(&wrong_stored_key, auth_message.as_bytes());

    let wrong_client_proof: Vec<u8> = wrong_client_key
        .iter()
        .zip(wrong_client_signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();

    let wrong_proof_b64 = BASE64_STANDARD.encode(&wrong_client_proof);
    let client_final = format!("{},p={}", client_final_without_proof, wrong_proof_b64);

    // Server should reject
    let result = server.process_client_final(&client_final, &stored_key, &server_key);
    assert!(result.is_err());
}
