//! Reconciliation of the local account stores with the identity profile
//! (`waddle_xmpp::identity`, decisions in
//! `server/docs/planning/identity-profile-2047.md`).
//!
//! The account stores are the authority for which accounts exist and how
//! each is named. The profile changes how inputs are matched against them:
//! a name is taken exactly, never folded. Accounts written before the
//! profile can hold names that it refuses or that fold onto another
//! account's name. [`reconcile_identity_profile`] reads both stores and
//! reports every such account with the consequence the profile has for it;
//! it changes nothing. Startup logs the counts only
//! ([`log_identity_profile_report`]); the report itself carries account
//! identifiers and is for an operator's private use.
//!
//! Bookmark items (XEP-0503 Space items, XEP-0402 and Waddle DM bookmarks)
//! are keyed by a bare JID taken exactly as well. [`reconcile_bookmark_item_ids`]
//! counts the stored items whose id is not canonical, which only a retract
//! of that exact id removes; startup logs those counts too
//! ([`log_bookmark_item_report`]).

use std::collections::BTreeMap;

use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::{info, warn};
use waddle_xmpp::identity::{
    AccountLocalpart, CanonicalBareJid, CanonicalDomain, IdentityError, IDENTITY_PROFILE,
};
use waddle_xmpp::xep::xep0402::PEP_NODE as PEP_BOOKMARKS_NODE;
use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

use crate::db::actor::{DbActor, DbQuery};
use crate::db::{row_value, Database, Value, ValueExt};

use super::directory::{canonical_account_jid, canonical_localpart};
use super::AuthError;

/// The account domain and service domains this deployment serves, each of
/// which must already be written in canonical form. Every account's bound
/// JID and every served address is built from this text, so a configured
/// domain that preparation would change (`Example.com`, a trailing dot or a
/// U-label domain) is refused at startup instead of being rewritten.
pub fn require_canonical_domains(
    account_domain: &str,
    service_domains: &[&str],
) -> Result<CanonicalDomain, IdentityError> {
    let account = CanonicalDomain::parse_canonical(account_domain)?;
    for service in service_domains {
        CanonicalDomain::parse_canonical(service)?;
    }
    Ok(account)
}

/// An account row, identified by its store key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccountRecord {
    /// An OIDC account, by its stored bare JID (the `users` primary key).
    Oidc { jid: String },
    /// A native account, by its row id.
    Native { id: i64 },
}

/// How an account stands against the identity profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccountFinding {
    /// The stored name, or the stored bare JID of an OIDC account, is not
    /// the canonical address of the stored name: preparation would change
    /// it. An OIDC account cannot sign in over XMPP; a native account signs
    /// in only under the canonical spelling its lookup key records.
    NonCanonicalSpelling,
    /// The name is canonical but outside the account profile, for example
    /// non-ASCII or longer than 64 bytes. The account keeps signing in;
    /// no new account can be provisioned with such a name.
    OutsideAccountProfile,
    /// The row has no lookup key. Lookups and sign-in miss it until the
    /// periodic backfill keys it.
    MissingKey,
    /// The recorded lookup key is not the key of the stored name.
    KeyMismatch,
    /// Two or more accounts claim one address, across both stores: their
    /// stored names fold to it, or an identity provider provisioned an OIDC
    /// account under a native account's exact name. At most one of them can
    /// hold that address. A native account and an OIDC row with its exact
    /// name that no identity provider provisioned (a `users` row with no
    /// `auth_identities` row, such as one written for that native account)
    /// are one account, not a collision.
    FoldedCollision,
    /// A native account whose domain is not this deployment's account
    /// domain. It is not an account of this deployment.
    ForeignDomain,
}

impl AccountFinding {
    /// Every finding, in report order.
    pub const ALL: [AccountFinding; 6] = [
        AccountFinding::NonCanonicalSpelling,
        AccountFinding::OutsideAccountProfile,
        AccountFinding::MissingKey,
        AccountFinding::KeyMismatch,
        AccountFinding::FoldedCollision,
        AccountFinding::ForeignDomain,
    ];

    /// A stable code for logs and reports.
    pub fn code(self) -> &'static str {
        match self {
            Self::NonCanonicalSpelling => "non_canonical_spelling",
            Self::OutsideAccountProfile => "outside_account_profile",
            Self::MissingKey => "missing_key",
            Self::KeyMismatch => "key_mismatch",
            Self::FoldedCollision => "folded_collision",
            Self::ForeignDomain => "foreign_domain",
        }
    }
}

