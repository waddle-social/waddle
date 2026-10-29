use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Candidate {
    Postgres,
    SpiceDb,
}

impl Candidate {
    pub fn storage(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::SpiceDb => "spicedb",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Polarity {
    Grant,
    Revoke,
}

impl Polarity {
    pub fn storage(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
    pub fn relation(self) -> &'static str {
        match self {
            Self::Grant => "reader",
            Self::Revoke => "tombstone",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeafRelation {
    Reader,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub subject: Uuid,
    pub relation: LeafRelation,
    pub polarity: Polarity,
}

impl Intent {
    pub fn grant() -> Self {
        Self {
            subject: Uuid::new_v4(),
            relation: LeafRelation::Reader,
            polarity: Polarity::Grant,
        }
    }
    pub fn revoke(&self) -> Self {
        Self {
            polarity: Polarity::Revoke,
            ..self.clone()
        }
    }
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(
            format!(
                "v1:reader:{}:{}",
                self.subject.simple(),
                self.polarity.storage()
            )
            .as_bytes(),
        )
        .into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub candidate: Candidate,
    pub scope: Uuid,
    pub incarnation: Uuid,
    pub generation: i64,
    pub operation: Uuid,
    pub digest: [u8; 32],
}

impl Identity {
    pub fn fresh(candidate: Candidate, intent: &Intent) -> Self {
        Self {
            candidate,
            scope: Uuid::new_v4(),
            incarnation: Uuid::new_v4(),
            generation: 1,
            operation: Uuid::new_v4(),
            digest: intent.digest(),
        }
    }
    pub fn next(&self, intent: &Intent) -> Result<Self> {
        Ok(Self {
            generation: self
                .generation
                .checked_add(1)
                .context("recovery generation overflow")?,
            operation: Uuid::new_v4(),
            digest: intent.digest(),
            ..self.clone()
        })
    }
    pub fn digest_hex(&self) -> String {
        hex::encode(self.digest)
    }
    pub fn graph_id(&self) -> String {
        format!(
            "v2_{}_{}_{}_{}_{}_{}",
            self.candidate.storage(),
            self.scope.simple(),
            self.incarnation.simple(),
            self.generation,
            self.operation.simple(),
            self.digest_hex()
        )
    }
    pub fn validate(&self, intent: &Intent) -> Result<()> {
        ensure!(
            self.generation > 0 && self.digest == intent.digest(),
            "exact recovery identity/payload digest mismatch"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fact {
    AdmissionClosed,
    CanonicalMutationCommitted,
    ProjectionVerified,
}

impl Fact {
    pub fn storage(self) -> &'static str {
        match self {
            Self::AdmissionClosed => "admission_closed",
            Self::CanonicalMutationCommitted => "canonical_committed",
            Self::ProjectionVerified => "projection_verified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub identity: Identity,
    pub fact: Fact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pause {
    None,
    BeforeFenceCommit,
    AfterAcknowledgedWrite,
    BeforeActivation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RecoveryAction {
    Mutate {
        intent: Intent,
        previous: Option<Identity>,
        pause: Pause,
        write_proxy_port: Option<u16>,
    },
    Recover {
        pause: Pause,
    },
    Admit {
        pause_after_check: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RecoveryWork {
    pub settings: Settings,
    pub identity: Identity,
    pub action: RecoveryAction,
    pub attempt: Uuid,
    pub barriers: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Activated,
    VerifiedRevoke,
    AuthorityUnavailable,
    CanonicalMissing,
    Stale,
    Admitted,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteFailure {
    CallerDeadlineExceeded,
    GrpcStatus(i32),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signal {
    WriteFailed(WriteFailure),
    FenceHeld,
    AdmissionClosed,
    AcknowledgedWrite,
    ProjectionVerified,
    BeforeActivation,
    ActivationAttempt,
    Checked,
    Done(Outcome),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub identity: Identity,
    pub signal: Signal,
    pub backend_pid: Option<i32>,
}

#[derive(Clone, Copy)]
pub enum Component {
    Marker,
    Leaf(Polarity),
}

impl Component {
    pub fn relation(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::Leaf(polarity) => polarity.relation(),
        }
    }
}
