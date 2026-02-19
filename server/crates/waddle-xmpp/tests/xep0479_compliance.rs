//! XEP-0479 (XMPP Compliance Suites 2023) orchestration via testcontainers-rs.
//!
//! This test:
//! - provisions temporary TLS certificates,
//! - starts `waddle-server` as a managed child process,
//! - runs the XMPP interop test image via testcontainers,
//! - writes logs/JUnit/summary artifacts for analysis.
//!
//! Default mode is report-only (`best_effort_full`) so compliance failures are
//! reported but do not fail this test. Harness/setup failures still fail.

mod common;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::env;
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use testcontainers::{
    core::{Host, Mount},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};
use tokio_rustls::TlsConnector;

const XMPP_INTEROP_IMAGE: &str = "ghcr.io/xmpp-interop-testing/xmpp_interop_tests";
const XMPP_INTEROP_TAG: &str = "latest";

const DEFAULT_DOMAIN: &str = "localhost";
const DEFAULT_HOST: &str = "host.docker.internal";
const DEFAULT_TIMEOUT_MS: u32 = 10_000;
const DEFAULT_CONTAINER_TIMEOUT_SECS_CORE: u64 = 60 * 20;
const DEFAULT_CONTAINER_TIMEOUT_SECS_FULL: u64 = 60 * 90;
const DEFAULT_CONTAINER_STARTUP_TIMEOUT_SECS: u64 = 60 * 5;
const DEFAULT_ADMIN_USERNAME: &str = "";
const DEFAULT_ADMIN_PASSWORD: &str = "";
const SERVER_READY_TIMEOUT_SECS: u64 = 45;
const CONTAINER_ARTIFACTS_DIR: &str = "/waddle-artifacts";
const CONTAINER_SERVER_CERT: &str = "/waddle-artifacts/server.crt";
const CONTAINER_CACERTS_PATH: &str = "/opt/java/openjdk/lib/security/cacerts";
const CONTAINER_CERT_ALIAS: &str = "waddle-compliance-local-ca";
const INTEROP_COMPLETE_LOG_FILENAME: &str = "completeLog";
const INTEROP_OUTSIDE_LOG_FILENAME: &str = "outsideTestLog";
const INTEROP_TESTS_FILENAME: &str = "tests";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ComplianceProfile {
    BestEffortFull,
    CoreStrict,
    FullStrict,
}

impl ComplianceProfile {
    fn report_only(self) -> bool {
        matches!(self, Self::BestEffortFull)
    }
}

impl FromStr for ComplianceProfile {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "best_effort_full" | "best-effort-full" | "best_effort" | "best-effort" => {
                Ok(Self::BestEffortFull)
            }
            "core_strict" | "core-strict" => Ok(Self::CoreStrict),
            "full_strict" | "full-strict" => Ok(Self::FullStrict),
            other => Err(format!("Unsupported profile '{other}'")),
        }
    }
}

#[derive(Debug, Clone)]
struct ComplianceConfig {
    profile: ComplianceProfile,
    artifact_dir: PathBuf,
    keep_containers: bool,
    domain: String,
    host: String,
    timeout_ms: u32,
    container_timeout_secs: Option<u64>,
    admin_username: String,
    admin_password: String,
    enabled_specs: Vec<String>,
    disabled_specs: Vec<String>,
    enabled_tests: Vec<String>,
    disabled_tests: Vec<String>,
}