/// One account with at least one finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountReconciliation {
    pub record: AccountRecord,
    pub findings: Vec<AccountFinding>,
}

/// The accounts outside the identity profile, from one pass over both
/// account stores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentityProfileReport {
    /// Rows read across both stores.
    pub accounts: usize,
    /// Accounts with findings, ordered by record.
    pub outside: Vec<AccountReconciliation>,
}

impl IdentityProfileReport {
    /// Whether every account is inside the profile.
    pub fn is_clean(&self) -> bool {
        self.outside.is_empty()
    }

    /// How many accounts have `finding`.
    pub fn count(&self, finding: AccountFinding) -> usize {
        self.outside
            .iter()
            .filter(|account| account.findings.contains(&finding))
            .count()
    }

    /// The findings of one account, if it has any.
    pub fn findings(&self, record: &AccountRecord) -> Option<&[AccountFinding]> {
        self.outside
            .iter()
            .find(|account| &account.record == record)
            .map(|account| account.findings.as_slice())
    }
}

/// A row that holds a stored name.
#[derive(Clone, Debug)]
struct NameHolder {
    record: AccountRecord,
    /// An OIDC row with an `auth_identities` row: an identity provider
    /// provisioned it, so it is an account of its own even when a native
    /// account has the same name. A row without one mirrors the native
    /// account of that name.
    provisioned_by_idp: bool,
}

/// Rows grouped by the address their stored names fold to, and within that
/// by the exact stored name and domain. More than one exact name in a group
/// is a collision. Rows with the same exact name are one account, unless an
/// identity provider provisioned the OIDC row: then it is a separate
/// account that shares the native account's name, also a collision.
type FoldedNames = BTreeMap<String, BTreeMap<(String, String), Vec<NameHolder>>>;

fn exact_name_is_shared(holders: &[NameHolder]) -> bool {
    let native = holders
        .iter()
        .any(|holder| matches!(holder.record, AccountRecord::Native { .. }));
    native && holders.iter().any(|holder| holder.provisioned_by_idp)
}

