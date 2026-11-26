//! Mock-HTTP integration tests proving `ScalewayKmsProvider` speaks the exact
//! Scaleway Key Manager v1alpha1 JSON wire shape for lookup, create, encrypt,
//! decrypt, delete-key-material, and status checks.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;
use waddle_broker::{
    BrokerCustodyService, BrokerError, EnrollCredentialInput, Key256, KeyHierarchyService,
    KmsError, KmsKeyRef, KmsProvider, ScalewayKmsConfig, ScalewayKmsProvider, SecretBytes,
    SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

#[derive(Debug, Clone)]
struct RecordedScwRequest {
    method: String,
    path: String,
    auth_token: Option<String>,
    body: Option<Value>,
}

struct MockScwKey {
    id: String,
    project_id: String,
    name: String,
    state: String,
    material: [u8; 32],
}

#[derive(Default)]
struct MockScwState {
    expected_token: String,
    keys: HashMap<String, MockScwKey>,
    requests: Vec<RecordedScwRequest>,
}

struct MockScalewayKmsServer {
    base_url: String,
    state: Arc<RwLock<MockScwState>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockScalewayKmsServer {
    async fn start(expected_token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock scaleway kms");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(RwLock::new(MockScwState {
            expected_token: expected_token.to_owned(),
            keys: HashMap::new(),
            requests: Vec::new(),
        }));
        let state_clone = Arc::clone(&state);
        let (tx, mut rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let st = Arc::clone(&state_clone);
                        tokio::spawn(async move {
                            let _ = handle_conn(&mut stream, st).await;
                        });
                    }
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            state,
            shutdown: Some(tx),
        }
    }

    fn requests(&self) -> Vec<RecordedScwRequest> {
        self.state.read().expect("lock").requests.clone()
    }
}

impl Drop for MockScalewayKmsServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_conn(
    stream: &mut tokio::net::TcpStream,
    state: Arc<RwLock<MockScwState>>,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.lines();
    let req_line = lines.next().unwrap_or_default();
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut content_length = 0usize;
    let mut auth_token = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("x-auth-token") {
                auth_token = Some(v.trim().to_owned());
            }
        }
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body_bytes = if content_length > 0 && buf.len() >= body_start + content_length {
        &buf[body_start..body_start + content_length]
    } else {
        &[]
    };
    let body_json: Option<Value> = if body_bytes.is_empty() {
        None
    } else {
        serde_json::from_slice(body_bytes).ok()
    };

    let (status, reason, resp_body) = route_request(&state, &method, &path, auth_token, body_json);
    let resp_str = resp_body.map(|v| v.to_string()).unwrap_or_default();
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        resp_str.len(),
        resp_str
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}

