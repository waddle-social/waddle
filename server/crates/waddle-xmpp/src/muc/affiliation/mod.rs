//! MUC Affiliation Sync with Zanzibar Permissions
//!
//! This module implements the synchronization between Waddle's Zanzibar-based
//! permission system and XMPP MUC (Multi-User Chat) affiliations.
//!
//! Affiliation resolution is driven by `AffiliationResolver` (and the
//! `AppStateAffiliationResolver` backed by the application's permission
//! graph). Federation policy / per-domain or per-JID overrides live in
//! `FederatedAffiliationConfig`.
//!
//! ## Example
//!
//! ```ignore
//! use waddle_xmpp::muc::affiliation::AffiliationResolver;
//!
//! // Check affiliation for a user joining a room
//! let affiliation = resolver.resolve_affiliation(&user_id, &channel_id).await?;
//! ```

mod config;
mod list;
mod membership;
mod resolver;

pub use config::{FederatedAffiliationConfig, FederatedPermissionPolicy};
pub use list::{AffiliationChange, AffiliationEntry, AffiliationList};
pub use membership::{DurableMembershipFuture, DurableMembershipSource};
pub use resolver::{AffiliationResolver, AppStateAffiliationResolver};

#[cfg(test)]
mod tests;