/// Read both account stores and report every account outside the identity
/// profile at `domain`, this deployment's canonical account domain. Read
/// only; the two stores are read one after the other, not in one snapshot.
pub async fn reconcile_identity_profile(
    actor: &ActorRef<DbActor>,
    domain: &CanonicalDomain,
) -> Result<IdentityProfileReport, AuthError> {
    let mut findings: BTreeMap<AccountRecord, Vec<AccountFinding>> = BTreeMap::new();
    let mut folded = FoldedNames::new();
    let mut accounts = 0;

    let users = query(
        actor,
        "SELECT jid, xmpp_localpart, localpart_key, \
         CASE WHEN EXISTS (SELECT 1 FROM auth_identities WHERE auth_identities.user_jid = users.jid) \
         THEN 1 ELSE 0 END \
         FROM users",
    )
    .await?;
    for row in users {
        accounts += 1;
        let stored_jid = text(&row, 0)?;
        let localpart = text(&row, 1)?;
        let key = optional_text(&row, 2)?;
        let provisioned_by_idp = integer(&row, 3)? != 0;
        let record = AccountRecord::Oidc {
            jid: stored_jid.clone(),
        };
        let mut found = Vec::new();
        match CanonicalBareJid::from_parts(&localpart, domain) {
            Ok(jid) if jid.as_str() == stored_jid => {
                if AccountLocalpart::parse(&localpart).is_err() {
                    found.push(AccountFinding::OutsideAccountProfile);
                }
            }
            _ => found.push(AccountFinding::NonCanonicalSpelling),
        }
        let expected_key = canonical_localpart(&localpart);
        match key {
            None => found.push(AccountFinding::MissingKey),
            Some(key) if expected_key.as_ref().map(|k| k.as_str()) != Some(key.as_str()) => {
                found.push(AccountFinding::KeyMismatch)
            }
            Some(_) => {}
        }
        if let Some(jid) = canonical_account_jid(&localpart, domain.as_str()) {
            folded
                .entry(jid.to_string())
                .or_default()
                .entry((localpart.clone(), domain.as_str().to_owned()))
                .or_default()
                .push(NameHolder {
                    record: record.clone(),
                    provisioned_by_idp,
                });
        }
        if !found.is_empty() {
            findings.entry(record).or_default().extend(found);
        }
    }

    let native = query(
        actor,
        "SELECT id, username, domain, jid_key FROM native_users ORDER BY id",
    )
    .await?;
    for row in native {
        accounts += 1;
        let id = integer(&row, 0)?;
        let username = text(&row, 1)?;
        let row_domain = text(&row, 2)?;
        let key = optional_text(&row, 3)?;
        let record = AccountRecord::Native { id };
        let mut found = Vec::new();
        let prepared_domain = CanonicalDomain::prepare(&row_domain).ok();
        if prepared_domain.as_ref() != Some(domain) {
            found.push(AccountFinding::ForeignDomain);
        }
        let canonical = prepared_domain
            .as_ref()
            .filter(|prepared| prepared.as_str() == row_domain)
            .and_then(|prepared| CanonicalBareJid::from_parts(&username, prepared).ok());
        match canonical {
            Some(_) => {
                if AccountLocalpart::parse(&username).is_err() {
                    found.push(AccountFinding::OutsideAccountProfile);
                }
            }
            None => found.push(AccountFinding::NonCanonicalSpelling),
        }
        let expected_key = canonical_account_jid(&username, &row_domain);
        match key {
            None => found.push(AccountFinding::MissingKey),
            Some(key) if expected_key.as_ref().map(|k| k.as_str()) != Some(key.as_str()) => {
                found.push(AccountFinding::KeyMismatch)
            }
            Some(_) => {}
        }
        if let Some(jid) = expected_key {
            folded
                .entry(jid.to_string())
                .or_default()
                .entry((username.clone(), row_domain.clone()))
                .or_default()
                .push(NameHolder {
                    record: record.clone(),
                    provisioned_by_idp: false,
                });
        }
        if !found.is_empty() {
            findings.entry(record).or_default().extend(found);
        }
    }

    for names in folded.into_values() {
        let colliding =
            names.len() > 1 || names.values().any(|holders| exact_name_is_shared(holders));
        if !colliding {
            continue;
        }
        for holder in names.into_values().flatten() {
            findings
                .entry(holder.record)
                .or_default()
                .push(AccountFinding::FoldedCollision);
        }
    }

    Ok(IdentityProfileReport {
        accounts,
        outside: findings
            .into_iter()
            .map(|(record, mut findings)| {
                findings.sort();
                findings.dedup();
                AccountReconciliation { record, findings }
            })
            .collect(),
    })
}

/// Log a report's counts. No account identifier is logged.
pub fn log_identity_profile_report(report: &IdentityProfileReport) {
    if report.is_clean() {
        info!(
            profile = IDENTITY_PROFILE,
            accounts = report.accounts,
            "every local account is inside the identity profile"
        );
        return;
    }
    warn!(
        profile = IDENTITY_PROFILE,
        accounts = report.accounts,
        outside = report.outside.len(),
        non_canonical_spelling = report.count(AccountFinding::NonCanonicalSpelling),
        outside_account_profile = report.count(AccountFinding::OutsideAccountProfile),
        missing_key = report.count(AccountFinding::MissingKey),
        key_mismatch = report.count(AccountFinding::KeyMismatch),
        folded_collision = report.count(AccountFinding::FoldedCollision),
        foreign_domain = report.count(AccountFinding::ForeignDomain),
        "local accounts outside the identity profile; see docs/operations/identity-profile-rollout.md"
    );
}

/// A PubSub carrier whose item ids are bare JIDs that key grants, links or
/// notification settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BookmarkCarrier {
    /// XEP-0503 Space nodes on the Spaces service: each item id is a room,
    /// and the item gives that channel a parent grant on the node.
    Spaces,
    /// XEP-0402 PEP bookmarks (`urn:xmpp:bookmarks:1`): each item id is a
    /// room, and the item feeds the owner's notification settings for it.
    PepBookmarks,
    /// Waddle DM bookmarks (`urn:waddle:dm-bookmarks:0`): each item id is a
    /// contact, and the item feeds the owner's notification settings for it.
    DmBookmarks,
}

impl BookmarkCarrier {
    /// Every carrier, in report order.
    pub const ALL: [BookmarkCarrier; 3] = [
        BookmarkCarrier::Spaces,
        BookmarkCarrier::PepBookmarks,
        BookmarkCarrier::DmBookmarks,
    ];

