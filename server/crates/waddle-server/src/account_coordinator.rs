//! The account coordinator: one authority per bare JID for that account's
//! resources.
//!
//! A coordinator owns the pure slot state of one account
//! (`waddle_xmpp::ownership::account_coordinator::AccountResources`) and
//! commits every mutation through the durable binding store
//! (`crate::account_bindings`) before it applies it in memory, so its
//! selection never runs ahead of the stored rows. It is created on demand
//! under the account's current `UserClaimIdentity` and reclaimed when the
//! account holds no binding and has been idle. The model is fixed by
//! `docs/planning/2138-account-coordinator.md`.
//!
//! [`AccountCoordinators`] is the directory of coordinators on this node.
//! A connection reaches it through [`AccountBindingAuthority`]; routing
//! reads it through [`AccountCoordinators::view`], and only when
//! [`AccountAuthoritySelection`] selects the coordinator. The default
//! selection is the connection registry, which no coordinator feeds.

mod actor;
mod authority;
mod claims;
mod config;
mod directory;
mod error;
mod messages;
mod reads;
mod selection;

pub use authority::AccountBindingAuthority;
pub use claims::{
    AccountClaimError, AccountClaimSource, BindingLiveness, ClaimStoreClaimSource,
    ClusterLeaseLiveness, ConnectionLiveness, LocalNodeLiveness, ProcessClaimSource,
};
pub use config::AccountCoordinatorConfig;
pub use directory::AccountCoordinators;
pub use error::AccountCoordinatorError;
pub use messages::{
    AccountView, AdoptionReport, BindingReply, ReleaseSessionRequest, RepairEvidence,
    RepairOutcome, SessionRelease, SlotStatus, UnregisterOutcome, UnregisterRequest,
};
pub use selection::{
    AccountAuthorityMode, AccountAuthoritySelection, AccountAuthoritySelectionError,
    InvalidAccountAuthorityMode, ACCOUNT_AUTHORITY_MODE_ENV,
};