impl ComplianceConfig {
    fn from_env() -> Result<Self> {
        let profile = env::var("WADDLE_COMPLIANCE_PROFILE")
            .unwrap_or_else(|_| "best_effort_full".to_string())
            .parse::<ComplianceProfile>()
            .map_err(|e| anyhow::anyhow!(e))?;

        let workspace_root = workspace_root();
        let artifact_dir = match env::var("WADDLE_COMPLIANCE_ARTIFACT_DIR") {
            Ok(path) => {
                let p = PathBuf::from(path);
                if p.is_absolute() {
                    p
                } else {
                    workspace_root.join(p)
                }
            }
            Err(_) => {
                let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
                workspace_root
                    .join("target")
                    .join("compliance-artifacts")
                    .join(ts.to_string())
            }
        };

        Ok(Self {
            profile,
            artifact_dir,
            keep_containers: env_bool("WADDLE_COMPLIANCE_KEEP_CONTAINERS", false),
            domain: env::var("WADDLE_COMPLIANCE_DOMAIN")
                .unwrap_or_else(|_| DEFAULT_DOMAIN.to_string()),
            host: env::var("WADDLE_COMPLIANCE_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string()),
            timeout_ms: env::var("WADDLE_COMPLIANCE_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_MS),
            container_timeout_secs: parse_container_timeout_secs(profile)?,
            admin_username: env::var("WADDLE_COMPLIANCE_ADMIN_USERNAME")
                .unwrap_or_else(|_| DEFAULT_ADMIN_USERNAME.to_string()),
            admin_password: env::var("WADDLE_COMPLIANCE_ADMIN_PASSWORD")
                .unwrap_or_else(|_| DEFAULT_ADMIN_PASSWORD.to_string()),
            enabled_specs: parse_csv_env("WADDLE_COMPLIANCE_ENABLED_SPECS"),
            disabled_specs: parse_csv_env("WADDLE_COMPLIANCE_DISABLED_SPECS"),
            enabled_tests: parse_csv_env("WADDLE_COMPLIANCE_ENABLED_TESTS"),
            disabled_tests: parse_csv_env("WADDLE_COMPLIANCE_DISABLED_TESTS"),
        })
    }

    fn use_service_admin_registration(&self) -> bool {
        !self.admin_username.trim().is_empty() && !self.admin_password.trim().is_empty()
    }

    fn effective_enabled_specs(&self) -> Vec<String> {
        if !self.enabled_specs.is_empty() {
            return self.enabled_specs.clone();
        }

        match self.profile {
            ComplianceProfile::CoreStrict => {
                vec!["RFC6120".into(), "RFC6121".into(), "XEP-0030".into()]
            }
            ComplianceProfile::BestEffortFull | ComplianceProfile::FullStrict => vec![],
        }
    }

    fn effective_disabled_specs(&self) -> Vec<String> {
        if !self.enabled_specs.is_empty() {
            return vec![];
        }

        self.disabled_specs.clone()
    }

    fn effective_enabled_tests(&self) -> Vec<String> {
        self.enabled_tests.clone()
    }

    fn effective_disabled_tests(&self) -> Vec<String> {
        if !self.enabled_tests.is_empty() {
            return vec![];
        }

        self.disabled_tests.clone()
    }
}

fn default_container_timeout_secs(profile: ComplianceProfile) -> u64 {
    match profile {
        ComplianceProfile::CoreStrict => DEFAULT_CONTAINER_TIMEOUT_SECS_CORE,
        ComplianceProfile::BestEffortFull | ComplianceProfile::FullStrict => {
            DEFAULT_CONTAINER_TIMEOUT_SECS_FULL
        }
    }
}

#[derive(Debug)]
struct ArtifactPaths {
    dir: PathBuf,
    server_log: PathBuf,
    interop_stdout: PathBuf,
    interop_stderr: PathBuf,
    interop_command: PathBuf,
    interop_logs_dir: PathBuf,
    interop_complete_log: PathBuf,
    interop_outside_log: PathBuf,
    interop_tests_log: PathBuf,
    junit_xml: PathBuf,
    summary_json: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
}

impl ArtifactPaths {
    fn create(config: &ComplianceConfig) -> Result<Self> {
        fs::create_dir_all(&config.artifact_dir)
            .with_context(|| format!("Creating artifact dir {}", config.artifact_dir.display()))?;

        let interop_logs_dir = config.artifact_dir.join("interop-logs");
        fs::create_dir_all(&interop_logs_dir).with_context(|| {
            format!(
                "Creating interop log mount dir {}",
                interop_logs_dir.display()
            )
        })?;

        Ok(Self {
            dir: config.artifact_dir.clone(),
            server_log: config.artifact_dir.join("waddle-server.log"),
            interop_stdout: config.artifact_dir.join("interop-stdout.log"),
            interop_stderr: config.artifact_dir.join("interop-stderr.log"),
            interop_command: config.artifact_dir.join("interop-command.txt"),
            interop_logs_dir: interop_logs_dir.clone(),
            interop_complete_log: interop_logs_dir.join(INTEROP_COMPLETE_LOG_FILENAME),
            interop_outside_log: interop_logs_dir.join(INTEROP_OUTSIDE_LOG_FILENAME),
            interop_tests_log: interop_logs_dir.join(INTEROP_TESTS_FILENAME),
            junit_xml: interop_logs_dir.join("test-results.xml"),
            summary_json: config.artifact_dir.join("summary.json"),
            cert_path: config.artifact_dir.join("server.crt"),
            key_path: config.artifact_dir.join("server.key"),
        })
    }
}

#[derive(Debug)]
struct ServerProcess {
    child: Child,
}

impl ServerProcess {
    fn start(config: &ComplianceConfig, artifacts: &ArtifactPaths) -> Result<Self> {
        ensure_port_available(5222).context("XMPP port 5222 is not available")?;
        ensure_port_available(3000).context("HTTP port 3000 is not available")?;

        let tls = common::TestTlsCredentials::generate(&config.domain);
        fs::write(&artifacts.cert_path, &tls.cert_pem)
            .with_context(|| format!("Writing cert to {}", artifacts.cert_path.display()))?;
        fs::write(&artifacts.key_path, &tls.key_pem)
            .with_context(|| format!("Writing key to {}", artifacts.key_path.display()))?;

        let binary = resolve_waddle_server_binary()?;
        let server_log = File::create(&artifacts.server_log)
            .with_context(|| format!("Creating {}", artifacts.server_log.display()))?;
        let server_log_err = server_log
            .try_clone()
            .context("Cloning server log handle for stderr")?;

        let db_path = artifacts.dir.join("compliance.db");

        let mut command = Command::new(&binary);
        command
            .current_dir(workspace_root())
            .stdout(Stdio::from(server_log))
            .stderr(Stdio::from(server_log_err))
            .env("RUST_LOG", "warn")
            .env("WADDLE_MODE", "standalone")
            .env("WADDLE_BASE_URL", "http://127.0.0.1:3000")
            .env("WADDLE_DB_PATH", db_path)
            .env("WADDLE_XMPP_ENABLED", "true")
            .env("WADDLE_XMPP_DOMAIN", &config.domain)
            .env("WADDLE_XMPP_C2S_ADDR", "0.0.0.0:5222")
            .env("WADDLE_XMPP_TLS_CERT", &artifacts.cert_path)
            .env("WADDLE_XMPP_TLS_KEY", &artifacts.key_path)
            .env("WADDLE_XMPP_S2S_ENABLED", "false")
            .env("WADDLE_NATIVE_AUTH_ENABLED", "true")
            .env("WADDLE_XMPP_ISR_IN_SASL_SUCCESS", "false")
            .env("WADDLE_REGISTRATION_ENABLED", "true");

        let mut child = command
            .spawn()
            .with_context(|| format!("Starting server binary {}", binary.display()))?;

        wait_for_server_ready(
            &mut child,
            Duration::from_secs(SERVER_READY_TIMEOUT_SECS),
            SocketAddr::from(([127, 0, 0, 1], 5222)),
        )?;

        Ok(Self { child })
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug)]
struct InteropResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct JunitTotals {
    tests: u64,
    failures: u64,
    errors: u64,
    skipped: u64,
}

#[derive(Debug, Clone, Serialize)]
struct InteropProgress {
    tests_started: u64,
    tests_completed: u64,
    tests_passed: u64,
    tests_failed: u64,
    pass_percentage_of_started: f64,
    pass_percentage_of_completed: f64,
    completion_percentage: f64,
    running_test: Option<String>,
    failed_tests: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ComplianceSummary {
    profile: ComplianceProfile,
    report_only: bool,
    domain: String,
    host: String,
    timeout_ms: u32,
    container_timeout_secs: Option<u64>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    duration_secs: f64,
    enabled_specs: Vec<String>,
    disabled_specs: Vec<String>,
    enabled_tests: Vec<String>,
    disabled_tests: Vec<String>,
    container_exit_code: Option<i64>,
    junit: Option<JunitTotals>,
    interop_progress: Option<InteropProgress>,
    compliance_failed: bool,
    artifacts: SummaryArtifacts,
}

#[derive(Debug, Serialize)]
struct SummaryArtifacts {
    root: String,
    server_log: String,
    interop_stdout: String,
    interop_stderr: String,
    interop_command: String,
    interop_complete_log: String,
    interop_outside_log: String,
    interop_tests_log: String,
    junit_xml: String,
    summary_json: String,
}

fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        common::install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_test_writer()
            .try_init();
    });
}

