//! Admin protocol namespace constants for Waddle V1.
//!
//! The admin command surface is XEP-0050 ad-hoc commands under
//! `urn:waddle:admin:*`. We mint our own namespace prefix
//! because no XEP defines a "list users with prefix search"
//! command and `urn:waddle:*` is honestly ours (no XSF
//! registration claimed). The wire shape of the commands and the
//! data forms they carry is plain XEP-0050 + XEP-0004 — the only
//! Waddle-specific bit is the node identifier and the FORM_TYPE.
//!
//! Constants live here so the disco-feature advertisement in
//! `crate::disco::info::server_features` and the command handler
//! in `waddle-server`'s `crate::admin::users_list` reference the
//! same string, preventing the "feature advert and handler node
//! drifted apart" class of bugs.

/// XEP-0050 node identifier and disco feature URI for the
/// admin users-list command. The same string serves both roles
/// per XEP-0050 §"Discovering Support" — a client that wants to
/// know whether `urn:waddle:admin:users:list:0` is
/// available can either disco#info the server for the feature
/// var or disco#items the commands node and look for the entry.
pub const NS_ADMIN_USERS_LIST: &str = "urn:waddle:admin:users:list:0";
