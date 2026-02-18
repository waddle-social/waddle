use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};

const PROSODY_IMAGE_NAME: &str = "waddle-prosody-test";
const PROSODY_IMAGE_TAG: &str = "latest";
const PROSODY_DOMAIN: &str = "localhost";
const PROSODY_MUC_DOMAIN: &str = "conference.localhost";
const PROSODY_C2S_PORT: u16 = 5222;
const PROSODY_HTTP_PORT: u16 = 5280;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProsodyUser {
    pub username: &'static str,
    pub jid: &'static str,
    pub password: &'static str,
}

pub const ALICE: ProsodyUser = ProsodyUser {
    username: "alice",
    jid: "alice@localhost",
    password: "alice_pass",
};

pub const BOB: ProsodyUser = ProsodyUser {
    username: "bob",
    jid: "bob@localhost",
    password: "bob_pass",
};

pub const CHARLIE: ProsodyUser = ProsodyUser {
    username: "charlie",
    jid: "charlie@localhost",
    password: "charlie_pass",
};

pub const USERS: [ProsodyUser; 3] = [ALICE, BOB, CHARLIE];

#[derive(Debug, thiserror::Error)]
pub enum ProsodyHarnessError {
    #[error("failed to locate Prosody docker context: {path}")]
    DockerContextMissing { path: String },

    #[error("docker command failed: {0}")]
    DockerIo(#[from] std::io::Error),

    #[error("docker build failed for image {image}: {details}")]
    DockerBuildFailed { image: String, details: String },

    #[error("failed to start Prosody container: {0}")]
    ContainerStart(String),

    #[error("failed to resolve Prosody host: {0}")]
    HostResolve(String),

    #[error("failed to resolve Prosody mapped port {port}: {details}")]
    PortResolve { port: u16, details: String },

    #[error(
        "timed out waiting for Prosody at {endpoint} after {timeout_secs}s; last error: {last_error}"
    )]
    StartupTimeout {
        endpoint: String,
        timeout_secs: u64,
        last_error: String,
    },
}

#[derive(Debug)]
pub struct ProsodyHarness {
    container: ContainerAsync<GenericImage>,
    host: String,
    c2s_port: u16,
    http_port: u16,
}

impl ProsodyHarness {
    pub async fn start() -> Result<Self, ProsodyHarnessError> {
        ensure_image_available()?;

        let image = GenericImage::new(PROSODY_IMAGE_NAME, PROSODY_IMAGE_TAG)
            .with_exposed_port(PROSODY_C2S_PORT.tcp())
            .with_exposed_port(PROSODY_HTTP_PORT.tcp())
            .with_wait_for(WaitFor::seconds(1));

        let container = image
            .start()
            .await
            .map_err(|error| ProsodyHarnessError::ContainerStart(error.to_string()))?;
        let host = container
            .get_host()
            .await
            .map_err(|error| ProsodyHarnessError::HostResolve(error.to_string()))?
            .to_string();
        let c2s_port = mapped_port(&container, PROSODY_C2S_PORT).await?;
        let http_port = mapped_port(&container, PROSODY_HTTP_PORT).await?;

        wait_for_tcp(&host, c2s_port, STARTUP_TIMEOUT).await?;

        Ok(Self {
            container,
            host,
            c2s_port,
            http_port,
        })
    }

    pub fn container_id(&self) -> &str {
        self.container.id()
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn c2s_port(&self) -> u16 {
        self.c2s_port
    }

    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    pub fn c2s_endpoint(&self) -> String {
        endpoint(&self.host, self.c2s_port)
    }

    pub fn http_endpoint(&self) -> String {
        endpoint(&self.host, self.http_port)
    }

    pub fn domain(&self) -> &'static str {
        PROSODY_DOMAIN
    }

    pub fn muc_domain(&self) -> &'static str {
        PROSODY_MUC_DOMAIN
    }

    pub fn users(&self) -> &'static [ProsodyUser] {
        &USERS
    }
}

async fn mapped_port(
    container: &ContainerAsync<GenericImage>,
    internal_port: u16,
) -> Result<u16, ProsodyHarnessError> {
    match container.get_host_port_ipv4(internal_port).await {
        Ok(port) => Ok(port),
        Err(_) => container
            .get_host_port_ipv6(internal_port)
            .await
            .map_err(|error| ProsodyHarnessError::PortResolve {
                port: internal_port,
                details: error.to_string(),
            }),
    }
}

fn endpoint(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) -> Result<(), ProsodyHarnessError> {
    let started = Instant::now();
    let mut last_error = String::from("connection was never attempted");

    while started.elapsed() < timeout {
        match tokio::net::TcpStream::connect((host, port)).await {
            Ok(_stream) => {
                return Ok(());
            }
            Err(error) => {
                last_error = error.to_string();
                tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
            }
        }
    }

    Err(ProsodyHarnessError::StartupTimeout {
        endpoint: endpoint(host, port),
        timeout_secs: timeout.as_secs(),
        last_error,
    })
}

fn ensure_image_available() -> Result<(), ProsodyHarnessError> {
    static BUILD_RESULT: OnceLock<Result<(), String>> = OnceLock::new();

    BUILD_RESULT
        .get_or_init(|| build_image_if_missing().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|details| ProsodyHarnessError::DockerBuildFailed {
            image: image_ref(),
            details: details.clone(),
        })
        .map(|_| ())
}

fn build_image_if_missing() -> Result<(), ProsodyHarnessError> {
    if image_exists()? {
        return Ok(());
    }

    let context = docker_context_dir();
    if !context.is_dir() {
        return Err(ProsodyHarnessError::DockerContextMissing {
            path: context.display().to_string(),
        });
    }

    let dockerfile = context.join("Dockerfile");
    let output = Command::new("docker")
        .arg("build")
        .arg("--tag")
        .arg(image_ref())
        .arg("--file")
        .arg(dockerfile)
        .arg(".")
        .current_dir(context)
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let details = if stderr.is_empty() { stdout } else { stderr };

    Err(ProsodyHarnessError::DockerBuildFailed {
        image: image_ref(),
        details,
    })
}

fn image_exists() -> Result<bool, ProsodyHarnessError> {
    let status = Command::new("docker")
        .arg("image")
        .arg("inspect")
        .arg(image_ref())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn docker_context_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("docker")
        .join("prosody")
}

fn image_ref() -> String {
    format!("{PROSODY_IMAGE_NAME}:{PROSODY_IMAGE_TAG}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users_are_preconfigured() {
        assert_eq!(USERS.len(), 3);
        assert!(USERS.iter().any(|user| user.jid == "alice@localhost"));
        assert!(USERS.iter().any(|user| user.jid == "bob@localhost"));
        assert!(USERS.iter().any(|user| user.jid == "charlie@localhost"));
    }

    #[test]
    fn docker_context_contains_expected_assets() {
        let context = docker_context_dir();
        assert!(context.is_dir());
        assert!(context.ends_with("tests/docker/prosody"));
        assert!(context.join("Dockerfile").is_file());
        assert!(context.join("prosody.cfg.lua").is_file());
        assert!(context.join("create-users.sh").is_file());
    }
}