fn route_request(
    state: &Arc<RwLock<MockScwState>>,
    method: &str,
    full_path: &str,
    auth_token: Option<String>,
    body: Option<Value>,
) -> (u16, &'static str, Option<Value>) {
    let mut guard = state.write().expect("lock");
    guard.requests.push(RecordedScwRequest {
        method: method.to_owned(),
        path: full_path.to_owned(),
        auth_token: auth_token.clone(),
        body: body.clone(),
    });
    if auth_token.as_deref() != Some(guard.expected_token.as_str()) {
        return (401, "Unauthorized", Some(json!({ "message": "denied" })));
    }

    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    // /key-manager/v1alpha1/regions/{region}/keys
    if segments.len() == 5
        && segments[0] == "key-manager"
        && segments[1] == "v1alpha1"
        && segments[2] == "regions"
        && segments[4] == "keys"
    {
        if method == "GET" {
            let parsed_url = url::Url::parse(&format!("http://localhost/?{query}")).expect("url");
            let params: HashMap<String, String> = parsed_url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let project_filter = params.get("project_id").map(String::as_str);
            let name_filter = params.get("name").map(String::as_str);
            let keys: Vec<Value> = guard
                .keys
                .values()
                .filter(|k| {
                    project_filter.is_none_or(|p| k.project_id == p)
                        && name_filter.is_none_or(|n| k.name.contains(n))
                })
                .map(|k| {
                    json!({
                        "id": k.id,
                        "project_id": k.project_id,
                        "name": k.name,
                        "state": k.state,
                    })
                })
                .collect();
            return (
                200,
                "OK",
                Some(json!({ "keys": keys, "total_count": keys.len() })),
            );
        }
        if method == "POST" {
            let Some(val) = body else {
                return (
                    400,
                    "Bad Request",
                    Some(json!({ "message": "missing body" })),
                );
            };
            let project_id = val["project_id"].as_str().unwrap_or_default().to_owned();
            let name = val["name"].as_str().unwrap_or_default().to_owned();
            let sym = val["usage"]["symmetric_encryption"]
                .as_str()
                .unwrap_or_default();
            let origin = val["origin"].as_str().unwrap_or_default();
            let unprotected = val["unprotected"].as_bool();
            if project_id.is_empty()
                || name.is_empty()
                || sym != "aes_256_gcm"
                || origin != "scaleway_kms"
                || unprotected != Some(false)
            {
                return (
                    400,
                    "Bad Request",
                    Some(json!({ "message": "invalid create key wire shape" })),
                );
            }
            let id = Uuid::new_v4().to_string();
            let mut material = [0u8; 32];
            material[..16].copy_from_slice(Uuid::new_v4().as_bytes());
            material[16..].copy_from_slice(Uuid::new_v4().as_bytes());
            guard.keys.insert(
                id.clone(),
                MockScwKey {
                    id: id.clone(),
                    project_id: project_id.clone(),
                    name: name.clone(),
                    state: "enabled".to_owned(),
                    material,
                },
            );
            return (
                200,
                "OK",
                Some(json!({
                    "id": id,
                    "project_id": project_id,
                    "name": name,
                    "state": "enabled",
                })),
            );
        }
    }

    // /key-manager/v1alpha1/regions/{region}/keys/{key_id}
    if segments.len() == 6
        && segments[0] == "key-manager"
        && segments[1] == "v1alpha1"
        && segments[2] == "regions"
        && segments[4] == "keys"
        && method == "GET"
    {
        let key_id = segments[5];
        return match guard.keys.get(key_id) {
            Some(k) => (
                200,
                "OK",
                Some(json!({
                    "id": k.id,
                    "project_id": k.project_id,
                    "name": k.name,
                    "state": k.state,
                })),
            ),
            None => (404, "Not Found", Some(json!({ "message": "not found" }))),
        };
    }

    // /key-manager/v1alpha1/regions/{region}/keys/{key_id}/{action}
    if segments.len() == 7
        && segments[0] == "key-manager"
        && segments[1] == "v1alpha1"
        && segments[2] == "regions"
        && segments[4] == "keys"
        && method == "POST"
    {
        let key_id = segments[5];
        let action = segments[6];
        let Some(key) = guard.keys.get_mut(key_id) else {
            return (
                404,
                "Not Found",
                Some(json!({ "message": "key not found" })),
            );
        };
        match action {
            "encrypt" => {
                if key.state != "enabled" {
                    return (
                        410,
                        "Gone",
                        Some(json!({ "message": "key material destroyed" })),
                    );
                }
                let val = body.expect("encrypt body");
                let plaintext = STANDARD
                    .decode(val["plaintext"].as_str().unwrap_or_default())
                    .expect("b64 plaintext");
                let aad = STANDARD
                    .decode(val["associated_data"].as_str().unwrap_or_default())
                    .expect("b64 aad");
                let cipher = Aes256Gcm::new_from_slice(&key.material).expect("aes");
                let nonce_bytes = [7u8; 12];
                let ct = cipher
                    .encrypt(
                        &Nonce::from(nonce_bytes),
                        Payload {
                            msg: &plaintext,
                            aad: &aad,
                        },
                    )
                    .expect("encrypt");
                let mut combined = Vec::with_capacity(12 + ct.len());
                combined.extend_from_slice(&nonce_bytes);
                combined.extend_from_slice(&ct);
                return (
                    200,
                    "OK",
                    Some(json!({ "ciphertext": STANDARD.encode(combined) })),
                );
            }
            "decrypt" => {
                if key.state != "enabled" {
                    return (
                        410,
                        "Gone",
                        Some(json!({ "message": "key material destroyed" })),
                    );
                }
                let val = body.expect("decrypt body");
                let raw = STANDARD
                    .decode(val["ciphertext"].as_str().unwrap_or_default())
                    .expect("b64 ct");
                let aad = STANDARD
                    .decode(val["associated_data"].as_str().unwrap_or_default())
                    .expect("b64 aad");
                let nonce_bytes: [u8; 12] = raw[..12].try_into().expect("nonce");
                let cipher = Aes256Gcm::new_from_slice(&key.material).expect("aes");
                let pt = match cipher.decrypt(
                    &Nonce::from(nonce_bytes),
                    Payload {
                        msg: &raw[12..],
                        aad: &aad,
                    },
                ) {
                    Ok(pt) => pt,
                    Err(_) => return (400, "Bad Request", Some(json!({ "message": "bad aad" }))),
                };
                return (200, "OK", Some(json!({ "plaintext": STANDARD.encode(pt) })));
            }
            "delete-key-material" => {
                if key.state == "destroyed" {
                    return (410, "Gone", Some(json!({ "message": "already destroyed" })));
                }
                key.state = "destroyed".to_owned();
                key.material = [0u8; 32];
                return (200, "OK", Some(json!({})));
            }
            _ => {}
        }
    }

    (
        404,
        "Not Found",
        Some(json!({ "message": "unknown route" })),
    )
}

