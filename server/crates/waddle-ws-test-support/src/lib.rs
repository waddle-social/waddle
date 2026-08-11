//! WebSocket XMPP test client and test server harness.
//!
//! - `TestServer` spawns the `waddle-server` binary with test env vars and
//!   waits for it to become ready.
//! - `WsXmppClient` connects via WebSocket and authenticates with SCRAM-SHA-256.
//!
//! This lives in its own crate (rather than a `tests/ws_common/mod.rs`
//! shared module) so that every integration-test binary links the same
//! compiled library: helpers unused by one particular test binary are
//! still reachable public API of this crate, so per-binary `dead_code`
//! warnings cannot fire and no lint suppressions are needed.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite};

/// Re-exported so suites can match on raw frames from
/// [`WsXmppClient::recv_raw_timeout`] without a direct dependency.
pub use tokio_tungstenite::tungstenite as raw_ws;

type HmacSha256 = Hmac<Sha256>;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);
pub const TEST_GIT_SHA: &str = "feedface1234567890abcdef";
// Each test spawns a fresh waddle-server binary; startup includes
// ephemeral cert generation (CPU-bound ring keygen) before listeners
// bind. Under `cargo test --test-threads=N` several servers race for
// CPU and one can miss a tight budget on slower CI runners. Use a longer
// budget to reduce CI flake during full-workspace test runs.
const SERVER_STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

/// Locate the `waddle-server` binary produced by the current build.
///
/// The harness lives in a separate test-support crate, so the
/// `CARGO_BIN_EXE_waddle-server` compile-time env (which cargo injects
/// only into waddle-server's own test targets) is not available here.
/// Integration-test executables run from `<target>/<profile>/deps/`,
/// and cargo places package binaries one directory up. Running the
/// tests through cargo (`cargo test -p waddle-server`, `cargo test
/// --workspace`) always builds the binary before the tests execute.
fn waddle_server_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current test executable path");
    let profile_dir = exe
        .parent() // <target>/<profile>/deps
        .and_then(std::path::Path::parent) // <target>/<profile>
        .expect("test executable directory layout");
    let bin = profile_dir.join(format!("waddle-server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.exists(),
        "waddle-server binary not found at {}; run the tests via cargo \
         (e.g. `cargo test -p waddle-server`) so the binary is built first",
        bin.display()
    );
    bin
}

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
    upload_dir: std::path::PathBuf,
    fixed_account_password: String,
    test_profile_publish_token: String,
}

impl TestServer {
    /// Start a waddle-server with test configuration on dynamic ports.
    pub fn start() -> Self {
        Self::start_with_extra_accounts(&[])
    }

    /// Start a waddle-server and seed additional native users.
    pub fn start_with_extra_accounts(extra_accounts: &[(&str, &str)]) -> Self {
        Self::spawn(extra_accounts, None, None, &[])
    }

    /// Start a waddle-server pointed at a persistent SQLite file. Used by tests
    /// that need state to survive a server restart (e.g. XEP-0237 T5).
    /// `database_url` is passed verbatim as `WADDLE_DATABASE_URL`, e.g.
    /// `format!("sqlite://{}?mode=rwc", path.display())`.
    pub fn start_persistent_with_extra_accounts(
        database_url: &str,
        extra_accounts: &[(&str, &str)],
    ) -> Self {
        Self::spawn(extra_accounts, Some(database_url.to_string()), None, &[])
    }

    /// Start a waddle-server pointed at a persistent SQLite MAM database,
    /// while leaving the user/inbox/pubsub stores in-memory. Used by tests
    /// that need to exercise the production-shaped MAM read/write path
    /// against a real on-disk SQLite backend (rather than the
    /// `sqlite::memory:` shortcut the rest of the harness uses).
    pub fn start_with_persistent_mam(mam_database_url: &str) -> Self {
        Self::spawn(&[], None, Some(mam_database_url.to_string()), &[])
    }

