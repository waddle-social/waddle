use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::Parser;
use serde_json::json;

use super::analysis;
use super::model::{AuditError, FIELD_BYTES};
use super::report::{publish, Report, Status};
use super::source::{read, Driver};

const CORPUS_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "waddle-identity-audit",
    about = "Read-only snapshot diagnostics; never enrollment or authorization"
)]
struct Args {
    #[arg(long, value_enum)]
    driver: Driver,
    /// Operator assertion: users rows do not store the authoritative home domain.
    #[arg(long)]
    oidc_home_domain: String,
    /// New private JSON artifact; identifiers never go to stdout or stderr.
    #[arg(long)]
    report: PathBuf,
    /// Optional JSON array of raw XML stanza examples (maximum 4 MiB).
    #[arg(long)]
    frames: Option<PathBuf>,
    #[arg(long, default_value_t = 10_000, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    max_rows: u32,
    /// Deadline for connection and snapshot reads; local artifact I/O is separate.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout_seconds: u64,
}

pub async fn run() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(_) => {
            eprintln!("identity audit incomplete: configuration");
            return ExitCode::from(2);
        }
    };
    match execute(args).await {
        Ok(report) => {
            let code = match report.status {
                Status::CompleteNoFindings => 0,
                Status::CompleteWithFindings => 1,
            };
            println!(
                "{}",
                json!({"status":report.status,"account_rows":report.accounts.len(),"collision_groups":report.collisions.len(),"frames":report.frames.len()})
            );
            ExitCode::from(code)
        }
        Err(error) => {
            // Deliberately do not format underlying SQLx, XML, file or CLI values.
            eprintln!("identity audit incomplete: {error}");
            println!("{}", json!({"status":"incomplete","error":error}));
            ExitCode::from(2)
        }
    }
}

async fn execute(args: Args) -> Result<Report, AuditError> {
    if args.oidc_home_domain.is_empty() || args.oidc_home_domain.len() > FIELD_BYTES {
        return Err(AuditError::Configuration);
    }
    let url = std::env::var("WADDLE_IDENTITY_AUDIT_DATABASE_URL")
        .map_err(|_| AuditError::Configuration)?;
    let rows = tokio::time::timeout(
        Duration::from_secs(args.timeout_seconds),
        read(args.driver, &url, &args.oidc_home_domain, args.max_rows),
    )
    .await
    .map_err(|_| AuditError::Timeout)??;
    let frames = match &args.frames {
        Some(path) => analysis::frames(&corpus(path)?)?,
        None => Vec::new(),
    };
    let (accounts, collisions) = analysis::accounts(rows);
    let report = Report::new(
        args.driver,
        args.oidc_home_domain,
        accounts,
        collisions,
        frames,
        args.frames.is_some(),
    );
    publish(&args.report, &report)?;
    Ok(report)
}

fn corpus(path: &Path) -> Result<Vec<String>, AuditError> {
    let file = open_corpus(path)?;
    let mut bytes = Vec::new();
    file.take(CORPUS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AuditError::Frames)?;
    if bytes.len() as u64 > CORPUS_BYTES {
        return Err(AuditError::Limit);
    }
    let frames: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| AuditError::Frames)?;
    if frames.len() > 1000 {
        return Err(AuditError::Limit);
    }
    Ok(frames)
}

#[cfg(unix)]
fn open_corpus(path: &Path) -> Result<File, AuditError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| AuditError::Frames)?;
    if !file.metadata().map_err(|_| AuditError::Frames)?.is_file() {
        return Err(AuditError::Frames);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_corpus(_path: &Path) -> Result<File, AuditError> {
    Err(AuditError::Frames)
}

#[cfg(test)]
mod tests;