    /// A stable code for logs and reports.
    pub fn code(self) -> &'static str {
        match self {
            Self::Spaces => "spaces",
            Self::PepBookmarks => "pep_bookmarks",
            Self::DmBookmarks => "dm_bookmarks",
        }
    }
}

/// Stored bookmark items whose id is not the canonical text of a bare JID,
/// from one pass over the PubSub store.
///
/// Such an item was stored as written before item ids were held to the
/// identity profile. Publishing it again under that id is refused; its
/// owner (or a Space owner, for a Space item) removes it by retracting that
/// exact id, which also removes the Space parent grant or notification
/// setting it held unless an item under the canonical id still holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookmarkItemReport {
    /// Items read across every carrier.
    pub items: usize,
    non_canonical: BTreeMap<BookmarkCarrier, usize>,
}

impl BookmarkItemReport {
    /// Whether every item id is canonical.
    pub fn is_clean(&self) -> bool {
        self.non_canonical.is_empty()
    }

    /// How many items of `carrier` have a non-canonical id.
    pub fn non_canonical(&self, carrier: BookmarkCarrier) -> usize {
        self.non_canonical.get(&carrier).copied().unwrap_or(0)
    }
}

/// Read the item ids of every bookmark carrier from the PubSub store
/// `pubsub` and count those [`CanonicalBareJid::parse_exact`] refuses.
/// `spaces_service` is the Spaces service that owns the Space nodes. Read
/// only; nothing is changed.
pub async fn reconcile_bookmark_item_ids(
    pubsub: &Database,
    spaces_service: &BareJid,
) -> Result<BookmarkItemReport, AuthError> {
    let spaces_service = spaces_service.to_string();
    let connection = pubsub
        .guard()
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    let mut rows = connection
        .query(
            "SELECT owner_jid, node_name, item_id FROM pubsub_items \
             WHERE owner_jid = ? OR node_name = ? OR node_name = ?",
            vec![
                Value::from(spaces_service.as_str()),
                Value::from(PEP_BOOKMARKS_NODE),
                Value::from(PEP_NODE_WADDLE_DM_BOOKMARKS),
            ],
        )
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    let mut report = BookmarkItemReport::default();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?
    {
        let column = |index| {
            row.get::<String>(index)
                .map_err(|error| AuthError::DatabaseError(error.to_string()))
        };
        let (owner, node, item_id) = (column(0)?, column(1)?, column(2)?);
        let carrier = if owner == spaces_service {
            BookmarkCarrier::Spaces
        } else if node == PEP_BOOKMARKS_NODE {
            BookmarkCarrier::PepBookmarks
        } else {
            BookmarkCarrier::DmBookmarks
        };
        report.items += 1;
        if CanonicalBareJid::parse_exact(&item_id).is_err() {
            *report.non_canonical.entry(carrier).or_default() += 1;
        }
    }
    Ok(report)
}

/// Log a bookmark item report's counts. No item id or owner is logged.
pub fn log_bookmark_item_report(report: &BookmarkItemReport) {
    if report.is_clean() {
        info!(
            profile = IDENTITY_PROFILE,
            items = report.items,
            "every stored bookmark item id is canonical"
        );
        return;
    }
    warn!(
        profile = IDENTITY_PROFILE,
        items = report.items,
        spaces = report.non_canonical(BookmarkCarrier::Spaces),
        pep_bookmarks = report.non_canonical(BookmarkCarrier::PepBookmarks),
        dm_bookmarks = report.non_canonical(BookmarkCarrier::DmBookmarks),
        "stored bookmark items with non-canonical ids; see docs/operations/identity-profile-rollout.md"
    );
}

async fn query(actor: &ActorRef<DbActor>, sql: &str) -> Result<Vec<Vec<Value>>, AuthError> {
    actor
        .ask(DbQuery {
            sql: sql.to_string(),
            params: vec![],
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

fn text(row: &[Value], index: usize) -> Result<String, AuthError> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

fn optional_text(row: &[Value], index: usize) -> Result<Option<String>, AuthError> {
    row_value(row, index)
        .and_then(ValueExt::as_optional_string)
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

fn integer(row: &[Value], index: usize) -> Result<i64, AuthError> {
    match row_value(row, index).map_err(|error| AuthError::DatabaseError(error.to_string()))? {
        Value::Integer(value) => Ok(*value),
        _ => Err(AuthError::DatabaseError(
            "account column is not an integer".to_string(),
        )),
    }
}
