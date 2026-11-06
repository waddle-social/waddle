//! Dark repository for one prepared local direct frame per captured SM session.
//!
//! The schema is unregistered and no live writer selects this module. A captured
//! endpoint is equality evidence supplied by the caller, not proof of publication.
//! Operations retain full payloads independently of the bounded replay cache.

mod admission;
mod handoff;
mod repository;
mod schema;
mod settlement;
mod types;

pub use handoff::{
    CandidateHydration, CandidateSmMemory, CommittedPreacceptHandoff, HandoffFailure,
    HandoffRecovery, HandoffStage,
};
pub use repository::SmCustodyRepository;
pub use schema::candidate_postgres_ddl;
pub use types::{
    CommittedAdmission, CommittedSlot, CustodyCapture, CustodyError, CustodyInventoryPage,
    CustodyLimits, CustodyOperation, CustodyRecoveryAuthority, CustodyResolution, CustodyTerminal,
    PreparedDirectFrame, SealedSession, SessionGeneration,
};
