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
//! A connection reaches its account's coordinator through
//! [`RoutedAccountAuthority`], which serves a request here when this node
//! owns the account and sends it to the owning node otherwise; routing
//! reads accounts the same way. Startup always selects the coordinator
//! ([`AccountAuthoritySelection`]); it is the only account-resource
//! authority.

mod actor;
mod authority;
mod claims;
mod config;
mod directory;
mod error;
mod messages;
mod mirror_claims;
mod publisher;
mod reads;
mod routed;
mod selection;

pub use authority::AccountBindingAuthority;
pub use claims::{
    AccountClaimError, AccountClaimSource, BindingLiveness, ClaimRelease, ClaimStoreClaimSource,
    ClusterLeaseLiveness, ConnectionLiveness, LocalNodeLiveness, MirrorClaimHolds, NodeLeaseRead,
    ProcessClaimSource,
};
pub use config::AccountCoordinatorConfig;
pub use directory::AccountCoordinators;
pub use error::AccountCoordinatorError;
pub use messages::{
    AccountView, AdoptionReport, BindingReply, ReleaseSessionRequest, RepairEvidence,
    RepairOutcome, SessionRelease, SlotStatus, UnregisterOutcome, UnregisterRequest,
};
pub use mirror_claims::{CoordinatorLookup, MirrorClaimStore};
pub use publisher::{
    ConfirmOutcome, CoordinatorBindingPublisher, PublishOutcome, PublishedBinding, RetireEvidence,
    RetireOutcome,
};
pub use routed::{
    AccountAuthorityAnswer, AccountAuthorityPeers, AccountAuthorityRequest, AccountOwner,
    AccountRoutingPolicy, ForwardFailure, RoutedAccountAuthority,
};
pub use selection::{
    warn_if_removed_selector_set, AccountAuthoritySelection, AccountAuthoritySelectionError,
    REMOVED_ACCOUNT_AUTHORITY_ENV,
};