    /// Start a waddle-server with additional environment variables
    /// merged into the spawn. The harness strips parent `WADDLE_*`
    /// envs to make tests deterministic; this hook is the only path
    /// for tests that need a specific `WADDLE_*` or feature-toggling
    /// env present (e.g. `LIVEKIT_*` for A/V call tests).
    pub fn start_with_extra_envs(
        extra_accounts: &[(&str, &str)],
        extra_envs: &[(&str, &str)],
    ) -> Self {
        Self::spawn(extra_accounts, None, None, extra_envs)
    }

    /// Combine persistent SQLite with extra `WADDLE_*` env vars. Used
    /// by tests that need to point the harness at a fixed sqlite file
    /// AND tune janitor intervals (e.g. `WADDLE_NOTIFICATION_OUTBOX_JANITOR_INTERVAL`).
    pub fn start_persistent_with_extra_envs(
        database_url: &str,
        extra_accounts: &[(&str, &str)],
        extra_envs: &[(&str, &str)],
    ) -> Self {
        Self::spawn(
            extra_accounts,
            Some(database_url.to_string()),
            None,
            extra_envs,
        )
    }

    fn spawn(
        extra_accounts: &[(&str, &str)],
        database_url: Option<String>,
        mam_database_url: Option<String>,
        extra_envs: &[(&str, &str)],
    ) -> Self {
        let bin = waddle_server_bin();
        let fixed_account_password = format!("ws-test-password-{}", uuid::Uuid::new_v4());
        let test_profile_publish_token = uuid::Uuid::new_v4().to_string();
        let extra_accounts_env = extra_accounts
            .iter()
            .map(|(username, password)| format!("{username}:{password}"))
            .collect::<Vec<_>>()
            .join(",");
        // A caller-supplied database URL (global OR MAM) means a DURABLE
        // store (persistent SQLite file), which the lineage attestation gate
        // (#1652) refuses to serve un-enrolled. The harness enrolls it under
        // one fixed deployment UUID so restart-persistence tests re-verify
        // the same identity across server generations. The in-memory
        // defaults are classified ephemeral and need neither. Callers can
        // override both via `extra_envs` (later `env()` calls win).
        let durable_database = database_url.is_some() || mam_database_url.is_some();
        let database_url = database_url.unwrap_or_else(|| "sqlite::memory:".to_string());

        // Temp file where the server writes its bound HTTP port
        let port_file =
            std::env::temp_dir().join(format!("waddle-test-port-{}", uuid::Uuid::new_v4()));
        let upload_dir =
            std::env::temp_dir().join(format!("waddle-test-uploads-{}", uuid::Uuid::new_v4()));

        let mut command = Command::new(&bin);
        for (key, _) in std::env::vars() {
            if key.starts_with("WADDLE_") || key.starts_with("OTEL_") {
                command.env_remove(key);
            }
        }
        command
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD",
                &fixed_account_password,
            )
            .env("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS", extra_accounts_env)
            .env(
                "WADDLE_TEST_PROFILE_PUBLISH_TOKEN",
                &test_profile_publish_token,
            )
            // The fixed test account ("admin") is always provisioned as a
            // server owner so integration tests can exercise owner-gated
            // operations (MUC creation, Space node creation, etc.)
            .env("WADDLE_SERVER_OWNER_LOCALPARTS", "admin")
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_XMPP_DOMAIN", "localhost")
            .env("WADDLE_DB_DRIVER", "sqlite")
            .env("WADDLE_DATABASE_URL", &database_url);
        if durable_database {
            command
                .env(
                    "WADDLE_DEPLOYMENT_UUID",
                    "018f47b2-4b2e-7a3a-9a4c-52a5a6a97e57",
                )
                .env("WADDLE_DB_LINEAGE_ACTION", "enroll");
        }
        command
            .env("WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY", "true")
            .env("WADDLE_XMPP_MAM_ALLOW_IN_MEMORY", "true")
            .env(
                "WADDLE_XMPP_MAM_DATABASE_URL",
                mam_database_url.as_deref().unwrap_or("sqlite::memory:"),
            )
            .env("WADDLE_XMPP_INBOX_DATABASE_URL", "sqlite::memory:")
            .env("WADDLE_XMPP_SM_DATABASE_URL", "sqlite::memory:")
            .env(
                "WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL",
                "sqlite::memory:",
            )
            .env("WADDLE_XMPP_PUBSUB_DATABASE_URL", "sqlite::memory:")
            .env("WADDLE_UPLOAD_DIR", &upload_dir)
            .env("WADDLE_GIT_SHA", TEST_GIT_SHA)
            .env(
                "WADDLE_SESSION_KEY",
                "integration-test-session-key-32-bytes-long",
            )
            .env(
                "WADDLE_OCCUPANT_ID_SECRET",
                "integration-test-occupant-id-secret-32-bytes-long",
            )
            .env("WADDLE_HTTP_PORT_FILE", &port_file);
        for (key, value) in extra_envs {
            command.env(*key, *value);
        }
        if std::env::var("WADDLE_TEST_SERVER_STDIO").as_deref() == Ok("inherit") {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to start waddle-server at {}: {e}", bin.display()));

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
            upload_dir,
            fixed_account_password,
            test_profile_publish_token,
        }
    }

    /// Token the harness generated to authenticate against the
    /// test-only `/api/test/profile-publish` route. Pass it as the
    /// `X-Waddle-Test-Token` header value.
    pub fn test_profile_publish_token(&self) -> &str {
        &self.test_profile_publish_token
    }

    /// WebSocket URL for the XMPP endpoint.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.http_port)
    }

    /// Base HTTP URL (no path). Useful for hitting test-only routes
    /// like `/api/test/profile-publish` that the harness gates on
    /// `WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true`.
    pub fn http_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    /// The password for the fixed test account (username: "admin").
    pub fn fixed_account_password(&self) -> &str {
        &self.fixed_account_password
    }

    /// Send SIGTERM to the server process — the real deploy-time
    /// graceful-shutdown trigger (issue #1091). Sent via the raw
    /// syscall so the suite has no external `kill` binary dependency.
    #[allow(dead_code)]
    pub fn send_sigterm(&self) {
        let pid = self.process.id() as libc::pid_t;
        // SAFETY: pid is a live child owned by this TestServer (reaped
        // only in Drop), so the signal cannot hit a recycled pid.
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(rc, 0, "kill(SIGTERM) failed for pid {pid}");
    }

    /// Wait for the server process to exit on its own, polling
    /// `try_wait`. Returns `true` if it exited within `timeout`.
    #[allow(dead_code)]
    pub async fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            match self.process.try_wait() {
                Ok(Some(_status)) => return true,
                Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                Err(error) => panic!("try_wait on waddle-server: {error}"),
            }
        }
        false
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.port_file);
        let _ = std::fs::remove_dir_all(&self.upload_dir);
    }
}

