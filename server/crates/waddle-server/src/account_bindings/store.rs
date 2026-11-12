//! The repository contract shared by the database and in-memory stores.

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::{BindingFenceError, Presentation};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceOperation, AccountResourceRevision,
};
use waddle_xmpp::ownership::{BindingRef, SmSessionKey, UserClaimIdentity};

use super::error::AccountBindingStoreError;
use super::row::StoredResource;

/// The result of one store execution. `stored` is the row as the commit
/// left it; it is absent for a rejection and for a replayed outcome, which
/// change nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredExecution {
    pub execution: AccountResourceExecution,
    pub stored: Option<StoredResource>,
}

/// The result of recording a one-time presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationMark {
    /// This call recorded the presentation; the caller presents.
    First,
    /// The presentation was already recorded; the caller must not present.
    AlreadyPresented,
    /// The binding named is not the resource's current binding.
    Fenced(BindingFenceError),
}

/// Durable account-resource rows, one per full JID.
///
/// Every write names its `authority`: the claim identity under which the
/// caller coordinates the account. It must be a claim on the resource's
/// account, and a clustered store proves in the write's transaction that it
/// is the claim currently held. The authority is separate from the claim an
/// operation's fence names, because a coordinator that takes an account
/// over writes rows still held under the previous claim.
///
/// Every mutation is one bounded transaction that locks the resource row,
/// resolves the operation identity, applies the pure slot transition to the
/// stored image and writes the result. A rejection writes nothing. A
/// committed operation is recorded under its identity; repeating it returns
/// `Replayed` with the committed revision, and reusing the identity for a
/// different operation is [`AccountBindingStoreError::OperationIdConflict`].
/// A rejected operation is not recorded: repeating it is decided afresh,
/// which is safe because a rejection changed nothing.
///
/// A commit that gives a binding a stream-management reference, or changes
/// the one it has, is refused unless the durable session row exists and is
/// bound at the generation the reference names. The database also holds the
/// session id as a deferred foreign key.
#[async_trait]
pub trait AccountBindingStore: Send + Sync + 'static {
    /// Apply one identified mutation to the row of `full_jid`. Only a
    /// `Bind` creates a row.
    async fn execute(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        operation: &AccountResourceOperation,
    ) -> Result<StoredExecution, AccountBindingStoreError>;

    /// Record that `presentation` happened for the resource's current
    /// binding, fenced by `binding_ref`.
    async fn mark_presented(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<PresentationMark, AccountBindingStoreError>;

    /// Delete the row of a resource that holds no binding, with its
    /// recorded operations. Returns whether a row was deleted; a row that
    /// holds a binding or is at another revision is kept.
    async fn forget(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        expected_revision: AccountResourceRevision,
    ) -> Result<bool, AccountBindingStoreError>;

    /// Read one resource row without locking it.
    async fn load(
        &self,
        full_jid: &FullJid,
    ) -> Result<Option<StoredResource>, AccountBindingStoreError>;

    /// The resources whose current binding references `session`. The
    /// durable session row cannot be deleted while this is not empty.
    async fn resources_for_session(
        &self,
        session: SmSessionKey,
    ) -> Result<Vec<FullJid>, AccountBindingStoreError>;

    /// Up to `limit` accounts that have a resource holding a binding, in
    /// bare-JID order, starting after `after`.
    async fn bound_accounts(
        &self,
        after: Option<&BareJid>,
        limit: u32,
    ) -> Result<Vec<BareJid>, AccountBindingStoreError>;

    /// Read every resource row of an account, ordered by full JID.
    async fn load_account(
        &self,
        account: &BareJid,
    ) -> Result<Vec<StoredResource>, AccountBindingStoreError>;
}
