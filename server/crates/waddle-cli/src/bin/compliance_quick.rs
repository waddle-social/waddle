use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use testcontainers::{
    core::{Host, Mount},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

const DEFAULT_CAAS_REPO: &str = "https://codeberg.org/iNPUTmice/caas.git";
const DEFAULT_CAAS_REF: &str = "master";
const DEFAULT_TIMEOUT_SECS: u64 = 60 * 30;
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 60 * 5;

const CAAS_IMAGE: &str = "maven";
const CAAS_TAG: &str = "3.9-eclipse-temurin-17";

const CONTAINER_REPO_DIR: &str = "/workspace/caas";
const CONTAINER_ARTIFACT_DIR: &str = "/waddle-artifacts";

#[derive(Debug, Parser)]
#[command(
    name = "compliance-quick",
    version,
    about = "Run CAAS quick XMPP compliance checks via testcontainers"
)]
struct Args {
    /// JID to test with (example: admin@example.com)
    #[arg(long)]
    jid: String,
    /// Password for the JID
    #[arg(long)]
    password: String,
    /// Artifact directory for logs and summary.json
    #[arg(long)]
    artifact_dir: Option<String>,
    /// CAAS git ref (branch/tag/commit)
    #[arg(long, default_value = DEFAULT_CAAS_REF)]
    caas_ref: String,
    /// Optional CAAS repository URL override
    #[arg(long, default_value = DEFAULT_CAAS_REPO)]
    caas_repo: String,
    /// Container execution timeout in seconds; 0 means unbounded
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
    /// Keep the CAAS container after completion
    #[arg(long, default_value_t = false)]
    keep_container: bool,
    /// Print extra progress messages
    #[arg(long, default_value_t = false)]
    verbose: bool,
    /// XMPP host CAAS should connect to from inside the container
    #[arg(long, default_value = "host.docker.internal")]
    xmpp_host: String,
    /// XMPP port CAAS should connect to
    #[arg(long, default_value_t = 5222)]
    xmpp_port: u16,
    /// Optional HTTP base URL for XEP-0156 host-meta checks in CAAS quick mode
    #[arg(long)]
    host_meta_base_url: Option<String>,
    /// Enable STARTTLS fallback for XEP-0368 in local quick mode
    #[arg(long, default_value_t = false)]
    xep0368_starttls_fallback: bool,
}

#[derive(Debug, Clone)]
struct ArtifactPaths {
    dir: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
    command_log: PathBuf,
    summary_json: PathBuf,
    repo_checkout: PathBuf,
}

impl ArtifactPaths {
    fn create(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("Creating {}", dir.display()))?;
        Ok(Self {
            stdout_log: dir.join("caas-stdout.log"),
            stderr_log: dir.join("caas-stderr.log"),
            command_log: dir.join("caas-command.sh"),
            summary_json: dir.join("summary.json"),
            repo_checkout: dir.join("caas-src"),
            dir,
        })
    }
}

#[derive(Debug, Serialize)]
struct ComplianceQuickSummary {
    generated_at: DateTime<Utc>,
    status: String,
    error: Option<String>,
    caas_repo: String,
    caas_ref: String,
    caas_commit: Option<String>,
    jid: String,
    domain: String,
    artifact_dir: String,
    timeout_secs: u64,
    xmpp_host: String,
    xmpp_port: u16,
    host_meta_base_url: Option<String>,
    xep0368_starttls_fallback: bool,
    duration_secs: f64,
    container_id: Option<String>,
    container_exit_code: Option<i64>,
    tests_total: usize,
    tests_passed: usize,
    tests_failed: usize,
    pass_percentage: f64,
    failed_tests: Vec<String>,
    stdout_log: String,
    stderr_log: String,
    command_log: String,
    summary_json: String,
}

#[derive(Debug)]
struct ContainerRunResult {
    container_id: String,
    exit_code: i64,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct ParsedResults {
    total: usize,
    passed: usize,
    failed: usize,
    pass_percentage: f64,
    failed_tests: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let started_at = Instant::now();
    let domain = jid_domain(&args.jid)?;
    let artifact_dir = resolve_artifact_dir(args.artifact_dir.as_deref())?;
    let artifacts = ArtifactPaths::create(artifact_dir)?;

    println!("Running CAAS quick compliance...");
    println!("  Domain:      {}", domain);
    println!("  JID:         {}", args.jid);
    println!("  CAAS ref:    {}", args.caas_ref);
    println!("  Artifacts:   {}", artifacts.dir.display());
    println!("  XMPP host:   {}:{}", args.xmpp_host, args.xmpp_port);
    if let Some(host_meta_base_url) = args
        .host_meta_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        println!("  Host-meta:   {}", host_meta_base_url);
    }
    println!(
        "  XEP-0368:    {}",
        if args.xep0368_starttls_fallback {
            "STARTTLS fallback enabled"
        } else {
            "SRV+Direct-TLS only"
        }
    );
    println!(
        "  Timeout (s): {}",
        if args.timeout_secs == 0 {
            "none (run until completion)".to_string()
        } else {
            args.timeout_secs.to_string()
        }
    );

