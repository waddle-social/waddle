//! Exactly-once identity for an ingress-driven append to a detached XEP-0198 queue.
//!
//! A recorded ingress direct-route obligation freezes a resource fanout. Appending one
//! of those resources to its recipient's detached replay queue must allocate at most one
//! queue entry per (obligation, resource), however many times post-commit execution runs:
//! two frozen decisions may execute concurrently, execution may be cancelled mid-persist,
//! and a recovery pass may re-execute an obligation whose progress was never committed.
//!
//! The identity below is that allocation's durable key. It is deliberately **not**
//! partitioned by stream: the accepting stream for a full JID is whichever session is
//! currently bound, a rebind displaces the previous one, and a successful resume deletes
//! the detached snapshot while the logical stream continues. Keying by stream would let
//! each of those transitions authorize a second allocation for the same obligation.
//!
//! XEP-0198 itself cannot promise a client observes every stanza exactly once — a
//! retransmission after an uncertain ack may duplicate (XEP-0198 §5). The guarantee here
//! is narrower and server-side: one durable queue allocation per obligation and resource,
//! with no retry-induced duplicate.

use jid::FullJid;

use super::SmIngressReceiptKind;
use crate::{ingress::MessageKey, pending_delivery::SmSessionId};

/// The obligation and resource an ingress-driven append discharges.
///
/// `kind` plus `semantic_identity_hash` are the recorded effect receipt identity, carried
/// opaquely (the same discriminator space as [`super::SmIngressFrameReceipt`], which
/// proves a different thing: that a frame reached the wire). Including the receipt
/// identity — not just the message key — is what keeps two *distinct* recorded routes to
/// the same resource independently appendable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmIngressAppendKey {
    pub message_key: MessageKey,
    pub kind: SmIngressReceiptKind,
    pub semantic_identity_hash: [u8; 32],
    pub resource: FullJid,
}

/// Result of a keyed append attempt.
///
/// There is no "committed but this registry lost the session" success: a snapshot that
/// commits while its session is displaced is reconciled into the promotion handoff, so
/// durable proof always means the entry is allocated and will be delivered or promoted.
/// Storage and registry failures are typed errors, not variants here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmKeyedAppendOutcome {
    /// This call allocated the queue entry.
    Appended { accepting_stream: SmSessionId },
    /// Durable proof already existed, so nothing was appended and no counter moved.
    ///
    /// `accepting_stream` is the stream that won the allocation, which may be an *older*
    /// stream than the one currently bound for the resource. That is still valid proof:
    /// the obligation was allocated once, which is exactly what must not happen twice.
    AlreadyAppended { accepting_stream: SmSessionId },
    /// No unexpired session for the resource. Nothing was appended and the obligation
    /// remains unresolved for its recorded route to retry or degrade.
    NoSession,
}

impl SmKeyedAppendOutcome {
    /// Whether a durable queue allocation exists for this obligation, by this call or an
    /// earlier one. Callers record delivery progress on exactly this condition.
    pub fn is_allocated(&self) -> bool {
        match self {
            Self::Appended { .. } | Self::AlreadyAppended { .. } => true,
            Self::NoSession => false,
        }
    }

    /// The stream holding the allocation, when there is one.
    pub fn accepting_stream(&self) -> Option<&SmSessionId> {
        match self {
            Self::Appended { accepting_stream } | Self::AlreadyAppended { accepting_stream } => {
                Some(accepting_stream)
            }
            Self::NoSession => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hash: u8, resource: &str) -> SmIngressAppendKey {
        SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [hash; 32],
            resource: resource.parse().expect("full jid"),
        }
    }

    #[test]
    fn distinct_receipt_identities_are_distinct_obligations() {
        let message_key = MessageKey::new();
        let first = SmIngressAppendKey {
            message_key,
            ..key(1, "juliet@example.com/phone")
        };
        let second = SmIngressAppendKey {
            message_key,
            ..key(2, "juliet@example.com/phone")
        };
        // One message can carry two recorded routes to one resource; each is allowed its
        // own allocation, so the receipt identity must participate in equality.
        assert_ne!(first, second);
    }

    #[test]
    fn distinct_resources_of_one_obligation_are_distinct_keys() {
        let message_key = MessageKey::new();
        let phone = SmIngressAppendKey {
            message_key,
            ..key(1, "juliet@example.com/phone")
        };
        let tablet = SmIngressAppendKey {
            message_key,
            ..key(1, "juliet@example.com/tablet")
        };
        assert_ne!(phone, tablet);
    }

    #[test]
    fn allocation_is_reported_for_fresh_and_prior_appends() {
        let stream = SmSessionId::new("stream-1");
        let appended = SmKeyedAppendOutcome::Appended {
            accepting_stream: stream.clone(),
        };
        let already = SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: stream.clone(),
        };
        assert!(appended.is_allocated());
        assert!(already.is_allocated());
        assert!(!SmKeyedAppendOutcome::NoSession.is_allocated());
        assert_eq!(appended.accepting_stream(), Some(&stream));
        assert_eq!(already.accepting_stream(), Some(&stream));
        assert_eq!(SmKeyedAppendOutcome::NoSession.accepting_stream(), None);
    }
}