/// Run the end-to-end XEP-0479 harness with managed server + testcontainer.
#[tokio::test]
#[ignore = "Requires Docker and local ports 3000/5222"]
async fn test_xep0479_compliance_managed() -> Result<()> {
    init_tracing();

    let config = ComplianceConfig::from_env()?;
    let artifacts = ArtifactPaths::create(&config)?;

    println!("Compliance profile: {:?}", config.profile);
    println!("Artifact directory: {}", artifacts.dir.display());
    match config.container_timeout_secs {
        Some(timeout_secs) => println!("Interop container timeout (s): {}", timeout_secs),
        None => println!("Interop container timeout (s): none (run until completion)"),
    }

    let enabled_specs = config.effective_enabled_specs();
    let disabled_specs = config.effective_disabled_specs();
    let enabled_tests = config.effective_enabled_tests();
    let disabled_tests = config.effective_disabled_tests();

    let started_at = Utc::now();
    let _server = ServerProcess::start(&config, &artifacts)?;
    if config.use_service_admin_registration() {
        ensure_admin_account(&config, &artifacts)
            .await
            .context("Ensuring interop admin account exists")?;
    } else {
        println!("Admin credentials not configured; using in-band account registration mode.");
    }
    let interop = run_interop_container(
        &config,
        &artifacts,
        &enabled_specs,
        &disabled_specs,
        &enabled_tests,
        &disabled_tests,
    )
    .await?;
    let finished_at = Utc::now();

    if contains_invalid_argument(&interop.stdout) || contains_invalid_argument(&interop.stderr) {
        bail!("Interop image rejected arguments; harness configuration is stale");
    }

    if interop.stdout.contains("sinttest.securityMode=disabled")
        || interop.stderr.contains("sinttest.securityMode=disabled")
    {
        bail!("Interop run used securityMode=disabled; harness must enforce TLS-required mode");
    }

    if interop.stdout.trim().is_empty() && interop.stderr.trim().is_empty() {
        eprintln!(
            "Interop stdout/stderr were empty; relying on mounted logs under {}.",
            artifacts.interop_logs_dir.display()
        );
    }

    let junit = parse_junit_totals(&artifacts.junit_xml)
        .with_context(|| format!("Parsing {}", artifacts.junit_xml.display()))?;
    let interop_progress = parse_interop_progress(&artifacts.interop_complete_log)
        .with_context(|| format!("Parsing {}", artifacts.interop_complete_log.display()))?;

    if junit.is_none() {
        eprintln!(
            "JUnit XML not found at {}; relying on container exit code.",
            artifacts.junit_xml.display()
        );
    }
    if interop_progress.is_none() {
        eprintln!(
            "Interop progress log not found at {}; pass percentages unavailable.",
            artifacts.interop_complete_log.display()
        );
    }

    let junit_failed = junit
        .map(|totals| totals.failures > 0 || totals.errors > 0)
        .unwrap_or(false);
    let progress_failed = interop_progress
        .as_ref()
        .map(|progress| progress_policy_failed(progress, config.profile.report_only()))
        .unwrap_or(false);
    let exit_failed = interop.exit_code.unwrap_or(1) != 0;
    let compliance_failed = junit_failed || progress_failed || exit_failed;

    let summary = ComplianceSummary {
        profile: config.profile,
        report_only: config.profile.report_only(),
        domain: config.domain.clone(),
        host: config.host.clone(),
        timeout_ms: config.timeout_ms,
        container_timeout_secs: config.container_timeout_secs,
        started_at,
        finished_at,
        duration_secs: (finished_at - started_at)
            .to_std()
            .unwrap_or_default()
            .as_secs_f64(),
        enabled_specs: enabled_specs.clone(),
        disabled_specs: disabled_specs.clone(),
        enabled_tests: enabled_tests.clone(),
        disabled_tests: disabled_tests.clone(),
        container_exit_code: interop.exit_code,
        junit,
        interop_progress,
        compliance_failed,
        artifacts: SummaryArtifacts {
            root: artifacts.dir.display().to_string(),
            server_log: artifacts.server_log.display().to_string(),
            interop_stdout: artifacts.interop_stdout.display().to_string(),
            interop_stderr: artifacts.interop_stderr.display().to_string(),
            interop_command: artifacts.interop_command.display().to_string(),
            interop_complete_log: artifacts.interop_complete_log.display().to_string(),
            interop_outside_log: artifacts.interop_outside_log.display().to_string(),
            interop_tests_log: artifacts.interop_tests_log.display().to_string(),
            junit_xml: artifacts.junit_xml.display().to_string(),
            summary_json: artifacts.summary_json.display().to_string(),
        },
    };

    fs::write(
        &artifacts.summary_json,
        serde_json::to_vec_pretty(&summary).context("Encoding summary.json")?,
    )
    .with_context(|| format!("Writing {}", artifacts.summary_json.display()))?;

    println!("Container exit code: {:?}", interop.exit_code);
    println!("JUnit totals: {:?}", summary.junit);
    if let Some(progress) = &summary.interop_progress {
        println!(
            "Interop progress: started={} completed={} passed={} failed={} pass(started)={:.2}% pass(completed)={:.2}%",
            progress.tests_started,
            progress.tests_completed,
            progress.tests_passed,
            progress.tests_failed,
            progress.pass_percentage_of_started,
            progress.pass_percentage_of_completed
        );
        if let Some(running_test) = &progress.running_test {
            println!("Last running test before stop: {}", running_test);
        }
    }
    println!("Summary JSON: {}", artifacts.summary_json.display());
    println!("Server log: {}", artifacts.server_log.display());
    println!("Interop stdout: {}", artifacts.interop_stdout.display());
    println!("Interop stderr: {}", artifacts.interop_stderr.display());

    if compliance_failed && config.profile.report_only() {
        eprintln!("Compliance failures detected (report-only mode): test remains successful.");
        return Ok(());
    }

    if compliance_failed {
        bail!("Compliance suite reported failures in strict mode");
    }

    Ok(())
}