// ---------------------------------------------------------------------------
// WebSocket XMPP client
// ---------------------------------------------------------------------------

/// A WebSocket XMPP client for integration tests.
pub struct WsXmppClient {
    /// `pub(crate)` so suites with frame-level needs (the RFC 7395
    /// keepalive tests observe raw `Ping`/`Close` control frames,
    /// which [`Self::recv_timeout`] deliberately treats as errors)
    /// can poll the stream directly with their own local helpers —
    /// keeping this shared harness free of per-suite methods that
    /// would be dead code in every other test binary.
    pub(crate) ws: tokio_tungstenite::WebSocketStream<
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

    pub async fn connect(url: &str) -> Result<Self, String> {
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

    pub async fn authenticate(
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

    pub async fn bind(&mut self, resource: &str) -> Result<String, String> {
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
            .send(tungstenite::Message::Text(xml.to_string().into()))
            .await
            .map_err(|e| format!("Send failed: {e}"))
    }

    pub async fn recv(&mut self) -> Result<String, String> {
        self.recv_timeout(RECV_TIMEOUT).await
    }

    /// Receive one raw WebSocket frame — control frames included.
    ///
    /// Suites observing server-initiated `Ping`/`Close` frames need the
    /// raw stream, which `recv_timeout` deliberately treats as errors.
    /// Polling also lets tokio-tungstenite flush its automatic `Pong`
    /// replies, so a client driven by this helper behaves like a
    /// healthy browser. `Ok(None)` means the stream ended.
    pub async fn recv_raw_timeout(
        &mut self,
        dur: Duration,
    ) -> Result<Option<tungstenite::Message>, String> {
        match timeout(dur, self.ws.next()).await {
            Ok(Some(Ok(message))) => Ok(Some(message)),
            Ok(Some(Err(e))) => Err(format!("WebSocket error: {e}")),
            Ok(None) => Ok(None),
            Err(_) => Err("Timeout waiting for raw frame".to_string()),
        }
    }

    pub async fn recv_timeout(&mut self, dur: Duration) -> Result<String, String> {
        match timeout(dur, self.ws.next()).await {
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => Ok(text.to_string()),
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

    /// Receive the first frame matching a predicate under one overall
    /// budget instead of the per-frame [`RECV_TIMEOUT`].
    ///
    /// For load-sensitive multi-node suites (issue #1627): the budget
    /// spans the whole wait, and a timeout error carries every frame
    /// that was discarded while waiting, so a genuinely misrouted
    /// stanza is distinguishable from slow delivery on a saturated
    /// runner.
    pub async fn recv_matching_within<F: Fn(&str) -> bool>(
        &mut self,
        budget: Duration,
        predicate: F,
    ) -> Result<String, String> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut skipped: Vec<String> = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "Timeout waiting for matching frame after {budget:?}; \
                     {} non-matching frames were discarded: {skipped:?}",
                    skipped.len()
                ));
            }
            match self.recv_timeout(remaining).await {
                Ok(frame) => {
                    if predicate(&frame) {
                        return Ok(frame);
                    }
                    skipped.push(frame);
                }
                Err(error) => {
                    return Err(format!(
                        "{error} (budget {budget:?}; {} non-matching frames \
                         were discarded: {skipped:?})",
                        skipped.len()
                    ));
                }
            }
        }
    }

    pub async fn close(mut self) -> Result<(), String> {
        let close =
            xmpp_parsers::minidom::Element::builder("close", "urn:ietf:params:xml:ns:xmpp-framing")
                .build();
        let mut close_xml = Vec::new();
        close
            .write_to(&mut close_xml)
            .map_err(|error| format!("XMPP close serialization failed: {error}"))?;
        let close_xml = String::from_utf8(close_xml)
            .map_err(|error| format!("XMPP close serialization emitted invalid UTF-8: {error}"))?;
        self.send(&close_xml).await?;
        timeout(Duration::from_secs(2), async {
            while let Some(message) = self.ws.next().await {
                match message {
                    Ok(tungstenite::Message::Text(text)) if text.contains("<close") => {
                        return Ok(());
                    }
                    Ok(tungstenite::Message::Text(text))
                        if text.contains("<stream:error") =>
                    {
                        // A stream-level error during close is a protocol
                        // violation we want the test to surface, not a
                        // benign in-flight stanza we should drain.
                        return Err(format!(
                            "Stream error during close handshake: {text}"
                        ));
                    }
                    Ok(tungstenite::Message::Text(text))
                        if text.starts_with("<message")
                            || text.starts_with("<presence")
                            || text.starts_with("<iq") =>
                    {
                        // In-flight stanzas can still arrive between our
                        // `</close>` and the server's ack — for example,
                        // an `unavailable` MUC presence broadcast from
                        // another occupant whose own teardown is being
                        // processed concurrently. The XMPP spec doesn't
                        // forbid the server from delivering already-queued
                        // stanzas during the close handshake, so drain
                        // them rather than failing the scenario.
                        continue;
                    }
                    Ok(tungstenite::Message::Text(text)) => {
                        // Unknown text frame that isn't a close ack, a
                        // known stanza, or a stream:error. Surface it so
                        // unexpected protocol behavior fails loudly.
                        return Err(format!(
                            "Unexpected text frame while waiting for XMPP close acknowledgement: {text}"
                        ));
                    }
                    Ok(tungstenite::Message::Close(_)) => {
                        return Err("WebSocket closed before XMPP close acknowledgement".into());
                    }
                    Err(error) => return Err(format!("WebSocket close receive failed: {error}")),
                    Ok(_) => {}
                }
            }
            Err("WebSocket ended before XMPP close acknowledgement".into())
        })
        .await
        .map_err(|_| "Timed out waiting for XMPP close acknowledgement".to_string())??;
        self.ws
            .close(None)
            .await
            .map_err(|error| format!("WebSocket close send failed: {error}"))?;
        timeout(Duration::from_secs(2), async {
            while let Some(message) = self.ws.next().await {
                match message {
                    Ok(tungstenite::Message::Close(_)) => return Ok(()),
                    Ok(tungstenite::Message::Text(text)) if text.contains("<stream:error") => {
                        return Err(format!(
                            "Stream error after WebSocket close request: {text}"
                        ));
                    }
                    Ok(tungstenite::Message::Text(text))
                        if text.starts_with("<message")
                            || text.starts_with("<presence")
                            || text.starts_with("<iq") =>
                    {
                        // Same rationale as above: any stanza arriving
                        // after our `<close/>` ack but before the WS
                        // close frame is an in-flight reflection of
                        // another occupant's teardown — drain.
                        continue;
                    }
                    Ok(tungstenite::Message::Text(text)) => {
                        return Err(format!(
                            "Unexpected text frame after WebSocket close request: {text}"
                        ));
                    }
                    // Once the XMPP close ack has been received and the
                    // WebSocket close frame has been sent, a peer-side TCP
                    // reset is equivalent to the connection being gone.
                    Err(_) => return Ok(()),
                    Ok(_) => {}
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| "Timed out waiting for WebSocket close".to_string())?
    }
}

pub async fn disco_info_query(
    client: &mut WsXmppClient,
    to: &str,
    id: &str,
) -> Result<String, String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}" to="{to}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await?;
    client.recv_matching(|frame| frame.contains(id)).await
}

