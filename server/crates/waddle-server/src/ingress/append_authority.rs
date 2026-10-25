//! Receiver-side authority for an ingress obligation a peer node relayed.
//!
//! Shared by every place a relayed obligation is about to key an XEP-0198 replay
//! append: the ordered-relay receivers (#1778) and the registered-socket detach
//! drain (#1789). An archive-ordered relay must retain valid authority to enter
//! the destination queue. Already accepted transport frames keep their drain
//! behavior when optional deduplication authority cannot be recovered.

use crate::db::Database;
use crate::ingress_uow::ArchiveDispatchRepository;
use crate::ingress_uow::CarbonReceiptRepository;
use std::time::Duration;

use waddle_xmpp::ingress::{IngressEffectKind, MessageKey};
use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;
use waddle_xmpp::Stanza;

#[path = "append_authority_carbons.rs"]
mod carbons;

#[cfg(test)]
#[path = "append_authority_carbon_tests.rs"]
mod carbon_tests;

#[cfg(test)]
#[path = "append_authority_room_tests.rs"]
mod room_tests;

#[cfg(all(test, feature = "clustering"))]
#[path = "append_authority_deferred_tests.rs"]
mod deferred_tests;

/// Canonical-row authorization reads, counted per test process (nextest runs
/// one test per process) so tests can assert where the read happens (#1790).
#[cfg(all(test, feature = "clustering"))]
pub(crate) mod canonical_reads {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static READS: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn record() {
        READS.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn count() -> usize {
        READS.load(Ordering::SeqCst)
    }
}

mod implementation;
pub(crate) use implementation::check_canonical_obligation;
pub(crate) use implementation::check_canonical_sender;
pub(crate) use implementation::check_resource_binding;
pub(crate) use implementation::check_stanza_binding;
pub(crate) use implementation::receipt_kind_is_append_eligible;
pub(super) use implementation::recipient_copy_matches;
pub(crate) use implementation::record_authorization_failure;
pub(super) use implementation::same_message_content;
pub(crate) use implementation::stanza_sender;
pub(crate) use implementation::AppendAuthority;
pub(crate) use implementation::AppendAuthorityRejection;
#[cfg(feature = "clustering")]
pub(crate) use implementation::DeferredAppendAuthority;
