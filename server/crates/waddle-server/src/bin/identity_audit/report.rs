use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use tempfile::NamedTempFile;

use super::analysis::{AccountObservation, Collision, FrameObservation};
use super::model::{AuditError, Finding, OriginalValue, PROFILE};
use super::profile::candidate_findings;
use super::source::Driver;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    CompleteNoFindings,
    CompleteWithFindings,
}

#[derive(Serialize)]
pub struct Report {
    pub format_version: u32,
    pub candidate_profile: &'static str,
    pub status: Status,
    pub driver: Driver,
    pub asserted_oidc_home_domain: OriginalValue,
    pub asserted_domain_findings: Vec<Finding>,
    pub users_snapshot_complete: bool,
    pub native_users_snapshot_complete: bool,
    pub frame_corpus_requested: bool,
    pub limitations: [&'static str; 5],
    pub accounts: Vec<AccountObservation>,
    pub collisions: Vec<Collision>,
    pub frames: Vec<FrameObservation>,
}

impl Report {
    pub fn new(
        driver: Driver,
        domain: String,
        accounts: Vec<AccountObservation>,
        collisions: Vec<Collision>,
        frames: Vec<FrameObservation>,
        frames_requested: bool,
    ) -> Self {
        let asserted_domain_findings = candidate_findings("audit", &domain);
        let findings = !asserted_domain_findings.is_empty()
            || accounts.iter().any(|row| !row.findings.is_empty())
            || !collisions.is_empty()
            || frames
                .iter()
                .any(|frame| !frame.from.findings.is_empty() || !frame.to.findings.is_empty());
        Self {
            format_version: 1, candidate_profile: PROFILE,
            status: if findings { Status::CompleteWithFindings } else { Status::CompleteNoFindings },
            driver, asserted_oidc_home_domain: OriginalValue::new(domain), asserted_domain_findings,
            users_snapshot_complete: true, native_users_snapshot_complete: true, frame_corpus_requested: frames_requested,
            limitations: ["Snapshot only; concurrent or future writers are not excluded", "OIDC home domain is an operator assertion, not authenticated ownership", "Top-level frame addresses only; no SASL, nested identity, resume, relay or S2S coverage", "Diagnostic candidate only; no enrollment, authority, migration or full PRECIS/IDNA conformance", "Trusted database administration and stable operator-controlled SQLite paths required; inode checks do not defeat hostile swap-back"],
            accounts, collisions, frames,
        }
    }
}

/// Identifiers are written only into a verified private, newly created inode.
/// No final artifact is published until serialization and sync both succeed.
pub fn publish(path: &Path, report: &Report) -> Result<(), AuditError> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(AuditError::Report);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent).map_err(|_| AuditError::Report)?;
    verify_private(&temporary)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), report)
        .map_err(|_| AuditError::Report)?;
    temporary.write_all(b"\n").map_err(|_| AuditError::Report)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| AuditError::Report)?;
    temporary
        .persist_noclobber(path)
        .map_err(|_| AuditError::Report)?;
    Ok(())
}

#[cfg(unix)]
fn verify_private(file: &NamedTempFile) -> Result<(), AuditError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file.as_file().metadata().map_err(|_| AuditError::Report)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(AuditError::Report);
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_private(_file: &NamedTempFile) -> Result<(), AuditError> {
    Err(AuditError::Report)
}
