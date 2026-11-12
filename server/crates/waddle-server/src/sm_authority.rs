//! Durable XEP-0198 session authority.
//!
//! The store in this module owns the lifecycle, owner fence, counters and
//! retained frames of a stream-management session, as fixed by
//! `docs/planning/2139-durable-sm-session-model.md`. The pure decisions live
//! in `waddle_xmpp::stream_management::durable`; this module loads the stored
//! record under its row lock, applies the decision and commits the result.
//!
//! `sm_durable_sessions` is the authority record. A detached session's
//! resource snapshot is its `sm_sessions` row and its retained frames are its
//! `sm_unacked` rows, both keyed by the session key and written only inside
//! the transaction that holds the authority row lock.

mod custody;
mod enable;
mod error;
mod operations;
mod record;
mod schema;
mod selection;
mod store;

pub use custody::{
    AckSettlement, AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection,
    AdmissionRequest, CommittedObligation, OwnerError, OwnerRejection, RetainedFrame,
};
pub use enable::{
    EnableError, EnableOperation, EnableRejection, EnableRequest, EnableResolution, EnabledCommit,
};
pub use error::DurableSmError;
pub use record::{DurableSessionRecord, RecordedClaim};
pub use schema::bootstrap_schema_owner as bootstrap_durable_sm_owner_schema;
pub use selection::{DurableSmMode, DurableSmSelection, InvalidDurableSmMode, DURABLE_SM_MODE_ENV};
pub use store::DurableSmStore;