    let mut summary = ComplianceQuickSummary {
        generated_at: Utc::now(),
        status: "running".to_string(),
        error: None,
        caas_repo: args.caas_repo.clone(),
        caas_ref: args.caas_ref.clone(),
        caas_commit: None,
        jid: args.jid.clone(),
        domain: domain.clone(),
        artifact_dir: artifacts.dir.display().to_string(),
        timeout_secs: args.timeout_secs,
        xmpp_host: args.xmpp_host.clone(),
        xmpp_port: args.xmpp_port,
        host_meta_base_url: args.host_meta_base_url.clone(),
        xep0368_starttls_fallback: args.xep0368_starttls_fallback,
        duration_secs: 0.0,
        container_id: None,
        container_exit_code: None,
        tests_total: 0,
        tests_passed: 0,
        tests_failed: 0,
        pass_percentage: 0.0,
        failed_tests: Vec::new(),
        stdout_log: artifacts.stdout_log.display().to_string(),
        stderr_log: artifacts.stderr_log.display().to_string(),
        command_log: artifacts.command_log.display().to_string(),
        summary_json: artifacts.summary_json.display().to_string(),
    };

    let run_result = run(&args, &artifacts, &domain, &mut summary).await;
    summary.duration_secs = started_at.elapsed().as_secs_f64();
    summary.generated_at = Utc::now();

    match &run_result {
        Ok(()) => {
            summary.status = "ok".to_string();
            summary.error = None;
        }
        Err(error) => {
            summary.status = "error".to_string();
            summary.error = Some(error.to_string());
        }
    }

    write_summary(&artifacts.summary_json, &summary)?;

    match run_result {
        Ok(()) => {
            print_human_summary(&summary);
            Ok(())
        }
        Err(error) => {
            eprintln!("compliance-quick failed: {error}");
            eprintln!("Summary: {}", artifacts.summary_json.display());
            Err(error)
        }
    }
}

async fn run(
    args: &Args,
    artifacts: &ArtifactPaths,
    domain: &str,
    summary: &mut ComplianceQuickSummary,
) -> Result<()> {
    clone_caas_repo(args, artifacts)?;
    patch_caas_repo_for_direct_connect(artifacts)?;
    summary.caas_commit = git_head_sha(&artifacts.repo_checkout);

    let mut container_result =
        run_caas_container(args, artifacts, domain, Some(args.password.as_str())).await?;
    if container_result.exit_code == 0
        && parse_caas_results(&container_result.stdout, &container_result.stderr).total == 0
        && is_auth_failure(&container_result.stderr)
    {
        eprintln!(
            "CAAS authentication failed for {}; retrying with in-band registration",
            args.jid
        );
        container_result = run_caas_container(args, artifacts, domain, None).await?;
    }

    summary.container_id = Some(container_result.container_id.clone());
    summary.container_exit_code = Some(container_result.exit_code);

    fs::write(&artifacts.stdout_log, &container_result.stdout)
        .with_context(|| format!("Writing {}", artifacts.stdout_log.display()))?;
    fs::write(&artifacts.stderr_log, &container_result.stderr)
        .with_context(|| format!("Writing {}", artifacts.stderr_log.display()))?;

    let parsed = parse_caas_results(&container_result.stdout, &container_result.stderr);
    summary.tests_total = parsed.total;
    summary.tests_passed = parsed.passed;
    summary.tests_failed = parsed.failed;
    summary.pass_percentage = parsed.pass_percentage;
    summary.failed_tests = parsed.failed_tests;

    if container_result.exit_code != 0 {
        bail!(
            "CAAS command exited with status {} (see {})",
            container_result.exit_code,
            artifacts.stderr_log.display()
        );
    }

    if parsed.total == 0 {
        if container_result.stderr.contains("Failed to connect to")
            || container_result.stderr.contains("Connection refused")
        {
            bail!(
                "CAAS could not connect to XMPP at {}:{} (see {})",
                args.xmpp_host,
                args.xmpp_port,
                artifacts.stderr_log.display()
            );
        }
        if is_auth_failure(&container_result.stderr) {
            bail!(
                "CAAS authentication failed for {} (not authorized; ensure user/password are valid, see {})",
                args.jid,
                artifacts.stderr_log.display()
            );
        }
        if container_result.stderr.contains("SSLHandshakeException")
            || container_result
                .stderr
                .contains("PKIX path building failed")
        {
            bail!(
                "CAAS failed TLS handshake with XMPP at {}:{} (likely untrusted/self-signed cert; see {})",
                args.xmpp_host,
                args.xmpp_port,
                artifacts.stderr_log.display()
            );
        }
        bail!(
            "CAAS finished without parsing any test results (see {} and {})",
            artifacts.stdout_log.display(),
            artifacts.stderr_log.display()
        );
    }

    Ok(())
}

