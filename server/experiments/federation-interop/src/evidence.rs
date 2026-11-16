//! The evidence log: one record per manifest row, each PASS, FAIL or
//! UNVERIFIED, written as JSON and Markdown under `docs/evidence/2047/`.
//!
//! A row is PASS only when every check it ran passed and it ran at least
//! one. A row that was not driven, or that hit a missing prerequisite, is
//! UNVERIFIED with the reason, never PASS.
//!
//! Every text that enters a record passes through [`scrub`]: log tails and
//! error chains carry absolute paths, connection strings and secrets of the
//! run, which the evidence must not keep.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Fail,
    Unverified,
}

impl fmt::Display for Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unverified => "UNVERIFIED",
        })
    }
}

/// One assertion within a row.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

/// What a driver established for one row.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub status: Status,
    /// Who produced the traffic: the stock server itself, a client of it,
    /// or the harness's raw peer holding a domain's leaf certificate.
    pub driven_by: String,
    pub checks: Vec<Check>,
    pub notes: Vec<String>,
}

impl Outcome {
    pub fn new(driven_by: impl Into<String>) -> Self {
        Self {
            status: Status::Unverified,
            driven_by: scrub(&driven_by.into()),
            checks: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// A row that was not driven, with the reason.
    pub fn not_driven(reason: impl Into<String>) -> Self {
        let mut outcome = Self::new("not driven");
        outcome.unverified("not driven", reason);
        outcome.finish()
    }

    /// Record an assertion that executed.
    pub fn check(&mut self, name: impl Into<String>, passed: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: scrub(&name.into()),
            status: if passed { Status::Pass } else { Status::Fail },
            detail: scrub(&detail.into()),
        });
    }

    /// Record a part of the row that could not be established.
    pub fn unverified(&mut self, name: impl Into<String>, reason: impl Into<String>) {
        self.checks.push(Check {
            name: scrub(&name.into()),
            status: Status::Unverified,
            detail: scrub(&reason.into()),
        });
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(scrub(&note.into()));
    }

    /// Settle the row's status from its checks.
    pub fn finish(mut self) -> Self {
        self.status = if self.checks.iter().any(|check| check.status == Status::Fail) {
            Status::Fail
        } else if self.checks.is_empty()
            || self
                .checks
                .iter()
                .any(|check| check.status == Status::Unverified)
        {
            Status::Unverified
        } else {
            Status::Pass
        };
        self
    }
}

/// A driver's way of saying a prerequisite outside Waddle was missing, so
/// the row is UNVERIFIED rather than FAIL.
#[derive(Debug)]
pub struct Unverifiable(pub String);

impl fmt::Display for Unverifiable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Unverifiable {}

/// Turn a driver's result into an outcome: an error is a FAIL unless it is
/// [`Unverifiable`].
pub fn settle(result: Result<Outcome>, driven_by: &str) -> Outcome {
    match result {
        Ok(outcome) => outcome.finish(),
        Err(error) => {
            let mut outcome = Outcome::new(driven_by);
            match error.downcast_ref::<Unverifiable>() {
                Some(reason) => outcome.unverified("prerequisite", reason.0.clone()),
                None => outcome.check("driver", false, format!("{error:#}")),
            }
            outcome.finish()
        }
    }
}

/// Which part of the verification phase a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// Step 6: S2S disabled.
    S2sDisabled,
    /// The flow table of steps 6 and 7.
    Flow,
    /// The negative matrix of step 6, and its G20 rows in step 7.
    Negative,
    /// The W10 observations of each peer's wire behavior.
    Observation,
    /// Step 8: failure and restart.
    Restart,
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub id: String,
    pub section: Section,
    pub title: String,
    /// The peer of this row: `waddle`, `prosody` or `ejabberd`.
    pub peer: String,
    pub g20: bool,
    pub outcome: Outcome,
}

#[derive(Debug, Serialize)]
pub struct EvidenceLog {
    pub run_id: String,
    pub started_unix: u64,
    /// Package versions and source revision the run used.
    pub environment: BTreeMap<String, String>,
    pub records: Vec<Record>,
}

