//! The evidence log: one record per manifest row, each PASS, FAIL or
//! UNVERIFIED, written as JSON and Markdown under `docs/evidence/2047/`.
//!
//! A row is PASS only when every check it ran passed and it ran at least
//! one. A row that was not driven, or that hit a missing prerequisite, is
//! UNVERIFIED with the reason, never PASS.

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
            driven_by: driven_by.into(),
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
            name: name.into(),
            status: if passed { Status::Pass } else { Status::Fail },
            detail: detail.into(),
        });
    }

    /// Record a part of the row that could not be established.
    pub fn unverified(&mut self, name: impl Into<String>, reason: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            status: Status::Unverified,
            detail: reason.into(),
        });
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
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
