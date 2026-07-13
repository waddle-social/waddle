//! [`IsrTokenStore`] trait + [`InMemoryIsrTokenStore`] (ADR-0017 Phase 3
//! Slice 8, Q8 resolution).
//!
//! Mirrors the `ClaimStore` split (ADR-0017 Phase 3 Slice 1, Q1): the trait
//! plus the trivial single-node implementation live here, unconditionally
//! compiled; the Postgres-backed, fenced implementation
//! (`PostgresIsrTokenStore`) lives downstream in
//! `waddle-server::clustering::isr`, gated `#[cfg(feature = "clustering")]`.
//!
//! **`InMemoryIsrTokenStore` is never advertised in production** — ADR-0017
//! Phase 3 Slice 8's compounding decision (Q8) gates XEP-0397 advertisement
//! on `clustering.enabled && Postgres`, full stop. This type exists so the
//! trait has a real single-node implementor (matching `InProcessClaimStore`'s
//! role for `ClaimStore`) and so unit tests can exercise the trait contract
//! without a live Postgres instance, not because single-node ISR is a
//! supported deployment shape.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use subtle::ConstantTimeEq;

use crate::stream_management::persistence::SmClaimFence;

/// A freshly issued or rotated ISR token (ADR-0017 Phase 3 Slice 8,
/// XEP-0397). The token string itself is a secret credential — never
/// compared with `==`; see [`IsrTokenStore::consume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedIsrToken {
    pub token: String,
    pub mechanism: String,
}

/// Outcome of a fenced [`IsrTokenStore::consume`] attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsrConsumeOutcome {
    /// The presented token matched (compared in constant time). The old
    /// token row was destroyed and a fresh one rotated in, atomically, in
    /// the same fencing transaction (XEP-0397 "Successful Stream
    /// Resumption": the server's success reply MUST include a *new* ISR
    /// token). `rotated` is the value actually committed — never a
    /// pre-commit guess.
    Matched { rotated: IssuedIsrToken },
    /// A token row EXISTED for this SM-ID, but the presented token did not
    /// match the stored one, or the stored token's pinned mechanism did not
    /// match the mechanism the caller is presenting it under. Per
    /// XEP-0397's anti-brute-force MUST, the row was already destroyed
    /// unconditionally before this variant is returned — this is a genuine
    /// wrong-token attempt against a real ISR-enabled session, so the
    /// caller MUST additionally destroy the SM session state the SM-ID
    /// identified (council-adjudicated FIX 3, ADR-0017 Phase 3 Slice 8).
    Mismatched,
    /// No token row existed for this SM-ID at all — either this session
    /// never opted into ISR (no `<isr-enable/>` was ever issued for it), or
    /// a previous attempt already consumed/destroyed it. Nothing was
    /// touched: no row existed to delete (council-adjudicated FIX 1's
    /// phantom-delete guard — a concurrent loser's blocked read observes
    /// exactly this after the winner's commit, per Postgres's
    /// delete-not-update row-lock semantics; see
    /// `PostgresIsrTokenStore::consume`'s doc comment). The caller MUST
    /// return the failure WITHOUT destroying any session state
    /// (council-adjudicated FIX 3): an ISR-authenticate attempt against a
    /// resumable-but-never-ISR-enabled session, or a replay of an
    /// already-consumed attempt, must not destroy anything.
    NoSuchToken,
}

/// [`IsrTokenStore`] failures. Typed per the repo's typed-payloads hard
/// rule — never a bare `String` masquerading as structured data.
#[derive(Debug, thiserror::Error)]
pub enum IsrTokenStoreError {
    /// The backing store's own error, converted to its `Display` text (the
    /// same necessary exception `ClaimError::Backend` documents: a
    /// Postgres-backed implementation's richly-typed error cannot be named
    /// from this unconditionally-compiled crate without an illegal reverse
    /// dependency).
    #[error("ISR token store backend error: {0}")]
    Backend(String),
    /// The caller's claimed fencing epoch no longer holds the SM-session
    /// claim (ADR-0017 Phase 3 Slice 8's locked spec: consume runs inside
    /// the SAME epoch-fenced transaction as the SM claim's own
    /// `SELECT ... FOR SHARE`). The caller lost ownership of the entity
    /// mid-flight; no token row is touched.
    #[error("SM-session claim fencing check failed: caller no longer holds this claim")]
    NotOwner,
    /// In-process bookkeeping lock was poisoned by a panicking holder.
    #[error("ISR token store internal lock poisoned")]
    Poisoned,
}

