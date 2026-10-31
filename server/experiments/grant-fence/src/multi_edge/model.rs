use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    Lifecycle,
    Binding,
    Ancestor,
    Direct,
}
impl Kind {
    pub fn sql(self) -> i16 {
        match self {
            Self::Lifecycle => 0,
            Self::Binding => 1,
            Self::Ancestor => 2,
            Self::Direct => 3,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    Lifecycle(Uuid),
    Binding(Uuid),
    Ancestor(bool),
    Direct(bool),
}
impl Payload {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Lifecycle(_) => Kind::Lifecycle,
            Self::Binding(_) => Kind::Binding,
            Self::Ancestor(_) => Kind::Ancestor,
            Self::Direct(_) => Kind::Direct,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub id: Uuid,
    pub generation: i64,
    pub operation: Uuid,
    pub payload: Payload,
}
impl Guard {
    pub fn kind(&self) -> Kind {
        self.payload.kind()
    }
    pub fn marker_id(&self) -> String {
        format!("k{}_{}", self.kind().sql(), self.id.simple())
    }
    pub fn next(&self, payload: Payload) -> Result<Self> {
        anyhow::ensure!(payload.kind() == self.kind(), "guard kind cannot change");
        Ok(Self {
            id: self.id,
            generation: self.generation.checked_add(1).context("version overflow")?,
            operation: Uuid::new_v4(),
            payload,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Branch {
    Inherited,
    Direct,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub channel: Uuid,
    pub principal: Uuid,
    pub branch: Branch,
    pub guards: Vec<Guard>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectAction {
    InspectSyntheticResource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub key: Uuid,
    pub action: EffectAction,
    pub payload: Uuid,
    pub witness: Witness,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pause {
    #[default]
    None,
    Anchored,
    Checked,
    Reserved,
    Locked,
    Committed,
    MutationLocked,
    Closed,
    Published,
}
/// Deliberately unsafe omissions exist only as named experimental controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    Full,
    OmitGuard(Kind),
    OmitVersionStamp(Kind),
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Action {
    Admit {
        effect: Effect,
        omit: Control,
        pause: Pause,
        lock_ms: u32,
    },
    Mutate {
        previous: Guard,
        next: Guard,
        principal: Uuid,
        pause: Pause,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Work {
    pub settings: Settings,
    pub barriers: PathBuf,
    pub action: Action,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Admitted,
    AlreadyCommitted,
    Conflict,
    DeniedGraph,
    DeniedFence,
    LockTimeout,
    Activated,
    Superseded,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub outcome: Option<Outcome>,
    pub pid: i32,
}
#[derive(Serialize)]
pub struct Case {
    pub name: &'static str,
    pub passed: bool,
}
