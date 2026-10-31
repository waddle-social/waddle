//! Offline schedule and observation accounting for #2046 W1.8b.
//! No execution, transport, credentials, provisioning or fault adapters exist.

mod accounting;
mod local_isolation;
mod manifest;
mod plan;

pub use accounting::{
    account, AccountingStatus, GeneratorOutcome, MessageResult, ObservationCoverage,
    ObservationSet, OfferObservation, OfferOutcome, PairOutcome, PairResult, Qualification,
    Receipt, Report, Summary,
};
pub use local_isolation::{
    AdditionalArtifact, AdditionalArtifactId, AdditionalArtifactRole, ArtifactContent,
    ArtifactRole, DeclaredArtifact, GuestArchitecture, LocalIsolationManifest, LocalIsolationPlan,
    LocalPurpose, LocalResources, LocalRunId, Sha256Digest,
};
pub use manifest::{ClientId, Manifest, Phases, RetryPolicy, Room, RoomId};
pub use plan::{MessageId, Phase, Plan, PlannedMessage, Totals};

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("invalid manifest: {0}")]
    Manifest(&'static str),
    #[error("invalid observations: {0}")]
    Observations(&'static str),
    #[error("invalid local isolation declaration: {0}")]
    LocalIsolation(&'static str),
}