/// Postgres-authoritative, epoch-fenced ISR token storage (ADR-0017 Phase 3
/// Slice 8, element 10). See the module doc for the trait/impl split
/// rationale.
///
/// [`consume`](Self::consume) is the locked-spec operation: fetch the
/// token row by the non-secret `sm_id` key (never by token), compare the
/// stored token against `presented_token` in Rust with a constant-time
/// primitive, and only then delete — all inside one epoch-fenced
/// transaction bound to `me`/`mine`'s currently-held SM-session claim.
/// Matching the token in a SQL `WHERE` clause is explicitly banned as a
/// timing oracle; every implementation of this trait must honor that ban.
#[async_trait]
pub trait IsrTokenStore: Send + Sync {
    /// Create the backing schema if it does not exist. Idempotent.
    async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError>;

    /// Mint and store a fresh token for `sm_id`, pinned to `mechanism`
    /// (XEP-0397's "mechanism pinning", element 10: the entities involved
    /// MUST only use or allow this mechanism when performing ISR with the
    /// returned token). Overwrites any existing token for this SM-ID —
    /// issuance is a fresh `<isr-enable/>`, never a rotation of an existing
    /// token; rotation is [`consume`](Self::consume)'s job.
    async fn issue(
        &self,
        sm_id: &str,
        mechanism: &str,
    ) -> Result<IssuedIsrToken, IsrTokenStoreError>;

    /// Fenced, single-use, constant-time consume. See the trait-level doc
    /// for the locked spec this must implement exactly.
    async fn consume(
        &self,
        sm_id: &str,
        presented_token: &[u8],
        mechanism: &str,
        fence: &SmClaimFence,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError>;

    /// Council-adjudicated FIX 4 (ADR-0017 Phase 3 Slice 8): reap token
    /// rows older than `max_age`. A row is minted per `<isr-enable/>` and
    /// is never reaped by the ordinary [`consume`](Self::consume) path
    /// alone — a token issued but never resumed (or whose SM session is
    /// later expired/reaped by some other path with no cascade hook back
    /// to this store) would otherwise sit in `clustering_isr_tokens`
    /// forever. Returns the number of rows deleted.
    ///
    /// [`InMemoryIsrTokenStore`]'s implementation is a no-op returning `0`:
    /// per this module's own doc comment it is never advertised/exercised
    /// in production (ISR requires `clustering.enabled && Postgres`), so
    /// nothing ever accumulates there worth sweeping, and it tracks no
    /// issuance timestamp to sweep by in the first place.
    async fn sweep_expired(&self, max_age: std::time::Duration) -> Result<u64, IsrTokenStoreError>;
}

/// Trivial single-node [`IsrTokenStore`]. See the module doc: never
/// advertised in production (ISR requires `clustering.enabled && Postgres`),
/// kept for trait-contract symmetry and unit testing only.
#[derive(Debug, Default)]
pub struct InMemoryIsrTokenStore {
    tokens: RwLock<HashMap<String, IssuedIsrToken>>,
}

impl InMemoryIsrTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl IsrTokenStore for InMemoryIsrTokenStore {
    async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
        // No backing schema — in-memory only.
        Ok(())
    }

    async fn issue(
        &self,
        sm_id: &str,
        mechanism: &str,
    ) -> Result<IssuedIsrToken, IsrTokenStoreError> {
        let issued = IssuedIsrToken {
            token: super::generate_isr_token(),
            mechanism: mechanism.to_string(),
        };
        let mut tokens = self
            .tokens
            .write()
            .map_err(|_| IsrTokenStoreError::Poisoned)?;
        tokens.insert(sm_id.to_string(), issued.clone());
        Ok(issued)
    }

