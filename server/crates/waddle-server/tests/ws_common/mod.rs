//! WebSocket XMPP test client and test server harness.
//!
//! - `TestServer` spawns the `waddle-server` binary with test env vars and
//!   waits for it to become ready.
//! - `WsXmppClient` connects via WebSocket and authenticates with SCRAM-SHA-256.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite};

type HmacSha256 = Hmac<Sha256>;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);
// Each test spawns a fresh waddle-server binary; startup includes
// ephemeral cert generation (CPU-bound ring keygen) before listeners
// bind. Under `cargo test --test-threads=N` several servers race for
// CPU and one can miss a tight budget on slower CI runners. Use a longer
// budget to reduce CI flake during full-workspace test runs.
const SERVER_STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Test server harness
// ---------------------------------------------------------------------------

/// A running waddle-server process for integration tests.
///
/// Spawns the binary with ephemeral certs and the fixed test account enabled.
/// The server binds HTTP to port 0 and writes the actual port to a temp file
/// so there are no TOCTOU port conflicts between parallel tests.
/// Kills the process on drop.
pub struct TestServer {
    process: Child,
    http_port: u16,
    port_file: std::path::PathBuf,
    fixed_account_password: String,
}

impl TestServer {
    /// Start a waddle-server with test configuration on dynamic ports.
    pub fn start() -> Self {
        let bin = env!("CARGO_BIN_EXE_waddle-server");
        let fixed_account_password = format!("ws-test-password-{}", uuid::Uuid::new_v4());

        // Temp file where the server writes its bound HTTP port
        let port_file =
            std::env::temp_dir().join(format!("waddle-test-port-{}", uuid::Uuid::new_v4()));

        let child = Command::new(bin)
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD",
                &fixed_account_password,
            )
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_XMPP_DOMAIN", "localhost")
            .env("WADDLE_XMPP_MAM_DB", ":memory:")
            .env("WADDLE_HTTP_PORT_FILE", &port_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to start waddle-server at {bin}: {e}"));

        // Poll the port file until the server writes it
        let deadline = Instant::now() + SERVER_STARTUP_TIMEOUT;
        let http_port = loop {
            if Instant::now() > deadline {
                panic!("Server failed to write port file within {SERVER_STARTUP_TIMEOUT:?}");
            }
            if let Ok(contents) = std::fs::read_to_string(&port_file) {
                if let Ok(port) = contents.trim().parse::<u16>() {
                    break port;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        // Wait for the port to actually accept connections
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut listening = false;
        while Instant::now() < deadline {
            if TcpStream::connect(format!("127.0.0.1:{http_port}")).is_ok() {
                listening = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !listening {
            panic!("Server failed to accept connections on 127.0.0.1:{http_port} within 5s");
        }

        Self {
            process: child,
            http_port,
            port_file,
            fixed_account_password,
        }
    }

    /// WebSocket URL for the XMPP endpoint.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/xmpp-websocket", self.http_port)
    }

    /// The password for the fixed test account (username: "admin").
    pub fn fixed_account_password(&self) -> &str {
        &self.fixed_account_password
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.port_file);
    }
}

// ---------------------------------------------------------------------------
// WebSocket XMPP client
// ---------------------------------------------------------------------------

/// A WebSocket XMPP client for integration tests.
pub struct WsXmppClient {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    pub full_jid: Option<String>,
}

impl WsXmppClient {
    /// Connect to a WebSocket XMPP endpoint, authenticate, and bind a resource.
    pub async fn connect_and_auth(
        url: &str,
        domain: &str,
        username: &str,
        password: &str,
        resource: &str,
    ) -> Result<Self, String> {
        // Retry connection a few times — under parallel test load the server
        // may need a moment after the port file is written before it starts
        // accepting WebSocket upgrades.
        let mut last_err = String::new();
        for _ in 0..5 {
            match Self::connect(url).await {
                Ok(mut client) => {
                    client.authenticate(domain, username, password).await?;
                    client.bind(resource).await?;
                    return Ok(client);
                }
                Err(e) => {
                    last_err = e;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
        Err(last_err)
    }

    async fn connect(url: &str) -> Result<Self, String> {
        let request = tungstenite::http::Request::builder()
            .uri(url)
            .header("Sec-WebSocket-Protocol", "xmpp")
            .header("Host", "localhost")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .map_err(|e| format!("Failed to build request: {e}"))?;

        let (ws, _) = connect_async(request)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        Ok(Self { ws, full_jid: None })
    }

    async fn authenticate(
        &mut self,
        domain: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        // Open stream
        self.send(&format!(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="{domain}" />"#
        ))
        .await?;
        let _open = self.recv().await?;
        let features = self.recv().await?;
        if !features.contains("SCRAM-SHA-256") {
            return Err(format!("Server does not offer SCRAM-SHA-256: {features}"));
        }

        // Client-first
        let client_nonce = BASE64_STANDARD.encode(rand::random::<[u8; 18]>());
        let client_first_bare = format!("n={username},r={client_nonce}");
        let client_first = format!("n,,{client_first_bare}");
        let auth_b64 = BASE64_STANDARD.encode(client_first.as_bytes());
        self.send(&format!(
            r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl" mechanism="SCRAM-SHA-256">{auth_b64}</auth>"#
        ))
        .await?;

        // Challenge
        let challenge_frame = self.recv().await?;
        let challenge_b64 = extract_element_text(&challenge_frame, "challenge")
            .ok_or_else(|| format!("No challenge received: {challenge_frame}"))?;
        let server_first = String::from_utf8(
            BASE64_STANDARD
                .decode(&challenge_b64)
                .map_err(|e| format!("Bad base64 in challenge: {e}"))?,
        )
        .map_err(|e| format!("Bad UTF-8 in challenge: {e}"))?;

        // Parse server-first
        let params: std::collections::HashMap<&str, &str> = server_first
            .split(',')
            .filter_map(|p| p.split_once('='))
            .collect();
        let combined_nonce = params.get("r").ok_or("Missing nonce")?.to_string();
        let salt = BASE64_STANDARD
            .decode(params.get("s").ok_or("Missing salt")?)
            .map_err(|e| format!("Bad salt: {e}"))?;
        let iterations: u32 = params
            .get("i")
            .ok_or("Missing iterations")?
            .parse()
            .map_err(|e| format!("Bad iterations: {e}"))?;

        // Compute SCRAM proof
        let salted_password = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);
        let channel_binding = BASE64_STANDARD.encode(b"n,,");
        let client_final_without_proof = format!("c={channel_binding},r={combined_nonce}");
        let auth_message =
            format!("{client_first_bare},{server_first},{client_final_without_proof}");
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let client_proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let proof_b64 = BASE64_STANDARD.encode(&client_proof);
        let client_final = format!("{client_final_without_proof},p={proof_b64}");
        let response_b64 = BASE64_STANDARD.encode(client_final.as_bytes());

        self.send(&format!(
            r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">{response_b64}</response>"#
        ))
        .await?;

        let result = self.recv().await?;
        if !result.contains("<success") {
            return Err(format!("SCRAM auth failed: {result}"));
        }

        // Re-open stream after SASL success
        self.send(&format!(
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="{domain}" />"#
        ))
        .await?;
        let _open = self.recv().await?;
        let _features = self.recv().await?;

        Ok(())
    }

    async fn bind(&mut self, resource: &str) -> Result<String, String> {
        let bind_id = format!("bind-{}", uuid::Uuid::new_v4());
        self.send(&format!(
            r#"<iq type="set" id="{bind_id}"><bind xmlns="urn:ietf:params:xml:ns:xmpp-bind"><resource>{resource}</resource></bind></iq>"#
        ))
        .await?;

        let response = self.recv().await?;
        let jid = extract_element_text(&response, "jid")
            .ok_or_else(|| format!("No JID in bind response: {response}"))?;
        self.full_jid = Some(jid.clone());
        Ok(jid)
    }

    pub async fn send(&mut self, xml: &str) -> Result<(), String> {
        self.ws
            .send(tungstenite::Message::Text(xml.to_string()))
            .await
            .map_err(|e| format!("Send failed: {e}"))
    }

    pub async fn recv(&mut self) -> Result<String, String> {
        self.recv_timeout(RECV_TIMEOUT).await
    }

    pub async fn recv_timeout(&mut self, dur: Duration) -> Result<String, String> {
        match timeout(dur, self.ws.next()).await {
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => Ok(text),
            Ok(Some(Ok(other))) => Err(format!("Unexpected message type: {other:?}")),
            Ok(Some(Err(e))) => Err(format!("WebSocket error: {e}")),
            Ok(None) => Err("WebSocket stream ended".to_string()),
            Err(_) => Err("Timeout waiting for message".to_string()),
        }
    }

    /// Receive frames until one matches the predicate. Returns all collected frames.
    pub async fn recv_until<F: Fn(&str) -> bool>(
        &mut self,
        predicate: F,
    ) -> Result<Vec<String>, String> {
        let mut frames = Vec::new();
        loop {
            let frame = self.recv().await?;
            let done = predicate(&frame);
            frames.push(frame);
            if done {
                return Ok(frames);
            }
        }
    }

    /// Receive the first frame matching a predicate, discarding others.
    pub async fn recv_matching<F: Fn(&str) -> bool>(
        &mut self,
        predicate: F,
    ) -> Result<String, String> {
        loop {
            let frame = self.recv().await?;
            if predicate(&frame) {
                return Ok(frame);
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self
            .send(r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#)
            .await;
        let _ = self.ws.close(None).await;
    }
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut output = vec![0u8; 32];
    pbkdf2::pbkdf2::<HmacSha256>(password, salt, iterations, &mut output)
        .expect("PBKDF2 output length is valid");
    output
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

fn extract_element_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start_idx = xml.find(&open)?;
    let rest = &xml[start_idx..];
    let content_start = rest.find('>')? + 1;
    let content = &rest[content_start..];
    let end_idx = content.find(&close)?;
    Some(content[..end_idx].trim().to_string())
}