async fn run_interop_container(
    config: &ComplianceConfig,
    artifacts: &ArtifactPaths,
    enabled_specs: &[String],
    disabled_specs: &[String],
    enabled_tests: &[String],
    disabled_tests: &[String],
) -> Result<InteropResult> {
    let java_cmd = build_interop_java_cmd(
        config,
        enabled_specs,
        disabled_specs,
        enabled_tests,
        disabled_tests,
    );
    let shell_cmd = build_interop_shell_cmd(&java_cmd);

    fs::write(
        &artifacts.interop_command,
        format!("sh {}", shell_cmd.join(" ")),
    )
    .with_context(|| format!("Writing {}", artifacts.interop_command.display()))?;

    let mount_source = artifacts
        .interop_logs_dir
        .canonicalize()
        .with_context(|| format!("Canonicalizing {}", artifacts.interop_logs_dir.display()))?;
    let artifacts_mount_source = artifacts
        .dir
        .canonicalize()
        .with_context(|| format!("Canonicalizing {}", artifacts.dir.display()))?;

    let image = GenericImage::new(XMPP_INTEROP_IMAGE, XMPP_INTEROP_TAG)
        .with_entrypoint("sh")
        .with_cmd(shell_cmd)
        .with_host("host.docker.internal", Host::HostGateway)
        .with_mount(Mount::bind_mount(
            mount_source.to_string_lossy().to_string(),
            "/logs",
        ))
        .with_mount(Mount::bind_mount(
            artifacts_mount_source.to_string_lossy().to_string(),
            CONTAINER_ARTIFACTS_DIR,
        ))
        .with_startup_timeout(Duration::from_secs(DEFAULT_CONTAINER_STARTUP_TIMEOUT_SECS));

    let container = image
        .start()
        .await
        .context("Starting interop container with testcontainers-rs")?;

    let exit_code = wait_for_container_exit(
        container.id(),
        config.container_timeout_secs.map(Duration::from_secs),
    )
    .with_context(|| match config.container_timeout_secs {
        Some(timeout_secs) => {
            format!(
                "Interop container exceeded execution timeout ({}s)",
                timeout_secs
            )
        }
        None => "Interop container wait failed in unbounded mode".to_string(),
    })?;

    let stdout = match container.stdout_to_vec().await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(error) => {
            eprintln!("Failed reading interop stdout via testcontainers: {error}");
            String::new()
        }
    };

    let stderr = match container.stderr_to_vec().await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(error) => {
            eprintln!("Failed reading interop stderr via testcontainers: {error}");
            String::new()
        }
    };

    fs::write(&artifacts.interop_stdout, &stdout)
        .with_context(|| format!("Writing {}", artifacts.interop_stdout.display()))?;
    fs::write(&artifacts.interop_stderr, &stderr)
        .with_context(|| format!("Writing {}", artifacts.interop_stderr.display()))?;

    if config.keep_containers {
        println!("Keeping interop container alive: {}", container.id());
        std::mem::forget(container);
    }

    Ok(InteropResult {
        stdout,
        stderr,
        exit_code: Some(exit_code),
    })
}

fn build_interop_java_cmd(
    config: &ComplianceConfig,
    enabled_specs: &[String],
    disabled_specs: &[String],
    enabled_tests: &[String],
    disabled_tests: &[String],
) -> Vec<String> {
    let mut cmd = vec![
        format!("-Dsinttest.service={}", config.domain),
        format!("-Dsinttest.host={}", config.host),
        "-Dsinttest.securityMode=required".to_string(),
        "-Dsinttest.accountRegistration=inBandRegistration".to_string(),
        format!("-Dsinttest.replyTimeout={}", config.timeout_ms),
        "-Dsinttest.enabledConnections=tcp".to_string(),
        "-Dsinttest.dnsResolver=javax".to_string(),
    ];

    if config.use_service_admin_registration() {
        cmd.push(format!(
            "-Dsinttest.adminAccountUsername={}",
            config.admin_username
        ));
        cmd.push(format!(
            "-Dsinttest.adminAccountPassword={}",
            config.admin_password
        ));
    }

    if !enabled_specs.is_empty() {
        cmd.push(format!(
            "-Dsinttest.enabledSpecifications={}",
            enabled_specs.join(",")
        ));
    } else if !disabled_specs.is_empty() {
        cmd.push(format!(
            "-Dsinttest.disabledSpecifications={}",
            disabled_specs.join(",")
        ));
    }

    if !enabled_tests.is_empty() {
        cmd.push(format!(
            "-Dsinttest.enabledTests={}",
            enabled_tests.join(",")
        ));
    } else if !disabled_tests.is_empty() {
        cmd.push(format!(
            "-Dsinttest.disabledTests={}",
            disabled_tests.join(",")
        ));
    }

    cmd.push("-Dsinttest.testRunResultProcessors=org.igniterealtime.smack.inttest.util.StdOutTestRunResultProcessor,org.igniterealtime.smack.inttest.util.JUnitXmlTestRunResultProcessor".to_string());
    cmd.push("-Dsinttest.debugger=org.igniterealtime.smack.inttest.util.ModifiedStandardSinttestDebuggerMetaFactory".to_string());
    cmd.push("-DlogDir=/logs".to_string());
    cmd.push("-jar".to_string());
    cmd.push("/usr/local/sintse/sintse.jar".to_string());

    cmd
}