fn clone_caas_repo(args: &Args, artifacts: &ArtifactPaths) -> Result<()> {
    if artifacts.repo_checkout.exists() {
        if args.verbose {
            println!(
                "Reusing existing checkout at {}",
                artifacts.repo_checkout.display()
            );
        }
        return Ok(());
    }

    fs::create_dir_all(&artifacts.dir)
        .with_context(|| format!("Creating {}", artifacts.dir.display()))?;

    if args.verbose {
        println!(
            "Cloning CAAS repository {} @ {}...",
            args.caas_repo, args.caas_ref
        );
    }

    let clone_status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(&args.caas_ref)
        .arg(&args.caas_repo)
        .arg(&artifacts.repo_checkout)
        .status()
        .context("Running git clone for CAAS")?;

    if clone_status.success() {
        return Ok(());
    }

    if artifacts.repo_checkout.exists() {
        let _ = fs::remove_dir_all(&artifacts.repo_checkout);
    }

    let fallback_clone_status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(&args.caas_repo)
        .arg(&artifacts.repo_checkout)
        .status()
        .context("Running fallback git clone for CAAS")?;
    if !fallback_clone_status.success() {
        bail!("Failed to clone CAAS repository {}", args.caas_repo);
    }

    let fetch_status = Command::new("git")
        .arg("-C")
        .arg(&artifacts.repo_checkout)
        .arg("fetch")
        .arg("--depth")
        .arg("1")
        .arg("origin")
        .arg(&args.caas_ref)
        .status()
        .context("Fetching requested CAAS ref")?;
    if !fetch_status.success() {
        bail!(
            "Failed to fetch CAAS ref {} from {}",
            args.caas_ref,
            args.caas_repo
        );
    }

    let checkout_status = Command::new("git")
        .arg("-C")
        .arg(&artifacts.repo_checkout)
        .arg("checkout")
        .arg("FETCH_HEAD")
        .status()
        .context("Checking out fetched CAAS ref")?;
    if !checkout_status.success() {
        bail!("Failed to checkout fetched CAAS ref {}", args.caas_ref);
    }

    Ok(())
}

fn patch_caas_repo_for_direct_connect(artifacts: &ArtifactPaths) -> Result<()> {
    let test_executor = artifacts
        .repo_checkout
        .join("caas-app/src/main/java/im/conversations/compliance/xmpp/TestExecutor.java");
    let source = fs::read_to_string(&test_executor)
        .with_context(|| format!("Reading {}", test_executor.display()))?;
    let patched = patch_caas_test_executor_source(&source)?;
    if patched != source {
        fs::write(&test_executor, patched)
            .with_context(|| format!("Writing {}", test_executor.display()))?;
    }

    let registration_helper = artifacts
        .repo_checkout
        .join("caas-app/src/main/java/im/conversations/compliance/RegistrationHelper.java");
    let source = fs::read_to_string(&registration_helper)
        .with_context(|| format!("Reading {}", registration_helper.display()))?;
    let patched = patch_caas_registration_helper_source(&source)?;
    if patched != source {
        fs::write(&registration_helper, patched)
            .with_context(|| format!("Writing {}", registration_helper.display()))?;
    }

    let inband_registration_test = artifacts.repo_checkout.join(
        "caas-app/src/main/java/im/conversations/compliance/xmpp/tests/InBandRegistrationTest.java",
    );
    let source = fs::read_to_string(&inband_registration_test)
        .with_context(|| format!("Reading {}", inband_registration_test.display()))?;
    let patched = patch_caas_inband_registration_test_source(&source)?;
    if patched != source {
        fs::write(&inband_registration_test, patched)
            .with_context(|| format!("Writing {}", inband_registration_test.display()))?;
    }

    let alternate_connections_test = artifacts.repo_checkout.join(
        "caas-app/src/main/java/im/conversations/compliance/xmpp/tests/AlternateConnections.java",
    );
    let source = fs::read_to_string(&alternate_connections_test)
        .with_context(|| format!("Reading {}", alternate_connections_test.display()))?;
    let patched = patch_caas_alternate_connections_source(&source)?;
    if patched != source {
        fs::write(&alternate_connections_test, patched)
            .with_context(|| format!("Writing {}", alternate_connections_test.display()))?;
    }

    let xmpp_over_tls_test = artifacts
        .repo_checkout
        .join("caas-app/src/main/java/im/conversations/compliance/xmpp/tests/XmppOverTls.java");
    let source = fs::read_to_string(&xmpp_over_tls_test)
        .with_context(|| format!("Reading {}", xmpp_over_tls_test.display()))?;
    let patched = patch_caas_xmpp_over_tls_source(&source)?;
    if patched != source {
        fs::write(&xmpp_over_tls_test, patched)
            .with_context(|| format!("Writing {}", xmpp_over_tls_test.display()))?;
    }

    Ok(())
}