impl EvidenceLog {
    pub fn new(run_id: String, started_unix: u64, environment: BTreeMap<String, String>) -> Self {
        Self {
            run_id,
            started_unix,
            environment,
            records: Vec::new(),
        }
    }

    pub fn record(&mut self, record: Record) {
        println!(
            "{:<10} {:<9} {:<48} {}",
            record.outcome.status.to_string(),
            record.peer,
            record.id,
            record.title
        );
        self.records.push(record);
    }

    /// Whether a record exists for `id` against `peer`.
    pub fn has(&self, id: &str, peer: &str) -> bool {
        self.records
            .iter()
            .any(|record| record.id == id && record.peer == peer)
    }

    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for record in &self.records {
            *counts.entry(record.outcome.status.to_string()).or_insert(0) += 1;
        }
        counts
    }

    /// Write `<stem>.json` and `<stem>.md` into `directory`.
    pub fn write(&self, directory: &Path) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let stem = ["federation-interop", &self.run_id].join("-");
        let json = directory.join([stem.as_str(), "json"].join("."));
        let markdown = directory.join([stem.as_str(), "md"].join("."));
        std::fs::write(&json, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", json.display()))?;
        std::fs::write(&markdown, self.markdown())
            .with_context(|| format!("write {}", markdown.display()))?;
        Ok((json, markdown))
    }

    fn markdown(&self) -> String {
        let mut lines = vec![
            format!("# Federation interoperability run {}", self.run_id),
            String::new(),
            "Generated by `server/experiments/federation-interop`. A row is PASS only when \
             every assertion it ran passed; UNVERIFIED rows name what was missing."
                .to_owned(),
            String::new(),
            "## Environment".to_owned(),
            String::new(),
        ];
        for (key, value) in &self.environment {
            lines.push(format!("- `{key}`: `{value}`"));
        }
        lines.push(String::new());
        lines.push("## Summary".to_owned());
        lines.push(String::new());
        for (status, count) in self.counts() {
            lines.push(format!("- {status}: {count}"));
        }
        for section in [
            Section::S2sDisabled,
            Section::Flow,
            Section::Observation,
            Section::Negative,
            Section::Restart,
        ] {
            let rows: Vec<&Record> = self
                .records
                .iter()
                .filter(|record| record.section == section)
                .collect();
            if rows.is_empty() {
                continue;
            }
            lines.push(String::new());
            lines.push(format!("## {}", section_title(section)));
            lines.push(String::new());
            lines.push("| Status | Peer | Row | Driven by | Detail |".to_owned());
            lines.push("| --- | --- | --- | --- | --- |".to_owned());
            for record in rows {
                let detail: Vec<String> = record
                    .outcome
                    .checks
                    .iter()
                    .map(|check| format!("{}: {} ({})", check.status, check.name, check.detail))
                    .chain(record.outcome.notes.iter().cloned())
                    .collect();
                lines.push(format!(
                    "| {} | {} | `{}`{} {} | {} | {} |",
                    record.outcome.status,
                    record.peer,
                    record.id,
                    if record.g20 { " (G20)" } else { "" },
                    cell(&record.title),
                    cell(&record.outcome.driven_by),
                    cell(&detail.join("; "))
                ));
            }
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

fn section_title(section: Section) -> &'static str {
    match section {
        Section::S2sDisabled => "Step 6: S2S disabled",
        Section::Flow => "Flow table (steps 6 and 7)",
        Section::Observation => "Peer wire observations (W10)",
        Section::Negative => "Negative matrix (step 6; G20 rows in step 7)",
        Section::Restart => "Step 8: failure and restart",
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The most characters of one detail, reason or note the evidence keeps.
pub const DETAIL_LIMIT: usize = 1500;

/// Words that mark the value after them (`key=value`, `"key":"value"`) as a
/// credential.
const CREDENTIAL_KEYS: [&str; 10] = [
    "password",
    "passwd",
    "secret",
    "token",
    "preshared",
    "private_key",
    "privatekey",
    "api_key",
    "apikey",
    "session_key",
];

/// Make `text` fit for the evidence: connection strings and URLs with user
/// information are replaced, credential values after a credential key are
/// redacted, absolute paths keep only their last component, runs of 32 or
/// more hexadecimal digits (the run's generated passwords and secrets) are
/// redacted, and the result is cut at [`DETAIL_LIMIT`] characters.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(DETAIL_LIMIT + 16));
    let mut token = String::new();
    // A credential key whose separator ended its token (`password: value`)
    // redacts the next token.
    let mut redact_next = false;
    let flush = |token: &mut String, out: &mut String, redact_next: &mut bool| {
        if token.is_empty() {
            return;
        }
        if std::mem::take(redact_next) {
            out.push_str("[redacted]");
        } else if ends_with_separator(token) && redact_credential(token).is_some() {
            out.push_str(token);
            *redact_next = true;
        } else {
            out.push_str(&scrub_token(token));
        }
        token.clear();
    };
    for character in text.chars() {
        if character.is_whitespace() {
            flush(&mut token, &mut out, &mut redact_next);
            out.push(character);
        } else {
            token.push(character);
        }
    }
    flush(&mut token, &mut out, &mut redact_next);
    if out.chars().count() > DETAIL_LIMIT {
        let mut cut: String = out.chars().take(DETAIL_LIMIT).collect();
        cut.push_str(" [truncated]");
        return cut;
    }
    out
}

/// Whether `token` ends with a key's separator and so carries no value.
fn ends_with_separator(token: &str) -> bool {
    token.trim_end_matches(['"', '\'']).ends_with([':', '='])
}

fn scrub_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    if let Some(redacted) = redact_url(token) {
        return redacted;
    }
    if let Some(redacted) = redact_credential(token) {
        return redacted;
    }
    redact_hex(&shorten_paths(token))
}

/// A URL with user information, or any PostgreSQL connection string.
fn redact_url(token: &str) -> Option<String> {
    let marker = token.find("://")?;
    let head = token.get(..marker)?;
    let scheme_start = head
        .char_indices()
        .rev()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
        .last()
        .map_or(marker, |(index, _)| index);
    let scheme = head.get(scheme_start..)?.to_ascii_lowercase();
    let rest = token.get(marker + 3..)?;
    if rest.contains('@') || scheme.starts_with("postgres") {
        Some([head.get(..scheme_start)?, "[connection string]"].concat())
    } else {
        None
    }
}

fn redact_credential(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    CREDENTIAL_KEYS.iter().find_map(|key| {
        let at = lower.find(key)?;
        let after = at + key.len();
        let separator = lower
            .get(after..)?
            .char_indices()
            .find(|(_, character)| !matches!(character, '"' | '\'' | '_' | '-'))
            .filter(|(_, character)| matches!(character, '=' | ':'))
            .map(|(offset, _)| after + offset + 1)?;
        Some([token.get(..separator)?, "[redacted]"].concat())
    })
}

/// Replace every absolute path in `token` by its last component.
fn shorten_paths(token: &str) -> String {
    let characters: Vec<char> = token.chars().collect();
    let mut out = String::with_capacity(token.len());
    let mut index = 0;
    while let Some(&character) = characters.get(index) {
        let previous = index.checked_sub(1).and_then(|at| characters.get(at));
        let starts_path = character == '/'
            && previous.is_none_or(|previous| "=:\"'([<,".contains(*previous))
            && characters.get(index + 1).is_some_and(|next| *next != '/');
        if !starts_path {
            out.push(character);
            index += 1;
            continue;
        }
        let end = characters
            .iter()
            .skip(index)
            .position(|character| "\"'),;]>:".contains(*character))
            .map_or(characters.len(), |offset| index + offset);
        let path: String = characters
            .get(index..end)
            .unwrap_or_default()
            .iter()
            .collect();
        if path.matches('/').count() >= 2 {
            let last = path.rsplit('/').next().unwrap_or_default();
            out.push_str("[path]/");
            out.push_str(last);
        } else {
            out.push_str(&path);
        }
        index = end;
    }
    out
}

/// Redact runs of 32 or more hexadecimal digits.
fn redact_hex(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 32 {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for character in token.chars() {
        if character.is_ascii_hexdigit() {
            run.push(character);
        } else {
            flush(&mut run, &mut out);
            out.push(character);
        }
    }
    flush(&mut run, &mut out);
    out
}