fn build_interop_shell_cmd(java_args: &[String]) -> Vec<String> {
    let script = format!(
        "keytool -delete -storepass changeit -alias {alias} -keystore {cacerts} >/dev/null 2>&1 || true; \
keytool -importcert -noprompt -storepass changeit -alias {alias} -file {cert} -keystore {cacerts} >/dev/null; \
exec java \"$@\"",
        alias = CONTAINER_CERT_ALIAS,
        cacerts = CONTAINER_CACERTS_PATH,
        cert = CONTAINER_SERVER_CERT,
    );

    let mut cmd = vec!["-lc".to_string(), script, "_".to_string()];
    cmd.extend(java_args.iter().cloned());
    cmd
}

async fn ensure_admin_account(config: &ComplianceConfig, artifacts: &ArtifactPaths) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 5222));
    let mut client = common::RawXmppClient::connect(addr)
        .await
        .with_context(|| format!("Connecting to XMPP server at {addr}"))?;

    let stream_open = format!(
        "<stream:stream to='{domain}' xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' version='1.0'>",
        domain = config.domain
    );
    client
        .send(&stream_open)
        .await
        .context("Sending initial stream header")?;
    let features = client
        .read_until("</stream:features>", Duration::from_secs(10))
        .await
        .context("Reading initial stream features")?;
    if !features.contains("starttls") {
        bail!("Server did not advertise STARTTLS before registration");
    }
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await
        .context("Requesting STARTTLS")?;
    let proceed = client
        .read_until("<proceed", Duration::from_secs(10))
        .await
        .context("Waiting for STARTTLS proceed")?;
    if !proceed.contains("<proceed") {
        bail!("Server did not return STARTTLS proceed");
    }
    client.clear();

    let connector =
        build_tls_connector_from_cert(&artifacts.cert_path).context("Building TLS connector")?;
    client
        .upgrade_tls(connector, &config.domain)
        .await
        .context("Upgrading XMPP socket to TLS")?;

    client
        .send(&stream_open)
        .await
        .context("Sending post-TLS stream header")?;
    let post_tls_features = client
        .read_until("</stream:features>", Duration::from_secs(10))
        .await
        .context("Reading post-TLS stream features")?;
    if !post_tls_features.contains("iq-register") {
        bail!("Registration feature not advertised after TLS");
    }
    client.clear();

    let registration_form_iq = format!(
        "<iq xmlns='jabber:client' type='get' id='reg0' to='{domain}'>\
           <query xmlns='jabber:iq:register'/>\
         </iq>",
        domain = xml_escape(&config.domain)
    );
    client
        .send(&registration_form_iq)
        .await
        .context("Sending XEP-0077 registration form request")?;
    let _ = client
        .read_until("</iq>", Duration::from_secs(10))
        .await
        .context("Reading registration form response")?;
    client.clear();

    let registration_iq = format!(
        "<iq xmlns='jabber:client' type='set' id='reg1' to='{domain}'>\
           <query xmlns='jabber:iq:register'>\
             <username xmlns='jabber:iq:register'>{username}</username>\
             <password xmlns='jabber:iq:register'>{password}</password>\
           </query>\
         </iq>",
        domain = xml_escape(&config.domain),
        username = xml_escape(&config.admin_username),
        password = xml_escape(&config.admin_password),
    );
    client
        .send(&registration_iq)
        .await
        .context("Sending XEP-0077 registration IQ")?;
    let registration_response = client
        .read_until("reg1", Duration::from_secs(10))
        .await
        .context("Reading registration IQ response")?;

    if registration_response.contains("type='result'")
        || registration_response.contains("type=\"result\"")
    {
        let _ = client.send("</stream:stream>").await;
        return Ok(());
    }

    if registration_response.contains("conflict") {
        let _ = client.send("</stream:stream>").await;
        return Ok(());
    }

    bail!(
        "Unexpected registration response: {}",
        registration_response
    )
}

fn build_tls_connector_from_cert(cert_path: &Path) -> Result<TlsConnector> {
    let cert_pem = fs::read(cert_path)
        .with_context(|| format!("Reading certificate {}", cert_path.display()))?;
    let mut reader = Cursor::new(cert_pem);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Parsing PEM certificates for registration client")?;
    if certs.is_empty() {
        bail!("No certificates found in {}", cert_path.display());
    }

    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .context("Adding certificate to registration client root store")?;
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn wait_for_server_ready(child: &mut Child, timeout: Duration, address: SocketAddr) -> Result<()> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait().context("Checking server process status")? {
            bail!("Server exited early with status: {status}");
        }

        if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
            return Ok(());
        }

        if Instant::now() > deadline {
            bail!("Timed out waiting for server to accept connections on {address}");
        }

        thread::sleep(Duration::from_millis(250));
    }
}

fn ensure_port_available(port: u16) -> Result<()> {
    let listener =
        TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).with_context(|| {
            format!(
                "Port {port} is already in use; stop conflicting process before compliance test"
            )
        })?;
    drop(listener);
    Ok(())
}