fn patch_caas_test_executor_source(source: &str) -> Result<String> {
    if source.contains("WADDLE_CAAS_INSECURE_TLS") {
        return Ok(source.to_string());
    }

    let needle = "XmppClient client = XmppClient.create(credential.getDomain(), configuration);";
    let replacement = "\
final String xmppHost = System.getenv(\"WADDLE_CAAS_XMPP_HOST\");
        XmppClient client;
        if (xmppHost == null || xmppHost.trim().isEmpty()) {
            client = XmppClient.create(credential.getDomain(), configuration);
        } else {
            final String xmppPortValue = System.getenv(\"WADDLE_CAAS_XMPP_PORT\");
            final int xmppPort = xmppPortValue == null ? 5222 : Integer.parseInt(xmppPortValue);
            final String insecureTlsValue = System.getenv(\"WADDLE_CAAS_INSECURE_TLS\");
            final boolean insecureTls = insecureTlsValue == null
                    || !(\"0\".equals(insecureTlsValue)
                            || \"false\".equalsIgnoreCase(insecureTlsValue));
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration.Builder socketConnectionBuilder =
                    rocks.xmpp.core.net.client.SocketConnectionConfiguration.builder()
                            .hostname(xmppHost)
                            .port(xmppPort);
            if (insecureTls) {
                final javax.net.ssl.TrustManager[] trustAllManagers =
                        new javax.net.ssl.TrustManager[] {
                            new javax.net.ssl.X509TrustManager() {
                                @Override
                                public void checkClientTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public void checkServerTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public java.security.cert.X509Certificate[] getAcceptedIssuers() {
                                    return new java.security.cert.X509Certificate[0];
                                }
                            }
                        };
                try {
                    final javax.net.ssl.SSLContext sslContext =
                            javax.net.ssl.SSLContext.getInstance(\"TLS\");
                    sslContext.init(null, trustAllManagers, new java.security.SecureRandom());
                    socketConnectionBuilder.sslContext(sslContext);
                    socketConnectionBuilder.hostnameVerifier((hostname, session) -> true);
                } catch (java.security.GeneralSecurityException e) {
                    throw new RuntimeException(
                            \"Failed to configure insecure TLS for WADDLE_CAAS\", e);
                }
            }
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration socketConnectionConfiguration =
                    socketConnectionBuilder.build();
            client = XmppClient.create(
                    credential.getDomain(), configuration, socketConnectionConfiguration);
        }";

    if source.contains(needle) {
        return Ok(source.replacen(needle, replacement, 1));
    }

    // Upgrade earlier waddle patch revisions that already injected host/port overrides.
    let previous_socket_block = "\
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration socketConnectionConfiguration =
                    rocks.xmpp.core.net.client.SocketConnectionConfiguration.builder()
                            .hostname(xmppHost)
                            .port(xmppPort)
                            .build();";
    let upgraded_socket_block = "\
            final String insecureTlsValue = System.getenv(\"WADDLE_CAAS_INSECURE_TLS\");
            final boolean insecureTls = insecureTlsValue == null
                    || !(\"0\".equals(insecureTlsValue)
                            || \"false\".equalsIgnoreCase(insecureTlsValue));
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration.Builder socketConnectionBuilder =
                    rocks.xmpp.core.net.client.SocketConnectionConfiguration.builder()
                            .hostname(xmppHost)
                            .port(xmppPort);
            if (insecureTls) {
                final javax.net.ssl.TrustManager[] trustAllManagers =
                        new javax.net.ssl.TrustManager[] {
                            new javax.net.ssl.X509TrustManager() {
                                @Override
                                public void checkClientTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public void checkServerTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public java.security.cert.X509Certificate[] getAcceptedIssuers() {
                                    return new java.security.cert.X509Certificate[0];
                                }
                            }
                        };
                try {
                    final javax.net.ssl.SSLContext sslContext =
                            javax.net.ssl.SSLContext.getInstance(\"TLS\");
                    sslContext.init(null, trustAllManagers, new java.security.SecureRandom());
                    socketConnectionBuilder.sslContext(sslContext);
                    socketConnectionBuilder.hostnameVerifier((hostname, session) -> true);
                } catch (java.security.GeneralSecurityException e) {
                    throw new RuntimeException(
                            \"Failed to configure insecure TLS for WADDLE_CAAS\", e);
                }
            }
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration socketConnectionConfiguration =
                    socketConnectionBuilder.build();";

    if source.contains(previous_socket_block) {
        return Ok(source.replacen(previous_socket_block, upgraded_socket_block, 1));
    }

    bail!("Unable to patch CAAS TestExecutor.java: expected line not found ({needle})");
}

fn patch_caas_registration_helper_source(source: &str) -> Result<String> {
    if source.contains("WADDLE_CAAS_INSECURE_TLS") {
        return Ok(source.to_string());
    }

    let needle = "XmppClient client = XmppClient.create(jid.getDomain());";
    let replacement = "\
final String xmppHost = System.getenv(\"WADDLE_CAAS_XMPP_HOST\");
        final String xmppPortValue = System.getenv(\"WADDLE_CAAS_XMPP_PORT\");
        final int xmppPort = xmppPortValue == null ? 5222 : Integer.parseInt(xmppPortValue);
        final String insecureTlsValue = System.getenv(\"WADDLE_CAAS_INSECURE_TLS\");
        final boolean insecureTls = insecureTlsValue == null
                || !(\"0\".equals(insecureTlsValue)
                        || \"false\".equalsIgnoreCase(insecureTlsValue));
        XmppClient client;
        if (xmppHost == null || xmppHost.trim().isEmpty()) {
            client = XmppClient.create(jid.getDomain());
        } else {
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration.Builder socketConnectionBuilder =
                    rocks.xmpp.core.net.client.SocketConnectionConfiguration.builder()
                            .hostname(xmppHost)
                            .port(xmppPort);
            if (insecureTls) {
                final javax.net.ssl.TrustManager[] trustAllManagers =
                        new javax.net.ssl.TrustManager[] {
                            new javax.net.ssl.X509TrustManager() {
                                @Override
                                public void checkClientTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public void checkServerTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public java.security.cert.X509Certificate[] getAcceptedIssuers() {
                                    return new java.security.cert.X509Certificate[0];
                                }
                            }
                        };
                try {
                    final javax.net.ssl.SSLContext sslContext =
                            javax.net.ssl.SSLContext.getInstance(\"TLS\");
                    sslContext.init(null, trustAllManagers, new java.security.SecureRandom());
                    socketConnectionBuilder.sslContext(sslContext);
                    socketConnectionBuilder.hostnameVerifier((hostname, session) -> true);
                } catch (java.security.GeneralSecurityException e) {
                    throw new RuntimeException(
                            \"Failed to configure insecure TLS for WADDLE_CAAS\", e);
                }
            }
            client = XmppClient.create(jid.getDomain(), socketConnectionBuilder.build());
        }";

    if !source.contains(needle) {
        bail!("Unable to patch CAAS RegistrationHelper.java: expected line not found ({needle})");
    }

    Ok(source.replacen(needle, replacement, 1))
}

fn patch_caas_inband_registration_test_source(source: &str) -> Result<String> {
    if source.contains("WADDLE_CAAS_XMPP_HOST") {
        return Ok(source.to_string());
    }

    let needle = "\
        final String domain = client.getConnectedResource().getDomain();
        final XmppClient testClient = XmppClient.create(domain);";
    let replacement = "\
        final String domain = client.getConnectedResource().getDomain();
        final String xmppHost = System.getenv(\"WADDLE_CAAS_XMPP_HOST\");
        final String xmppPortValue = System.getenv(\"WADDLE_CAAS_XMPP_PORT\");
        final int xmppPort = xmppPortValue == null ? 5222 : Integer.parseInt(xmppPortValue);
        final String insecureTlsValue = System.getenv(\"WADDLE_CAAS_INSECURE_TLS\");
        final boolean insecureTls = insecureTlsValue == null
                || !(\"0\".equals(insecureTlsValue)
                        || \"false\".equalsIgnoreCase(insecureTlsValue));
        final XmppClient testClient;
        if (xmppHost == null || xmppHost.trim().isEmpty()) {
            testClient = XmppClient.create(domain);
        } else {
            final rocks.xmpp.core.net.client.SocketConnectionConfiguration.Builder socketConnectionBuilder =
                    rocks.xmpp.core.net.client.SocketConnectionConfiguration.builder()
                            .hostname(xmppHost)
                            .port(xmppPort);
            if (insecureTls) {
                final javax.net.ssl.TrustManager[] trustAllManagers =
                        new javax.net.ssl.TrustManager[] {
                            new javax.net.ssl.X509TrustManager() {
                                @Override
                                public void checkClientTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public void checkServerTrusted(
                                        java.security.cert.X509Certificate[] chain,
                                        String authType) {}

                                @Override
                                public java.security.cert.X509Certificate[] getAcceptedIssuers() {
                                    return new java.security.cert.X509Certificate[0];
                                }
                            }
                        };
                try {
                    final javax.net.ssl.SSLContext sslContext =
                            javax.net.ssl.SSLContext.getInstance(\"TLS\");
                    sslContext.init(null, trustAllManagers, new java.security.SecureRandom());
                    socketConnectionBuilder.sslContext(sslContext);
                    socketConnectionBuilder.hostnameVerifier((hostname, session) -> true);
                } catch (java.security.GeneralSecurityException e) {
                    throw new RuntimeException(
                            \"Failed to configure insecure TLS for WADDLE_CAAS\", e);
                }
            }
            testClient = XmppClient.create(domain, socketConnectionBuilder.build());
        }";

    if !source.contains(needle) {
        bail!(
            "Unable to patch CAAS InBandRegistrationTest.java: expected line not found ({needle})"
        );
    }

    Ok(source.replacen(needle, replacement, 1))
}

fn patch_caas_alternate_connections_source(source: &str) -> Result<String> {
    if source.contains("WADDLE_CAAS_HOST_META_BASE_URL") {
        return Ok(source.to_string());
    }

    let needle = "\
            final URL url =
                    new URL(\"https\", domain, \"/.well-known/host-meta\" + (json ? \".json\" : \"\"));";
    let replacement = "\
            final String hostMetaBaseUrl = System.getenv(\"WADDLE_CAAS_HOST_META_BASE_URL\");
            final URL url;
            if (hostMetaBaseUrl != null && !hostMetaBaseUrl.trim().isEmpty()) {
                String base = hostMetaBaseUrl.trim();
                while (base.endsWith(\"/\")) {
                    base = base.substring(0, base.length() - 1);
                }
                url = new URL(base + \"/.well-known/host-meta\" + (json ? \".json\" : \"\"));
            } else {
                url = new URL(\"https\", domain, \"/.well-known/host-meta\" + (json ? \".json\" : \"\"));
            }";

    if !source.contains(needle) {
        bail!("Unable to patch CAAS AlternateConnections.java: expected line not found ({needle})");
    }

    Ok(source.replacen(needle, replacement, 1))
}

fn patch_caas_xmpp_over_tls_source(source: &str) -> Result<String> {
    if source.contains("WADDLE_CAAS_XEP0368_STARTTLS_FALLBACK") {
        return Ok(source.to_string());
    }

    let constructor_needle = "\
    public XmppOverTls(XmppClient client) {
        super(client);
    }

    @Override";
    let constructor_replacement = "\
    public XmppOverTls(XmppClient client) {
        super(client);
    }

    private boolean testStartTlsFallback(final String domain) {
        final String xmppHost = System.getenv(\"WADDLE_CAAS_XMPP_HOST\");
        final String host = (xmppHost == null || xmppHost.trim().isEmpty()) ? domain : xmppHost;
        final String xmppPortValue = System.getenv(\"WADDLE_CAAS_XMPP_PORT\");
        final int xmppPort = xmppPortValue == null ? 5222 : Integer.parseInt(xmppPortValue);
        try (java.net.Socket socket = new java.net.Socket(host, xmppPort)) {
            socket.setSoTimeout(2000);
            final BufferedReader bufferedReader =
                    new BufferedReader(new InputStreamReader(socket.getInputStream()));
            final BufferedWriter bufferedWriter =
                    new BufferedWriter(new OutputStreamWriter(socket.getOutputStream()));

            bufferedWriter.write(
                    \"<?xml version='1.0'?><stream:stream to='\"
                            + domain
                            + \"' version='1.0' xml:lang='en' xmlns='jabber:client'\"
                            + \" xmlns:stream='http://etherx.jabber.org/streams'>\");
            bufferedWriter.flush();

            final char[] featuresBuffer = new char[4096];
            final int featuresRead = bufferedReader.read(featuresBuffer);
            if (featuresRead <= 0) {
                return false;
            }
            final String features = new String(featuresBuffer, 0, featuresRead);
            if (!features.contains(\"<starttls\")) {
                return false;
            }

            bufferedWriter.write(\"<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>\");
            bufferedWriter.flush();

            final char[] proceedBuffer = new char[1024];
            final int proceedRead = bufferedReader.read(proceedBuffer);
            if (proceedRead <= 0) {
                return false;
            }
            final String proceed = new String(proceedBuffer, 0, proceedRead);
            return proceed.contains(\"<proceed\");
        } catch (IOException e) {
            LOGGER.debug(e.getMessage());
            return false;
        }
    }

    @Override";

    let with_helper = if source.contains(constructor_needle) {
        source.replacen(constructor_needle, constructor_replacement, 1)
    } else {
        bail!(
            "Unable to patch CAAS XmppOverTls.java: expected constructor marker not found ({constructor_needle})"
        );
    };

    let run_needle = "        final String domain = client.getDomain().getDomain();";
    let run_replacement = "\
        final String fallbackValue = System.getenv(\"WADDLE_CAAS_XEP0368_STARTTLS_FALLBACK\");
        final boolean startTlsFallback =
                fallbackValue != null
                        && !(\"0\".equals(fallbackValue)
                                || \"false\".equalsIgnoreCase(fallbackValue));
        final String domain = client.getDomain().getDomain();
        if (startTlsFallback) {
            return testStartTlsFallback(domain);
        }";

    if !with_helper.contains(run_needle) {
        bail!(
            "Unable to patch CAAS XmppOverTls.java: expected run marker not found ({run_needle})"
        );
    }

    Ok(with_helper.replacen(run_needle, run_replacement, 1))
}

async fn run_caas_container(
    args: &Args,
    artifacts: &ArtifactPaths,
    domain: &str,
    password: Option<&str>,
) -> Result<ContainerRunResult> {
    let repo_mount_source = artifacts
        .repo_checkout
        .canonicalize()
        .with_context(|| format!("Canonicalizing {}", artifacts.repo_checkout.display()))?;
    let artifact_mount_source = artifacts
        .dir
        .canonicalize()
        .with_context(|| format!("Canonicalizing {}", artifacts.dir.display()))?;

    let shell_script = format!(
        "set -eu; \
cd {repo}; \
mvn -q -DskipTests -Dspotless.check.skip=true package -pl caas-annotations,caas-app; \
if [ \"${{WADDLE_CAAS_FORCE_REGISTRATION:-0}}\" = \"1\" ]; then \
  rm -f accounts.xml; \
  java -jar caas-app/target/caas-app.jar \"$WADDLE_CAAS_JID\"; \
elif [ -n \"${{WADDLE_CAAS_PASSWORD:-}}\" ]; then \
  java -jar caas-app/target/caas-app.jar \"$WADDLE_CAAS_JID\" \"$WADDLE_CAAS_PASSWORD\"; \
else \
  java -jar caas-app/target/caas-app.jar \"$WADDLE_CAAS_JID\"; \
fi",
        repo = CONTAINER_REPO_DIR
    );
    fs::write(&artifacts.command_log, &shell_script)
        .with_context(|| format!("Writing {}", artifacts.command_log.display()))?;

    let mut container_request = GenericImage::new(CAAS_IMAGE, CAAS_TAG)
        .with_entrypoint("sh")
        .with_cmd(vec!["-lc".to_string(), shell_script])
        .with_env_var("WADDLE_CAAS_JID", args.jid.clone())
        .with_env_var("WADDLE_CAAS_XMPP_HOST", args.xmpp_host.clone())
        .with_env_var("WADDLE_CAAS_XMPP_PORT", args.xmpp_port.to_string())
        .with_env_var("WADDLE_CAAS_INSECURE_TLS", "true")
        .with_env_var(
            "WADDLE_CAAS_XEP0368_STARTTLS_FALLBACK",
            if args.xep0368_starttls_fallback {
                "1"
            } else {
                "0"
            },
        )
        .with_host("host.docker.internal", Host::HostGateway)
        .with_mount(Mount::bind_mount(
            repo_mount_source.to_string_lossy().to_string(),
            CONTAINER_REPO_DIR,
        ))
        .with_mount(Mount::bind_mount(
            artifact_mount_source.to_string_lossy().to_string(),
            CONTAINER_ARTIFACT_DIR,
        ))
        .with_startup_timeout(Duration::from_secs(DEFAULT_STARTUP_TIMEOUT_SECS));

    if let Some(host_meta_base_url) = args
        .host_meta_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        container_request =
            container_request.with_env_var("WADDLE_CAAS_HOST_META_BASE_URL", host_meta_base_url);
    }

    container_request = match password {
        Some(value) => container_request
            .with_env_var("WADDLE_CAAS_PASSWORD", value.to_string())
            .with_env_var("WADDLE_CAAS_FORCE_REGISTRATION", "0"),
        None => container_request
            .with_env_var("WADDLE_CAAS_PASSWORD", String::new())
            .with_env_var("WADDLE_CAAS_FORCE_REGISTRATION", "1"),
    };

    let container = container_request
        .start()
        .await
        .context("Starting CAAS container with testcontainers-rs")?;

    let container_id = container.id().to_string();
    if args.verbose {
        println!("CAAS container started: {}", container_id);
    }

    let timeout = if args.timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(args.timeout_secs))
    };

    let exit_code = wait_for_container_exit(container.id(), timeout).await?;

    let stdout = match container.stdout_to_vec().await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(error) => {
            eprintln!("Failed reading container stdout via testcontainers: {error}");
            String::new()
        }
    };
    let stderr = match container.stderr_to_vec().await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(error) => {
            eprintln!("Failed reading container stderr via testcontainers: {error}");
            String::new()
        }
    };

    if args.keep_container {
        println!("Keeping CAAS container alive: {}", container.id());
        std::mem::forget(container);
    }

    let resolved_domain = if domain.is_empty() {
        "<unknown>"
    } else {
        domain
    };
    println!("CAAS execution finished for domain {}", resolved_domain);

    Ok(ContainerRunResult {
        container_id,
        exit_code,
        stdout,
        stderr,
    })
}

async fn wait_for_container_exit(container_id: &str, timeout: Option<Duration>) -> Result<i64> {
    let deadline = timeout.map(|limit| Instant::now() + limit);
    loop {
        let status = inspect_container_status(container_id)?;
        if !status.running {
            return Ok(status.exit_code);
        }

        if let Some(deadline) = deadline {
            if Instant::now() > deadline {
                bail!(
                    "Timed out waiting for CAAS container {} after {}s",
                    container_id,
                    timeout.map(|value| value.as_secs()).unwrap_or(0)
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
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
        .context("Inspecting CAAS container status")?;

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

fn parse_caas_results(stdout: &str, stderr: &str) -> ParsedResults {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failed_tests = BTreeSet::new();

    for line in stdout.lines().chain(stderr.lines()) {
        let clean = strip_ansi(line).trim().to_string();
        if clean.ends_with(" PASSED") {
            passed += 1;
            continue;
        }
        if clean.ends_with(" FAILED") {
            failed += 1;
            let test_name = clean.trim_end_matches(" FAILED").trim();
            if !test_name.is_empty() {
                failed_tests.insert(test_name.to_string());
            }
        }
    }

    let total = passed + failed;
    let pass_percentage = if total == 0 {
        0.0
    } else {
        (passed as f64 / total as f64) * 100.0
    };

    ParsedResults {
        total,
        passed,
        failed,
        pass_percentage,
        failed_tests: failed_tests.into_iter().collect(),
    }
}

fn is_auth_failure(stderr: &str) -> bool {
    stderr.contains("AuthenticationException") || stderr.contains("not-authorized")
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                let _ = chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn resolve_artifact_dir(path: Option<&str>) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("Resolving current working directory")?;
    let dir = match path {
        Some(value) => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                cwd.join(candidate)
            }
        }
        None => {
            let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
            cwd.join("test-logs")
                .join(format!("compliance-quick-{}", ts))
        }
    };
    Ok(dir)
}

fn write_summary(path: &Path, summary: &ComplianceQuickSummary) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(summary).context("Encoding compliance quick summary JSON")?;
    fs::write(path, bytes).with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

fn print_human_summary(summary: &ComplianceQuickSummary) {
    println!();
    println!("CAAS quick compliance summary:");
    println!("  Total:   {}", summary.tests_total);
    println!("  Passed:  {}", summary.tests_passed);
    println!("  Failed:  {}", summary.tests_failed);
    println!("  Pass %:  {:.2}%", summary.pass_percentage);
    println!(
        "  Runtime: {:.1}s",
        if summary.duration_secs.is_finite() {
            summary.duration_secs
        } else {
            0.0
        }
    );
    println!("  Summary: {}", summary.summary_json);

    if !summary.failed_tests.is_empty() {
        println!("  Failed tests:");
        for test in summary.failed_tests.iter().take(20) {
            println!("    - {}", test);
        }
        if summary.failed_tests.len() > 20 {
            println!("    ... and {} more", summary.failed_tests.len() - 20);
        }
    }
}

fn jid_domain(jid: &str) -> Result<String> {
    let cleaned = jid.trim();
    if cleaned.is_empty() {
        bail!("--jid must not be empty");
    }

    let domain_part = if let Some((_, rest)) = cleaned.split_once('@') {
        rest
    } else {
        cleaned
    };

    let domain = domain_part.split('/').next().unwrap_or("").trim();
    if domain.is_empty() {
        bail!("Unable to parse domain from JID '{}'", jid);
    }

    Ok(domain.to_string())
}

fn git_head_sha(repo_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_results_counts_pass_and_fail() {
        let out = "\
XEP-0191: Blocking Command PASSED
XEP-0368: SRV records for XMPP over TLS FAILED
";
        let parsed = parse_caas_results(out, "");
        assert_eq!(parsed.total, 2);
        assert_eq!(parsed.passed, 1);
        assert_eq!(parsed.failed, 1);
        assert_eq!(parsed.failed_tests.len(), 1);
    }

    #[test]
    fn parse_results_handles_ansi() {
        let out = "\u{1b}[31mXEP-0485: PubSub Server Information FAILED\u{1b}[0m";
        let parsed = parse_caas_results(out, "");
        assert_eq!(parsed.failed, 1);
        assert_eq!(
            parsed.failed_tests[0],
            "XEP-0485: PubSub Server Information"
        );
    }

    #[test]
    fn parse_domain_from_jid() {
        assert_eq!(jid_domain("alice@example.com").unwrap(), "example.com");
        assert_eq!(
            jid_domain("alice@example.com/resource").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn patch_test_executor_source_adds_waddle_override() {
        let source =
            "XmppClient client = XmppClient.create(credential.getDomain(), configuration);";
        let patched = patch_caas_test_executor_source(source).unwrap();
        assert!(patched.contains("WADDLE_CAAS_XMPP_HOST"));
        assert!(patched.contains("SocketConnectionConfiguration"));
    }

    #[test]
    fn patch_test_executor_source_is_idempotent() {
        let source =
            "XmppClient client = XmppClient.create(credential.getDomain(), configuration);";
        let once = patch_caas_test_executor_source(source).unwrap();
        let twice = patch_caas_test_executor_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn patch_registration_helper_source_adds_waddle_override() {
        let source = "XmppClient client = XmppClient.create(jid.getDomain());";
        let patched = patch_caas_registration_helper_source(source).unwrap();
        assert!(patched.contains("WADDLE_CAAS_XMPP_HOST"));
        assert!(patched.contains("WADDLE_CAAS_INSECURE_TLS"));
    }

    #[test]
    fn patch_registration_helper_source_is_idempotent() {
        let source = "XmppClient client = XmppClient.create(jid.getDomain());";
        let once = patch_caas_registration_helper_source(source).unwrap();
        let twice = patch_caas_registration_helper_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn patch_test_executor_source_errors_when_expected_line_missing() {
        let error = patch_caas_test_executor_source("no matching line here").unwrap_err();
        assert!(error
            .to_string()
            .contains("Unable to patch CAAS TestExecutor.java"));
    }

    #[test]
    fn patch_alternate_connections_source_adds_host_meta_override() {
        let source = "\
            final URL url =
                    new URL(\"https\", domain, \"/.well-known/host-meta\" + (json ? \".json\" : \"\"));";
        let patched = patch_caas_alternate_connections_source(source).unwrap();
        assert!(patched.contains("WADDLE_CAAS_HOST_META_BASE_URL"));
    }

    #[test]
    fn patch_xmpp_over_tls_source_adds_starttls_fallback() {
        let source = "\
public class XmppOverTls extends AbstractTest {
    public XmppOverTls(XmppClient client) {
        super(client);
    }

    @Override
    public boolean run() {
        final String domain = client.getDomain().getDomain();
        return false;
    }
}";
        let patched = patch_caas_xmpp_over_tls_source(source).unwrap();
        assert!(patched.contains("WADDLE_CAAS_XEP0368_STARTTLS_FALLBACK"));
        assert!(patched.contains("testStartTlsFallback"));
    }
}
