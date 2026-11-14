//! The `waddle-identity-v1` identity profile: canonical domains, canonical
//! addresses and the account localpart profile.
//!
//! The decisions and their rationale are recorded in
//! `server/docs/planning/identity-profile-2047.md`. In short:
//!
//! - A domain has one canonical form: one trailing label separator removed
//!   (RFC 7622 §3.2), DNS names prepared under IDNA2008 through UTS #46
//!   non-transitional processing with STD3 ASCII rules, and written as
//!   lowercase A-labels. The U-label and A-label spellings of a domain are
//!   the same [`CanonicalDomain`]. IP literals keep their canonical text.
//!   Each canonical domain has one [`DomainKind`]: a fully qualified DNS
//!   name, a single-label name, a numeric IPv4 form such as `0x7f.1`, or an
//!   IP literal. Only a fully qualified DNS name can name a federation peer;
//!   the other kinds are valid addresses that admission, routing and
//!   discovery refuse by kind.
//! - Waddle never folds an address it is given. An XML-decoded address is
//!   accepted only when the `jid` parser would leave its localpart and
//!   resourcepart byte-for-byte unchanged and its domainpart is written in
//!   a canonical spelling. Spellings that preparation would change (case,
//!   width, compatibility forms, `ß`, final sigma, ignorable code points)
//!   are refused, so two accepted spellings name the same entity only when
//!   they are identical apart from the domain's label form or a trailing
//!   dot.
//! - Accounts that this deployment provisions use the narrower account
//!   profile ([`AccountLocalpart`]).
//!
//! The acceptance set depends on the `jid`, `stringprep` and `idna` releases
//! the server links; `waddle-server` pins `jid` exactly and `Cargo.lock`
//! fixes the others. Every check compares against the linked parser's own
//! output, so an accepted address is always parser-stable.
//!
//! Errors carry no input material.

mod account;
mod address;
mod domain;
mod error;
mod stanza;

pub use account::{AccountLocalpart, ACCOUNT_LOCALPART_MAX_BYTES};
pub use address::{
    canonical_jid, parse_bare_jid, parse_full_jid, parse_jid, parse_localpart, parse_resourcepart,
    CanonicalBareJid,
};
pub use domain::{CanonicalDomain, DomainKind};
pub use error::IdentityError;
pub use stanza::enforce_stanza_addresses;

/// The identity profile version this module implements.
pub const IDENTITY_PROFILE: &str = "waddle-identity-v1";
