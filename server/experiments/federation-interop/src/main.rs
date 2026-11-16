//! `federation-interop`: start the environment, walk the manifest and
//! write the evidence log. Run it through `run-native.sh`, which supplies
//! the pinned binaries; see the README for the options.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

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
    environment.insert(
        "waddle_server".to_owned(),
        bins.waddle.display().to_string(),
    );
    environment.insert("git_sha".to_owned(), git_sha.to_owned());
    environment
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

/// SIGINT or SIGTERM.
async fn interrupt() {
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
}

#[tokio::main]
async fn main() -> Result<()> {
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
    std::fs::create_dir_all(&root)?;
    let mut log = EvidenceLog::new(run_id, started, environment(&bins, &git_sha));
    let later = [
        Section::Flow,
        Section::Observation,
        Section::Negative,
        Section::Restart,
    ];

    match Harness::prepare(&root, options.ports, bins, git_sha).await {
        Ok(mut harness) => {
            // An interrupted run still stops every service it started: the
            // run is abandoned at its next await and the harness shuts down.
            let interrupted = tokio::select! {
                () = run_all(&mut harness, &mut log, &options.filter, &later) => false,
                () = interrupt() => true,
            };
            harness.shutdown().await;
            if interrupted {
                log.write(&options.evidence)?;
                anyhow::bail!("interrupted; partial evidence written");
            }
        }
        Err(error) => {
            let reason = format!("the environment did not start: {error:#}");
            runner::record_all_unverified(
                &mut log,
                &options.filter,
                &[Section::S2sDisabled],
                &reason,
            );
            runner::record_all_unverified(&mut log, &options.filter, &later, &reason);
        }
    }
    let (json, markdown) = log.write(&options.evidence)?;
    println!("evidence: {} and {}", json.display(), markdown.display());
    println!("summary: {:?}", log.counts());
    if options.keep {
        println!("work directory kept: {}", root.display());
    } else {
        std::fs::remove_dir_all(&root).ok();
    }
    Ok(())
}