fn resolve_waddle_server_binary() -> Result<PathBuf> {
    if let Ok(path) = env::var("WADDLE_SERVER_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
        bail!(
            "WADDLE_SERVER_BIN path does not exist: {}",
            candidate.display()
        );
    }

    let root = workspace_root();
    let bin_name = if cfg!(windows) {
        "waddle-server.exe"
    } else {
        "waddle-server"
    };

    let debug_path = root.join("target").join("debug").join(bin_name);
    let release_path = root.join("target").join("release").join(bin_name);

    if env_bool("WADDLE_COMPLIANCE_SKIP_SERVER_BUILD", false) {
        if debug_path.exists() {
            return Ok(debug_path);
        }
        if release_path.exists() {
            return Ok(release_path);
        }

        bail!(
            "WADDLE_COMPLIANCE_SKIP_SERVER_BUILD=true but no binary found at {} or {}. \
Run `cargo build --package waddle-server` or set WADDLE_SERVER_BIN.",
            debug_path.display(),
            release_path.display()
        );
    }

    // Always rebuild to ensure the harness picks up latest local source changes.
    let status = Command::new("cargo")
        .arg("build")
        .arg("--package")
        .arg("waddle-server")
        .current_dir(&root)
        .status()
        .context("Building waddle-server for compliance harness")?;

    if !status.success() {
        bail!("cargo build --package waddle-server failed with status: {status}");
    }

    if debug_path.exists() {
        return Ok(debug_path);
    }

    if release_path.exists() {
        return Ok(release_path);
    }

    bail!(
        "waddle-server binary not found after build at {} or {}",
        debug_path.display(),
        release_path.display()
    );
}

