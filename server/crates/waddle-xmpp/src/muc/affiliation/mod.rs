//! MUC Affiliation Sync with Zanzibar Permissions
//!
//! This module implements the synchronization between Waddle's Zanzibar-based
//! permission system and XMPP MUC (Multi-User Chat) affiliations.
//!
//! ## Permission to Affiliation Mapping
//!
//! Per RFC-0002, Waddle permissions map to MUC affiliations as follows:
//! - `owner` -> Owner (highest privilege, can configure room)
//! - `admin` -> Admin (can manage members, kick users)
//! - `moderator` -> Admin (same as admin for MUC purposes)
//! - `member` -> Member (can join members-only rooms)
//! - `viewer` -> Member (read-only access maps to Member)
//! - No permission -> None (blocked from members-only rooms)
//!
//! ## Example
//!
//! ```ignore
//! use waddle_xmpp::muc::affiliation::{AffiliationResolver, PermissionMapper};
//!
//! // Check affiliation for a user joining a room
//! let affiliation = resolver.resolve_affiliation(&user_id, &channel_id).await?;
//! ```

mod config;
mod list;
mod mapper;
mod resolver;

pub use config::{FederatedAffiliationConfig, FederatedPermissionPolicy};
pub use list::{AffiliationChange, AffiliationEntry, AffiliationList};
pub use mapper::PermissionMapper;
pub use resolver::{AffiliationResolver, AppStateAffiliationResolver};

#[cfg(test)]
mod tests;
