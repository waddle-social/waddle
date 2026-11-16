//! `federation-interop`: start the environment, walk the manifest and
//! write the evidence log. Run it through `run-native.sh`, which supplies
//! the pinned binaries; see the README for the options.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::signal::unix::{signal, Signal, SignalKind};

use waddle_federation_interop::evidence::{EvidenceLog, Section};
use waddle_federation_interop::scenarios::harness::{Binaries, Harness};
use waddle_federation_interop::scenarios::runner::{self, Filter};
use waddle_federation_interop::topology::Ports;

struct Options {
    evidence: PathBuf,
    root: Option<PathBuf>,
    ports: Ports,
    filter: Filter,
    keep: bool,
}

fn options() -> Result<Options> {
    let mut evidence = None;
    let mut root = None;
    let mut ports = Ports {
        base: Ports::DEFAULT_BASE,
    };
    let mut filter = Filter::default();
    let mut keep = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--evidence-dir" => {
                evidence = Some(PathBuf::from(
                    args.next().context("--evidence-dir needs a path")?,
                ))
            }
            "--work-dir" => {
                root = Some(PathBuf::from(
                    args.next().context("--work-dir needs a path")?,
                ))
            }
            "--port-base" => {
                ports.base = args.next().context("--port-base needs a port")?.parse()?;
            }
            "--only" => filter.prefixes.extend(
                args.next()
                    .context("--only needs a prefix")?
                    .split(',')
                    .map(str::to_owned),
            ),
            "--keep" => keep = true,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    Ok(Options {
        evidence: evidence.context("--evidence-dir is required")?,
        root,
        ports,
        filter,
        keep,
    })
}

/// The server binary as the evidence names it: its path relative to the
/// repository when it lies inside it, otherwise its file name and a hash of
/// the path, so no absolute path of the machine that ran it is recorded.
fn binary_name(path: &Path) -> String {
    if let Some(relative) = std::env::var_os("INTEROP_REPOSITORY_ROOT")
        .and_then(|root| path.strip_prefix(root).ok().map(Path::to_path_buf))
    {
        return relative.display().to_string();
    }
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    let hash: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    [
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
        " (path sha256 ",
        &hash,
        ")",
    ]
    .concat()
}

fn environment(bins: &Binaries, git_sha: &str) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    for key in [
        "INTEROP_PROSODY_VERSION",
        "INTEROP_EJABBERD_VERSION",
        "INTEROP_POSTGRES_VERSION",
        "INTEROP_SPICEDB_VERSION",
        "INTEROP_LANDED",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_owned(), value);
        }
    }
    environment.insert("waddle_server".to_owned(), binary_name(&bins.waddle));
    environment.insert("git_sha".to_owned(), git_sha.to_owned());
    environment
}

/// The work directory, readable by its owner only: it holds the run's
/// private keys, secrets and passwords.
fn create_private_dir(root: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .with_context(|| format!("create {}", root.display()))?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {}", root.display()))
}

/// Step 6's S2S-off row, then the deployments with S2S, the third
/// parties and every other row.
async fn run_all(harness: &mut Harness, log: &mut EvidenceLog, filter: &Filter, later: &[Section]) {
    runner::run(harness, log, filter, &[Section::S2sDisabled]).await;
    match harness.start_standard_waddles().await {
        Ok(()) => {
            harness.start_third_parties().await;
            runner::run(harness, log, filter, later).await;
        }
        Err(error) => runner::record_all_unverified(
            log,
            filter,
            later,
            &format!("the Waddle deployments did not start: {error:#}"),
        ),
    }
}

/// SIGINT, SIGTERM and SIGHUP, caught from before the first service starts
/// so that none of them ends the harness without its cleanup: the default
/// action would leave every service's process group running.
struct Interrupts {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

impl Interrupts {
    fn new() -> Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt()).context("catch SIGINT")?,
            terminate: signal(SignalKind::terminate()).context("catch SIGTERM")?,
            hangup: signal(SignalKind::hangup()).context("catch SIGHUP")?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut interrupts = Interrupts::new()?;
    let options = options()?;
    let bins = Binaries::from_env()?;
    let git_sha = std::env::var("INTEROP_GIT_SHA").unwrap_or_else(|_| "unknown".to_owned());
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let run_id = [
        started.to_string(),
        "-".to_owned(),
        git_sha.chars().take(12).collect(),
    ]
    .concat();
    let root = match &options.root {
        Some(root) => root.clone(),
        None => std::env::temp_dir().join(["waddle-federation-interop-", &run_id].concat()),
    };
    create_private_dir(&root)?;
    let mut log = EvidenceLog::new(run_id, started, environment(&bins, &git_sha));
    let later = [
        Section::Flow,
        Section::Observation,
        Section::Negative,
        Section::Restart,
    ];
    let every = [
        Section::S2sDisabled,
        Section::Flow,
        Section::Observation,
        Section::Negative,
        Section::Restart,
    ];

    // An interrupt abandons the work at its next await. During `prepare`
    // the services started so far are dropped, which kills their process
    // groups; afterwards the harness shuts every service down.
    let prepared = tokio::select! {
        prepared = Harness::prepare(&root, options.ports, bins, git_sha) => Some(prepared),
        () = interrupts.recv() => None,
    };
    let interrupted = match prepared {
        None => true,
        Some(Ok(mut harness)) => {
            let interrupted = tokio::select! {
                () = run_all(&mut harness, &mut log, &options.filter, &later) => false,
                () = interrupts.recv() => true,
            };
            harness.shutdown().await;
            interrupted
        }
        Some(Err(error)) => {
            let reason = format!("the environment did not start: {error:#}");
            runner::record_all_unverified(&mut log, &options.filter, &every, &reason);
            false
        }
    };
    if interrupted {
        runner::record_unrun(
            &mut log,
            &options.filter,
            &every,
            "the run was interrupted before this row finished",
        );
    }
    let (json, markdown) = log.write(&options.evidence)?;
    println!("evidence: {} and {}", json.display(), markdown.display());
    println!("summary: {:?}", log.counts());
    if options.keep {
        println!("work directory kept: {}", root.display());
    } else {
        std::fs::remove_dir_all(&root).ok();
    }
    anyhow::ensure!(!interrupted, "interrupted; partial evidence written");
    Ok(())
}
