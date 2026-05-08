use jid::BareJid;
use waddle_xmpp_core::roster::RosterVersion;

/// A roster mutation request at the row layer.
#[derive(Debug, Clone)]
pub enum RosterRowChange {
    /// Add or update an item. Storage decides Added vs Updated based on
    /// whether the row already exists.
    Upsert(RosterItemRow),
    /// Remove the item with the given contact JID.
    Remove(BareJid),
}

/// Outcome of an atomic roster mutation: the classified row-layer result and
/// the post-mutation roster version, computed under the same per-user lock.
///
/// XEP-0237 §2.6 requires every roster push to carry the post-mutation
/// version, those versions to be unique, and pushes to occur in modification
/// order. Returning the version from the same call that performed the
/// mutation is what makes those MUSTs holdable under concurrency.
#[derive(Debug, Clone)]
pub struct RosterRowMutation {
    /// Classified row-layer result (Added / Updated / Removed).
    pub kind: RosterRowMutationKind,
    /// Roster version after this mutation.
    pub version: RosterVersion,
}

/// Row-layer outcome classification.
#[derive(Debug, Clone)]
pub enum RosterRowMutationKind {
    /// Item was newly inserted.
    Added(RosterItemRow),
    /// Existing item was overwritten.
    Updated(RosterItemRow),
    /// Item was deleted.
    Removed(BareJid),
}

/// A roster item row from the database.
#[derive(Debug, Clone)]
pub struct RosterItemRow {
    /// The contact's JID (bare JID string).
    pub contact_jid: String,
    /// Optional display name for the contact.
    pub name: Option<String>,
    /// Subscription state: 'none', 'to', 'from', 'both'.
    pub subscription: String,
    /// Pending subscription request: 'subscribe' or None.
    pub ask: Option<String>,
    /// Whether the contact is pre-approved for a future subscription request.
    pub approved: bool,
    /// Groups this contact belongs to.
    pub groups: Vec<String>,
}

/// Errors that can occur during roster storage operations.
#[derive(Debug, thiserror::Error)]
pub enum RosterStorageError {
    /// Database-layer failure (connect, execute, transaction commit).
    /// Carries the typed [`DatabaseError`](crate::db::DatabaseError) verbatim
    /// so callers can branch on the underlying cause if needed.
    #[error("Database error: {0}")]
    Database(#[from] crate::db::DatabaseError),

    /// Connection acquisition failed at the storage layer (pre-existing variant).
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    /// Row-decode failure context. Only used for cases where the column
    /// extraction itself failed — the database error chain doesn't apply.
    #[error("Query failed: {0}")]
    QueryFailed(String),

    /// JSON serialization of a roster row's `groups` field failed.
    #[error("Roster row serialization failed: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// A `Remove` mutation targeted a roster item that does not exist.
    /// Callers map this to a `<item-not-found/>` stanza error per RFC 6121.
    #[error("Roster item not found")]
    ItemNotFound,

    /// A JID stored in the database failed to parse — indicates corruption,
    /// since stored JIDs are written via `BareJid::to_string`.
    #[error("Invalid stored JID '{value}': {source}")]
    InvalidStoredJid {
        value: String,
        #[source]
        source: jid::Error,
    },

    /// A roster version stored in the database failed to parse (e.g. empty
    /// string). Indicates corruption — stored versions are written via
    /// `RosterVersion::generate`.
    #[error("Invalid stored roster version: '{value}'")]
    InvalidStoredVersion { value: String },
}

/// Owned guard returned by [`DatabaseRosterStorage::apply_roster_change`] and
/// related mutating methods. Holding it serialises further mutations and
/// reads of the same user's roster. The caller MUST keep it alive until any
/// roster pushes that announce the mutation's `RosterVersion` have been
/// enqueued onto the recipient sockets — otherwise a concurrent mutation
/// could race ahead and break XEP-0237 §2.6's "pushes MUST occur in order
/// of modification" invariant.
///
/// Implemented as a type alias rather than a wrapper struct so the underlying
/// `OwnedMutexGuard` field cannot trigger `dead_code` warnings — the guard's
/// purpose is its `Drop` impl, not data access. (See PR #336 review.)
pub type UserMutationLock = tokio::sync::OwnedMutexGuard<()>;