pub async fn version_query(
    client: &mut WsXmppClient,
    to: &str,
    id: &str,
) -> Result<String, String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}" to="{to}"><query xmlns="jabber:iq:version"/></iq>"#
        ))
        .await?;
    client.recv_matching(|frame| frame.contains(id)).await
}

pub async fn entity_time_query(
    client: &mut WsXmppClient,
    to: &str,
    id: &str,
) -> Result<String, String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}" to="{to}"><time xmlns="urn:xmpp:time"/></iq>"#
        ))
        .await?;
    client.recv_matching(|frame| frame.contains(id)).await
}

pub async fn last_activity_query(
    client: &mut WsXmppClient,
    to: &str,
    id: &str,
) -> Result<String, String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}" to="{to}"><query xmlns="jabber:iq:last"/></iq>"#
        ))
        .await?;
    client.recv_matching(|frame| frame.contains(id)).await
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

pub fn extract_element_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start_idx = xml.find(&open)?;
    let rest = &xml[start_idx..];
    let content_start = rest.find('>')? + 1;
    let content = &rest[content_start..];
    let end_idx = content.find(&close)?;
    Some(content[..end_idx].trim().to_string())
}

pub fn extract_attr_after(xml: &str, marker: &str, attr: &str) -> Option<String> {
    let start = xml.find(marker)?;
    let tail = &xml[start..];
    let double = format!("{attr}=\"");
    if let Some(attr_start) = tail.find(&double).map(|idx| idx + double.len()) {
        let rest = &tail[attr_start..];
        return rest.find('"').map(|end| rest[..end].to_string());
    }
    let single = format!("{attr}='");
    if let Some(attr_start) = tail.find(&single).map(|idx| idx + single.len()) {
        let rest = &tail[attr_start..];
        return rest.find('\'').map(|end| rest[..end].to_string());
    }
    None
}