fn wait_for_container_exit(container_id: &str, timeout: Option<Duration>) -> Result<i64> {
    let deadline = timeout.map(|limit| Instant::now() + limit);

    loop {
        let status = inspect_container_status(container_id)?;
        if !status.running {
            return Ok(status.exit_code);
        }

        if let Some(deadline) = deadline {
            if Instant::now() > deadline {
                let timeout_secs = timeout.map(|value| value.as_secs()).unwrap_or(0);
                bail!(
                    "Timed out waiting for interop container {container_id} after {}s",
                    timeout_secs
                );
            }
        }

        thread::sleep(Duration::from_secs(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContainerStatus {
    running: bool,
    exit_code: i64,
}

fn inspect_container_status(container_id: &str) -> Result<ContainerStatus> {
    let output = Command::new("docker")
        .arg("inspect")
        .arg("--format")
        .arg("{{.State.Running}} {{.State.ExitCode}}")
        .arg(container_id)
        .output()
        .context("Inspecting interop container status")?;

    if !output.status.success() {
        bail!("docker inspect failed with status {}", output.status);
    }

    parse_container_status(String::from_utf8_lossy(&output.stdout).trim())
}

fn parse_container_status(output: &str) -> Result<ContainerStatus> {
    let mut parts = output.split_whitespace();
    let running = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing running state in docker inspect output"))?;
    let exit_code = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing exit code in docker inspect output"))?;

    if parts.next().is_some() {
        bail!("Unexpected docker inspect output: '{output}'");
    }

    let running = match running {
        "true" => true,
        "false" => false,
        other => bail!("Unexpected running state value '{other}'"),
    };

    let exit_code = exit_code
        .parse::<i64>()
        .with_context(|| format!("Parsing docker inspect exit code from '{exit_code}'"))?;

    Ok(ContainerStatus { running, exit_code })
}

fn parse_junit_totals(path: &Path) -> Result<Option<JunitTotals>> {
    if !path.exists() {
        return Ok(None);
    }

    let mut xml = String::new();
    File::open(path)
        .with_context(|| format!("Opening JUnit XML {}", path.display()))?
        .read_to_string(&mut xml)
        .with_context(|| format!("Reading JUnit XML {}", path.display()))?;

    let testsuite_start = match xml.find("<testsuite") {
        Some(index) => index,
        None => return Ok(None),
    };
    let testsuite_end = xml[testsuite_start..]
        .find('>')
        .map(|idx| testsuite_start + idx)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Malformed testsuite tag"))?;
    let tag = &xml[testsuite_start..testsuite_end];

    Ok(Some(JunitTotals {
        tests: parse_xml_attr_u64(tag, "tests").unwrap_or(0),
        failures: parse_xml_attr_u64(tag, "failures").unwrap_or(0),
        errors: parse_xml_attr_u64(tag, "errors").unwrap_or(0),
        skipped: parse_xml_attr_u64(tag, "skipped").unwrap_or(0),
    }))
}

fn progress_policy_failed(progress: &InteropProgress, report_only_profile: bool) -> bool {
    if report_only_profile {
        // Report-only gate accepts skipped/incomplete totals as long as completed tests are 100%
        // green and there is at least one completed testcase.
        return progress.tests_failed > 0
            || progress.tests_completed == 0
            || progress.tests_passed < progress.tests_completed;
    }

    // Strict profiles require all started tests to complete with zero failures.
    progress.tests_failed > 0 || progress.tests_completed < progress.tests_started
}

fn parse_interop_progress(path: &Path) -> Result<Option<InteropProgress>> {
    if !path.exists() {
        return Ok(None);
    }

    let mut complete_log = String::new();
    File::open(path)
        .with_context(|| format!("Opening interop completeLog {}", path.display()))?
        .read_to_string(&mut complete_log)
        .with_context(|| format!("Reading interop completeLog {}", path.display()))?;

    if complete_log.trim().is_empty() {
        return Ok(None);
    }

    let mut tests_started = 0_u64;
    let mut tests_passed = 0_u64;
    let mut tests_failed = 0_u64;
    let mut in_flight = Vec::new();
    let mut failed_tests = Vec::new();

    for line in complete_log.lines() {
        let line = line.trim_start();
        if let Some(test_name) = line.strip_prefix("START: ") {
            let test_name = test_name.trim().to_string();
            tests_started += 1;
            in_flight.push(test_name);
            continue;
        }

        if let Some(test_name) = line.strip_prefix("TEST SUCCESSFUL: ") {
            tests_passed += 1;
            remove_in_flight_test(&mut in_flight, test_name.trim());
            continue;
        }

        if let Some(test_name) = line.strip_prefix("TEST FAILED: ") {
            let test_name = test_name.trim().to_string();
            tests_failed += 1;
            failed_tests.push(test_name.clone());
            remove_in_flight_test(&mut in_flight, &test_name);
        }
    }

    let tests_completed = tests_passed + tests_failed;
    if tests_started == 0 && tests_completed == 0 {
        return Ok(None);
    }

    let pass_percentage_of_started = if tests_started > 0 {
        (tests_passed as f64 / tests_started as f64) * 100.0
    } else {
        0.0
    };
    let pass_percentage_of_completed = if tests_completed > 0 {
        (tests_passed as f64 / tests_completed as f64) * 100.0
    } else {
        0.0
    };
    let completion_percentage = if tests_started > 0 {
        (tests_completed as f64 / tests_started as f64) * 100.0
    } else {
        0.0
    };

    Ok(Some(InteropProgress {
        tests_started,
        tests_completed,
        tests_passed,
        tests_failed,
        pass_percentage_of_started,
        pass_percentage_of_completed,
        completion_percentage,
        running_test: in_flight.last().cloned(),
        failed_tests,
    }))
}

fn remove_in_flight_test(in_flight: &mut Vec<String>, finished_test: &str) {
    if let Some(index) = in_flight.iter().position(|entry| entry == finished_test) {
        in_flight.remove(index);
    }
}

fn parse_xml_attr_u64(tag: &str, attr: &str) -> Option<u64> {
    parse_xml_attr(tag, attr)?.parse::<u64>().ok()
}

fn parse_xml_attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let needle = format!("{attr}={quote}");
        if let Some(start) = tag.find(&needle) {
            let value_start = start + needle.len();
            let value_end = tag[value_start..].find(quote)? + value_start;
            return Some(&tag[value_start..value_end]);
        }
    }

    None
}

fn contains_invalid_argument(output: &str) -> bool {
    output.contains("Error: Invalid argument")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn parse_csv_env(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        ),
        Err(_) => default,
    }
}

fn parse_container_timeout_secs(profile: ComplianceProfile) -> Result<Option<u64>> {
    let default_timeout = default_container_timeout_secs(profile);
    let raw_value = match env::var("WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS") {
        Ok(value) => value,
        Err(_) => return Ok(Some(default_timeout)),
    };

    let normalized = raw_value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(Some(default_timeout));
    }

    if matches!(
        normalized.as_str(),
        "0" | "none" | "off" | "unbounded" | "infinite" | "no-limit"
    ) {
        return Ok(None);
    }

    let timeout_secs = normalized.parse::<u64>().with_context(|| {
        format!(
            "Parsing WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS from '{}'",
            raw_value
        )
    })?;

    if timeout_secs == 0 {
        return Ok(None);
    }

    Ok(Some(timeout_secs))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_profile_parse() {
        assert_eq!(
            "best_effort_full".parse::<ComplianceProfile>().unwrap(),
            ComplianceProfile::BestEffortFull
        );
        assert_eq!(
            "core-strict".parse::<ComplianceProfile>().unwrap(),
            ComplianceProfile::CoreStrict
        );
        assert_eq!(
            "full_strict".parse::<ComplianceProfile>().unwrap(),
            ComplianceProfile::FullStrict
        );
        assert!("unknown".parse::<ComplianceProfile>().is_err());
    }

    #[test]
    fn test_parse_xml_attr_u64() {
        let tag = "<testsuite tests=\"10\" failures='2' errors=\"1\" skipped=\"3\">";
        assert_eq!(parse_xml_attr_u64(tag, "tests"), Some(10));
        assert_eq!(parse_xml_attr_u64(tag, "failures"), Some(2));
        assert_eq!(parse_xml_attr_u64(tag, "errors"), Some(1));
        assert_eq!(parse_xml_attr_u64(tag, "skipped"), Some(3));
        assert_eq!(parse_xml_attr_u64(tag, "missing"), None);
    }

    #[test]
    fn test_parse_container_status() {
        assert_eq!(
            parse_container_status("true 0").unwrap(),
            ContainerStatus {
                running: true,
                exit_code: 0,
            }
        );
        assert_eq!(
            parse_container_status("false 137").unwrap(),
            ContainerStatus {
                running: false,
                exit_code: 137,
            }
        );
        assert!(parse_container_status("not-a-bool 0").is_err());
        assert!(parse_container_status("true").is_err());
        assert!(parse_container_status("true 0 extra").is_err());
    }

    #[test]
    fn test_parse_container_timeout_secs() {
        let env_name = "WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS";
        let previous = env::var(env_name).ok();

        env::remove_var(env_name);
        assert_eq!(
            parse_container_timeout_secs(ComplianceProfile::BestEffortFull).unwrap(),
            Some(DEFAULT_CONTAINER_TIMEOUT_SECS_FULL)
        );

        env::set_var(env_name, "7200");
        assert_eq!(
            parse_container_timeout_secs(ComplianceProfile::BestEffortFull).unwrap(),
            Some(7200)
        );

        env::set_var(env_name, "0");
        assert_eq!(
            parse_container_timeout_secs(ComplianceProfile::BestEffortFull).unwrap(),
            None
        );

        env::set_var(env_name, "none");
        assert_eq!(
            parse_container_timeout_secs(ComplianceProfile::BestEffortFull).unwrap(),
            None
        );

        env::set_var(env_name, "invalid");
        assert!(parse_container_timeout_secs(ComplianceProfile::BestEffortFull).is_err());

        match previous {
            Some(value) => env::set_var(env_name, value),
            None => env::remove_var(env_name),
        }
    }

    #[test]
    fn test_env_bool_parsing() {
        let env_name = "WADDLE_COMPLIANCE_SKIP_SERVER_BUILD";
        let previous = env::var(env_name).ok();

        env::remove_var(env_name);
        assert!(!env_bool(env_name, false));
        assert!(env_bool(env_name, true));

        env::set_var(env_name, "true");
        assert!(env_bool(env_name, false));

        env::set_var(env_name, "1");
        assert!(env_bool(env_name, false));

        env::set_var(env_name, "false");
        assert!(!env_bool(env_name, true));

        match previous {
            Some(value) => env::set_var(env_name, value),
            None => env::remove_var(env_name),
        }
    }

    #[test]
    fn test_resolve_waddle_server_binary_rejects_invalid_override_path() {
        let server_bin_env = "WADDLE_SERVER_BIN";
        let skip_build_env = "WADDLE_COMPLIANCE_SKIP_SERVER_BUILD";
        let previous_server_bin = env::var(server_bin_env).ok();
        let previous_skip_build = env::var(skip_build_env).ok();

        env::set_var(
            server_bin_env,
            "/tmp/waddle-server-does-not-exist-for-compliance-test",
        );
        env::remove_var(skip_build_env);

        let error = resolve_waddle_server_binary().expect_err("invalid server bin should fail");
        assert!(
            error
                .to_string()
                .contains("WADDLE_SERVER_BIN path does not exist"),
            "expected clear invalid-path error, got: {error}"
        );

        match previous_server_bin {
            Some(value) => env::set_var(server_bin_env, value),
            None => env::remove_var(server_bin_env),
        }
        match previous_skip_build {
            Some(value) => env::set_var(skip_build_env, value),
            None => env::remove_var(skip_build_env),
        }
    }

    #[test]
    fn test_resolve_waddle_server_binary_honors_override_path_when_present() {
        let server_bin_env = "WADDLE_SERVER_BIN";
        let skip_build_env = "WADDLE_COMPLIANCE_SKIP_SERVER_BUILD";
        let previous_server_bin = env::var(server_bin_env).ok();
        let previous_skip_build = env::var(skip_build_env).ok();

        let temp_path = std::env::temp_dir().join(format!(
            "waddle-server-override-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::write(&temp_path, b"#!/bin/sh\n").expect("write temp server bin");
        env::set_var(server_bin_env, temp_path.to_string_lossy().to_string());
        env::set_var(skip_build_env, "true");

        let resolved =
            resolve_waddle_server_binary().expect("existing override server bin should resolve");
        assert_eq!(resolved, temp_path);

        let _ = fs::remove_file(&temp_path);

        match previous_server_bin {
            Some(value) => env::set_var(server_bin_env, value),
            None => env::remove_var(server_bin_env),
        }
        match previous_skip_build {
            Some(value) => env::set_var(skip_build_env, value),
            None => env::remove_var(skip_build_env),
        }
    }

    #[test]
    fn test_parse_interop_progress() {
        let path = std::env::temp_dir().join(format!(
            "xep0479-completeLog-{}-{}.txt",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        fs::write(
            &path,
            "START: Alpha.testOne\n\
             TEST SUCCESSFUL: Alpha.testOne\n\
             START: Alpha.testTwo\n\
             TEST FAILED: Alpha.testTwo\n\
             START: Alpha.testThree\n",
        )
        .unwrap();

        let progress = parse_interop_progress(&path).unwrap().unwrap();
        assert_eq!(progress.tests_started, 3);
        assert_eq!(progress.tests_completed, 2);
        assert_eq!(progress.tests_passed, 1);
        assert_eq!(progress.tests_failed, 1);
        assert_eq!(progress.running_test.as_deref(), Some("Alpha.testThree"));
        assert_eq!(progress.failed_tests, vec!["Alpha.testTwo".to_string()]);
        assert!((progress.pass_percentage_of_started - 33.3333).abs() < 0.1);
        assert!((progress.pass_percentage_of_completed - 50.0).abs() < 0.1);

        let _ = fs::remove_file(path);
    }

    fn sample_config() -> ComplianceConfig {
        ComplianceConfig {
            profile: ComplianceProfile::BestEffortFull,
            artifact_dir: PathBuf::from("/tmp/interop-tests"),
            keep_containers: false,
            domain: "localhost".to_string(),
            host: "127.0.0.1".to_string(),
            timeout_ms: 5000,
            container_timeout_secs: None,
            admin_username: String::new(),
            admin_password: String::new(),
            enabled_specs: vec![],
            disabled_specs: vec![],
            enabled_tests: vec![],
            disabled_tests: vec![],
        }
    }

    #[test]
    fn test_build_interop_java_cmd_includes_enabled_tests() {
        let config = sample_config();
        let cmd = build_interop_java_cmd(
            &config,
            &[],
            &[],
            &["RFC6121Section8_5_2_1_1_MessageIntegrationTest".to_string()],
            &[],
        );

        assert!(cmd
            .iter()
            .any(|arg| arg
                == "-Dsinttest.enabledTests=RFC6121Section8_5_2_1_1_MessageIntegrationTest"));
        assert!(!cmd
            .iter()
            .any(|arg| arg.starts_with("-Dsinttest.disabledTests=")));
    }

    #[test]
    fn test_build_interop_java_cmd_enabled_tests_take_precedence() {
        let config = sample_config();
        let cmd = build_interop_java_cmd(&config, &[], &[], &["A".to_string()], &["B".to_string()]);

        assert!(cmd.iter().any(|arg| arg == "-Dsinttest.enabledTests=A"));
        assert!(!cmd.iter().any(|arg| arg == "-Dsinttest.disabledTests=B"));
    }

    #[test]
    fn test_progress_policy_failed_report_only_allows_skipped() {
        let progress = InteropProgress {
            tests_started: 10,
            tests_completed: 9,
            tests_passed: 9,
            tests_failed: 0,
            pass_percentage_of_started: 90.0,
            pass_percentage_of_completed: 100.0,
            completion_percentage: 90.0,
            running_test: Some("Foo.bar".to_string()),
            failed_tests: vec![],
        };

        assert!(!progress_policy_failed(&progress, true));
        assert!(progress_policy_failed(&progress, false));
    }
}