    async fn consume(
        &self,
        sm_id: &str,
        presented_token: &[u8],
        mechanism: &str,
        // No node-liveness table exists for the single-node case (mirrors
        // `InProcessClaimStore`'s own module doc): there is only one node,
        // so fencing is a no-op here rather than a meaningful check.
        _fence: &SmClaimFence,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
        let mut tokens = self
            .tokens
            .write()
            .map_err(|_| IsrTokenStoreError::Poisoned)?;
        // Council-adjudicated FIX 1/FIX 3: distinguish "no row at all"
        // (never opted in / already consumed) from "a row existed but
        // didn't match" (genuine wrong-token attempt) — mirroring
        // `PostgresIsrTokenStore::consume`'s same guard.
        let Some(stored) = tokens.remove(sm_id) else {
            return Ok(IsrConsumeOutcome::NoSuchToken);
        };
        // Constant-time comparison (ADR-0017 Phase 3 Slice 8, element 10):
        // never `==` on the secret token bytes. The mechanism pin is
        // non-secret metadata, compared plainly.
        let matches = stored.mechanism == mechanism
            && bool::from(stored.token.as_bytes().ct_eq(presented_token));
        if !matches {
            // Already removed above — destroyed unconditionally, per the
            // XEP's anti-brute-force MUST.
            return Ok(IsrConsumeOutcome::Mismatched);
        }
        let rotated = IssuedIsrToken {
            token: super::generate_isr_token(),
            mechanism: mechanism.to_string(),
        };
        tokens.insert(sm_id.to_string(), rotated.clone());
        Ok(IsrConsumeOutcome::Matched { rotated })
    }

    async fn sweep_expired(
        &self,
        _max_age: std::time::Duration,
    ) -> Result<u64, IsrTokenStoreError> {
        // See the trait method's doc comment: never advertised/exercised in
        // production, and this store tracks no issuance timestamp to sweep
        // by — a no-op, not a partial implementation of a real sweep.
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence() -> SmClaimFence {
        SmClaimFence::new(
            crate::ownership::NodeIdentity::local(),
            crate::ownership::ClaimEpoch(0),
        )
    }

    #[tokio::test]
    async fn issue_then_consume_with_matching_token_rotates() {
        let store = InMemoryIsrTokenStore::new();
        let issued = store.issue("sm-1", "PLAIN").await.expect("issue");
        let outcome = store
            .consume("sm-1", issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        let IsrConsumeOutcome::Matched { rotated } = outcome else {
            panic!("expected Matched, got {outcome:?}");
        };
        assert_ne!(rotated.token, issued.token);
    }

    #[tokio::test]
    async fn consume_with_wrong_token_is_mismatched_and_destroys_the_row() {
        let store = InMemoryIsrTokenStore::new();
        let issued = store.issue("sm-1", "PLAIN").await.expect("issue");
        let outcome = store
            .consume("sm-1", b"not-the-token", "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);

        // The row is gone — a second consume attempt with the ORIGINAL
        // (correct) token now finds no row at all (FIX 3: distinct from
        // the genuine-mismatch outcome above), proving destruction happened.
        let second = store
            .consume("sm-1", issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(second, IsrConsumeOutcome::NoSuchToken);
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let store = InMemoryIsrTokenStore::new();
        let issued = store.issue("sm-1", "PLAIN").await.expect("issue");
        let first = store
            .consume("sm-1", issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert!(matches!(first, IsrConsumeOutcome::Matched { .. }));

        // The OLD token no longer works, even though a rotated token now
        // exists under the same sm_id — a genuine mismatch (a row DOES
        // exist), not `NoSuchToken`.
        let replay = store
            .consume("sm-1", issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(replay, IsrConsumeOutcome::Mismatched);
    }

    #[tokio::test]
    async fn consume_with_no_issued_token_is_no_such_token() {
        let store = InMemoryIsrTokenStore::new();
        let outcome = store
            .consume("no-such-sm-id", b"anything", "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::NoSuchToken);
    }

    #[tokio::test]
    async fn consume_pins_the_mechanism() {
        let store = InMemoryIsrTokenStore::new();
        let issued = store.issue("sm-1", "PLAIN").await.expect("issue");
        let outcome = store
            .consume("sm-1", issued.token.as_bytes(), "SCRAM-SHA-256", &fence())
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);
    }
}