#[tokio::test]
async fn scaleway_kms_create_lookup_encrypt_decrypt_and_destroy_wire_contract() {
    let token = "scw-secret-live-test-token";
    let project_id = "22222222-3333-4444-5555-666666666666";
    let server = MockScalewayKmsServer::start(token).await;

    let provider = ScalewayKmsProvider::new(ScalewayKmsConfig {
        base_url: server.base_url.clone(),
        region: "fr-par".to_owned(),
        project_id: project_id.to_owned(),
        auth_token: Zeroizing::new(token.to_owned()),
        root_key_id: None,
        timeout: Duration::from_secs(5),
    })
    .expect("construct scaleway kms provider");

    let tenant = TenantId::parse("waddle.social").expect("tenant");
    let account = AccountId::new_v4();

    // 1. ensure_tenant_kek performs GET lookup -> miss -> POST create and returns a real UUID.
    let kek_ref = provider
        .ensure_tenant_kek(&tenant)
        .await
        .expect("ensure tenant kek");
    assert!(
        Uuid::parse_str(kek_ref.as_str()).is_ok(),
        "Scaleway KMS key ref must be a real key UUID, got {}",
        kek_ref.as_str()
    );
    assert_eq!(
        provider.tenant_kek_ref(&tenant).expect("cached ref"),
        kek_ref
    );

    // 2. A fresh provider instance (simulating pod restart) discovers the SAME key UUID via GET lookup without creating a duplicate.
    let restarted_provider = ScalewayKmsProvider::new(ScalewayKmsConfig {
        base_url: server.base_url.clone(),
        region: "fr-par".to_owned(),
        project_id: project_id.to_owned(),
        auth_token: Zeroizing::new(token.to_owned()),
        root_key_id: None,
        timeout: Duration::from_secs(5),
    })
    .expect("construct restarted provider");
    let discovered_ref = restarted_provider
        .ensure_tenant_kek(&tenant)
        .await
        .expect("discover existing tenant kek");
    assert_eq!(discovered_ref, kek_ref);

    // 3. Encrypt + Decrypt roundtrip via real Scaleway JSON wire shape.
    let dek = Key256::generate().expect("dek");
    let wrapped = restarted_provider
        .wrap_key(&discovered_ref, &dek, b"tenant-aad-v1")
        .await
        .expect("wrap key");
    let unwrapped = restarted_provider
        .unwrap_key(&discovered_ref, &wrapped, b"tenant-aad-v1")
        .await
        .expect("unwrap key");
    assert_eq!(unwrapped, dek);

    // 4. End-to-end BrokerCustodyService enrollment + unseal + user shredding against Scaleway KMS.
    let kms_arc: Arc<dyn KmsProvider> = Arc::new(restarted_provider);
    let hierarchy = Arc::new(KeyHierarchyService::new(
        "waddle.social",
        Arc::clone(&kms_arc),
    ));
    let ledger = Arc::new(
        SqliteCompatLedgerStore::connect_in_memory()
            .await
            .expect("ledger"),
    );
    let custody = BrokerCustodyService::new(hierarchy, ledger)
        .await
        .expect("custody");
    let receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: Some(account),
            provider: "openrouter".to_owned(),
            nonce: format!("scw-nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x22; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::from_slice(b"sk-or-v1-scaleway-kms-test").expect("secret"),
        })
        .await
        .expect("enroll credential");
    let unsealed = custody
        .unseal_for_provider_egress(&tenant, receipt.credential_id, "openrouter")
        .await
        .expect("unseal");
    assert_eq!(unsealed.as_bytes(), b"sk-or-v1-scaleway-kms-test");

    // 5. Destroy user wrapping key via /delete-key-material and verify is_destroyed + fail-closed unwrap.
    let user_ref = kms_arc
        .ensure_user_wrapping_secret(&tenant, account)
        .await
        .expect("user key ref");
    assert!(!kms_arc
        .is_destroyed(&user_ref)
        .await
        .expect("not destroyed"));
    kms_arc
        .destroy_wrapping_secret(&user_ref)
        .await
        .expect("destroy user key");
    assert!(kms_arc.is_destroyed(&user_ref).await.expect("destroyed"));
    assert!(matches!(
        custody
            .unseal_for_provider_egress(&tenant, receipt.credential_id, "openrouter")
            .await,
        Err(BrokerError::Kms(KmsError::KeyDestroyed(_)))
    ));

    // Verify exact Scaleway wire requests recorded.
    let reqs = server.requests();
    assert!(reqs.iter().all(|r| r.auth_token.as_deref() == Some(token)));
    assert!(reqs.iter().any(|r| r.method == "GET"
        && r.path
            .starts_with("/key-manager/v1alpha1/regions/fr-par/keys?")
        && r.path.contains(project_id)));
    assert!(reqs.iter().any(|r| {
        r.method == "POST"
            && r.path == "/key-manager/v1alpha1/regions/fr-par/keys"
            && r.body.as_ref().is_some_and(|b| {
                b["project_id"] == project_id
                    && b["usage"]["symmetric_encryption"] == "aes_256_gcm"
                    && b["unprotected"] == false
                    && b["origin"] == "scaleway_kms"
            })
    }));
    assert!(reqs.iter().any(|r| r.method == "POST"
        && r.path
            == format!(
                "/key-manager/v1alpha1/regions/fr-par/keys/{}/delete-key-material",
                user_ref.as_str()
            )));
}

