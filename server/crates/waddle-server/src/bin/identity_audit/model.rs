use serde::Serialize;
use thiserror::Error;

pub const PROFILE: &str = "candidate-ascii-account-v1";
pub const FIELD_BYTES: usize = 4096;
pub const RETAINED_BYTES: usize = 16 * 1024 * 1024;

/// Unprepared storage/input evidence. This is deliberately not a parsed JID.
/// No Debug implementation: these values belong only in the private report.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OriginalValue(String);

impl OriginalValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    Oidc,
    Native,
}

#[derive(Serialize)]
#[serde(tag = "source", content = "key", rename_all = "snake_case")]
pub enum RecordKey {
    Oidc(OriginalValue),
    Native(i64),
}

#[derive(Serialize)]
pub struct AccountRow {
    pub record: RecordKey,
    pub localpart: OriginalValue,
    pub domain: OriginalValue,
    pub stored_key: Option<OriginalValue>,
    #[serde(skip)]
    pub source: AccountSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    MissingPreparedKey,
    IncompatiblePreparedKey,
    SourceJidDisagrees,
    InvalidCurrentParserIdentity,
    ChangedByCurrentParser,
    PreparationPathsDisagree,
    UnicodeInput,
    CandidateLocalpart,
    CandidateDomain,
    CandidateALabel,
    CandidateTrailingDot,
    ResourceAddress,
    ServiceAddress,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditError {
    #[error("invalid_configuration")]
    Configuration,
    #[error("database_connection_failed")]
    Connection,
    #[error("unsupported_or_absent_schema")]
    Schema,
    #[error("database_read_incomplete")]
    Read,
    #[error("audit_limit_exceeded")]
    Limit,
    #[error("audit_timed_out")]
    Timeout,
    #[error("frame_corpus_incomplete")]
    Frames,
    #[error("private_report_unavailable")]
    Report,
}
