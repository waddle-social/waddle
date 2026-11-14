//! The codec's admission questions for one inbound connection.
//!
//! The codec asks while it checks each stream header. The answers come from
//! the configuration snapshot, in the identity profile's canonical domain
//! form, and the first admitted pair is remembered: it selects the trust
//! profile and reference identity before the TLS handshake starts.

use std::sync::{Arc, Mutex, MutexGuard};

use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{DestinationStatus, InboundAdmission};

use crate::s2s::config::S2sConfig;

/// What the stream headers of a connection claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Claim {
    /// No header has named an admissible pair yet.
    None,
    /// The header named this admitted pair.
    Admitted {
        local: CanonicalDomain,
        remote: CanonicalDomain,
    },
    /// The header named a remote domain that is not admitted.
    Refused,
}

/// Admission for one inbound connection.
#[derive(Debug)]
pub(super) struct ConnectionAdmission {
    config: Arc<S2sConfig>,
    claim: Mutex<Claim>,
}

impl ConnectionAdmission {
    pub(super) fn new(config: Arc<S2sConfig>) -> Arc<Self> {
        Arc::new(Self {
            config,
            claim: Mutex::new(Claim::None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Claim> {
        // The value is replaced whole, so a poisoned lock holds a usable
        // value.
        match self.claim.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// What the stream headers claimed so far.
    pub(super) fn claim(&self) -> Claim {
        self.lock().clone()
    }

    fn record(&self, claim: Claim) {
        let mut current = self.lock();
        // A pair, once admitted, stays the pair; the codec refuses any
        // later header that changes it.
        if !matches!(*current, Claim::Admitted { .. }) {
            *current = claim;
        }
    }
}

impl InboundAdmission for ConnectionAdmission {
    fn destination(&self, destination: &CanonicalDomain) -> DestinationStatus {
        if self.config.is_local(destination) {
            DestinationStatus::Served
        } else {
            DestinationStatus::Unknown
        }
    }

    fn admits(&self, source: &CanonicalDomain, destination: &CanonicalDomain) -> bool {
        if !self.config.is_local(destination) {
            self.record(Claim::Refused);
            return false;
        }
        match self.config.admission().decide(source) {
            Ok(()) => {
                self.record(Claim::Admitted {
                    local: destination.clone(),
                    remote: source.clone(),
                });
                true
            }
            Err(refusal) => {
                tracing::debug!(reason = refusal.code(), "s2s peer not admitted");
                self.record(Claim::Refused);
                false
            }
        }
    }
}