#[tokio::test]
async fn scaleway_kms_validates_project_id_and_supports_preprovisioned_root_key_id() {
    let invalid = ScalewayKmsProvider::new(ScalewayKmsConfig {
        base_url: "https://api.scaleway.com".to_owned(),
        region: "fr-par".to_owned(),
        project_id: "   ".to_owned(),
        auth_token: Zeroizing::new("tok".to_owned()),
        root_key_id: None,
        timeout: Duration::from_secs(5),
    });
    assert!(matches!(invalid, Err(KmsError::InvalidConfig(_))));

    let preprovisioned = Uuid::new_v4().to_string();
    let provider = ScalewayKmsProvider::new(ScalewayKmsConfig {
        base_url: "https://api.scaleway.com".to_owned(),
        region: "fr-par".to_owned(),
        project_id: String::new(),
        auth_token: Zeroizing::new("tok".to_owned()),
        root_key_id: Some(preprovisioned.clone()),
        timeout: Duration::from_secs(5),
    })
    .expect("root_key_id fallback config");
    let tenant = TenantId::parse("waddle.social").expect("tenant");
    let resolved = provider
        .ensure_tenant_kek(&tenant)
        .await
        .expect("root key fallback");
    assert_eq!(resolved, KmsKeyRef::new(preprovisioned).expect("ref"));
}
