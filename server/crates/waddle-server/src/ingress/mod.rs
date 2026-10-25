//! Durable ingress authority: immutable planning, atomic commit, bounded execution.
use crate::server::routes::interpret::DeliveryExecutionContext;
pub(crate) mod append_authority;
mod archive_authority;
mod archive_dispatch;
pub(crate) use archive_dispatch::{socket_delivery_readiness, DispatchProbeBudget};
mod capture;
mod pending_dispatch;
mod reflection_dispatch;
#[cfg(test)]
pub(crate) use capture::TEST_CAPTURE_LIMIT;
pub use capture::{IngressEffectCapture, IngressEffectCaptureSnapshot};
pub mod commit;
mod commit_room;
mod commit_stream;
pub mod decision;
mod durable;
pub(crate) use durable::receipt_key;
pub mod execute;
mod execute_uow;
pub(crate) use execute_uow::ambiguous_offline_pending_id;
mod frame_receipt_retry;
pub(crate) mod gc;
pub mod identity;
mod invitation_authority;
pub(crate) mod live_delivery;
pub mod nested;
pub mod principal;
pub(crate) mod room_manifest_replay;
pub use principal::{ExtensionPrincipal, IngressPrincipal};
pub(crate) mod maintenance;
mod receipts;
mod recovery;
mod recovery_departed;
mod recovery_environment;
mod recovery_executor;
mod recovery_ghosts;
mod recovery_reachability;
mod recovery_rebuild;
pub use recovery::{RecoveryPolicyDecision, RecoveryPreparation, RecoverySweepOutcome};
use recovery_environment::RecoveryBinding;
pub use recovery_environment::RecoveryEnvironment;
mod recorded;
pub(crate) mod room_manifest;
pub use recorded::{ProgressObligation, RouteProgress};
pub use room_manifest::{
    FrozenRoomManifest, RoomManifestChild, RoomManifestChildDisposition, RoomManifestSnapshot,
};
mod rejection;
pub mod restamp;
mod restore_offline;
pub mod submission;
pub mod suppression;

pub use crate::server::routes::interpret::{
    effects, plan_message_dispatch, Deps, HostOwnedResources,
};
pub use decision::{AliasOutcomeClass, EffectReceiptKey, IngressDecision, IngressDecisionClass};
pub use effects::{
    DurableEffect, ExternalEffect, ImmediateSink, IngressPlan, PlanSuppressionPolicy,
    PlannedEffect, RoomExecutionPath,
};
pub use execute::{ExecutionReport, ExternalOutcome, FrameObligation};
pub use identity::{IngressCanonicalRef, IngressStreamIdentity};
pub use submission::IngressSubmission;

use crate::config::{IngressConfig, IngressConfigError, LineageConfig};
use crate::db::DatabaseDriver;
use crate::db::{Database, DatabaseConfig, DatabaseError};
use crate::ingress_uow::{
    IngressUnitOfWork, IngressUowError, SmIngressRepository, SmIngressStreamRepository,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ingress::{SmIngressId, WireHandledCount};
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::SharedNodeIdentity;
use waddle_xmpp::pending_delivery::SmSessionId;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod room_pin_tests;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod subject_receipt_tests;

#[cfg(test)]
pub(crate) mod recovery_tests;

#[cfg(test)]
mod offline_settlement_tests;

#[cfg(test)]
mod offline_hardening_tests;

#[cfg(test)]
mod muc_occupant_progress_tests;

#[cfg(test)]
mod xep0045_room_manifest_tests;

mod room_canonical;
pub(crate) mod storage_hint;

mod contract;
use contract::non_advancing;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use contract::test_lineage_config;
pub use contract::IngressAuthority;
pub use contract::IngressRetirementOutcome;
pub use contract::IngressStartupError;
use contract::RETIREMENT_BATCH_SIZE;
