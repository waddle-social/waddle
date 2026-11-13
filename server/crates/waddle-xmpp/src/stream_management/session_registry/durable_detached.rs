//! Detached sessions held by the durable stream-management authority.
//!
//! Under the durable selector a resumable session's lifecycle, counters and
//! retained frames belong to the durable authority, not to this registry's
//! maps. A session whose transport is gone is still a recipient for the rest
//! of its resume window: a stanza routed to it is admitted to the session's
//! custody and replayed when the client resumes, or transferred to offline
//! delivery when the session expires.
//!
//! The registry's detached readers and appenders consult the attached
//! [`DurableDetachedSessions`] after their own maps, so every routing path
//! that asks this registry for detached resources sees those sessions with
//! the same selection metadata (presence, carbons, roster and block-list
//! interest) a legacy detached session carries.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};

use super::core::InMemorySmSessionRegistry;
use super::resources::DetachedPresenceState;
use super::SmRegistryError;
use crate::pending_delivery::SmSessionId;
use crate::stream_management::SmIngressAppendKey;
use crate::Stanza;

/// One detached durable session, as routing selects recipients from it.
#[derive(Debug, Clone)]
pub struct DurableDetachedResource {
    pub stream_id: SmSessionId,
    pub jid: FullJid,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    /// The resource's last broadcast presence, stored when it detached.
    pub presence: DetachedPresenceState,
}

/// One stanza offered to a detached durable session.
#[derive(Debug, Clone, Copy)]
pub struct DurableDetachedFrame<'a> {
    pub resource: &'a FullJid,
    pub stanza: &'a Stanza,
    pub original_receipt_at: DateTime<Utc>,
    /// The ingress obligation this copy discharges, when an ingress
    /// transaction owns it. A repeated offer of the same obligation to the
    /// same session is admitted once.
    pub key: Option<&'a SmIngressAppendKey>,
}

/// What a detached durable session did with an offered stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableDetachedAdmission {
    /// The stanza is in the session's custody under `accepting_stream`.
    Admitted { accepting_stream: SmSessionId },
    /// The resource has no detached durable session this node can admit
    /// to, or the session refused the stanza. Nothing was retained.
    NoSession,
}

/// The durable authority's detached sessions, attached to the registry
/// when the durable selector is active.
#[async_trait::async_trait]
pub trait DurableDetachedSessions: Send + Sync {
    /// Every detached session of `account` inside its resume window that
    /// this node can admit to.
    async fn detached_resources(
        &self,
        registry: &InMemorySmSessionRegistry,
        account: &BareJid,
    ) -> Result<Vec<DurableDetachedResource>, SmRegistryError>;

    /// Admit `frame` to the detached session of its resource.
    ///
    /// `Err` means the commit outcome is unknown: the stanza may be in
    /// custody, so the caller must not deliver it another way.
    async fn admit(
        &self,
        registry: &InMemorySmSessionRegistry,
        frame: DurableDetachedFrame<'_>,
    ) -> Result<DurableDetachedAdmission, SmRegistryError>;
}

impl InMemorySmSessionRegistry {
    /// Attach the durable authority's detached sessions. Returns `false`
    /// when a source is already attached; the first one stays.
    pub fn attach_durable_detached(&self, sessions: Arc<dyn DurableDetachedSessions>) -> bool {
        self.durable_detached.set(sessions).is_ok()
    }

    /// Whether the durable authority's detached sessions are attached.
    pub fn has_durable_detached(&self) -> bool {
        self.durable_detached.get().is_some()
    }

    /// The attached durable detached sessions of `account` that satisfy
    /// `keep`. Empty when no durable authority is attached.
    pub(super) async fn durable_detached_matching(
        &self,
        account: &BareJid,
        keep: impl Fn(&DurableDetachedResource) -> bool,
    ) -> Result<Vec<DurableDetachedResource>, SmRegistryError> {
        let Some(sessions) = self.durable_detached.get() else {
            return Ok(Vec::new());
        };
        Ok(sessions
            .detached_resources(self, account)
            .await?
            .into_iter()
            .filter(|resource| keep(resource))
            .collect())
    }

    /// Full JIDs of the attached durable detached sessions of `account` that
    /// satisfy `keep` and are not already in `found`, appended to `found`.
    pub(super) async fn extend_with_durable_detached(
        &self,
        account: &BareJid,
        found: &mut Vec<FullJid>,
        keep: impl Fn(&DurableDetachedResource) -> bool,
    ) -> Result<(), SmRegistryError> {
        for resource in self.durable_detached_matching(account, keep).await? {
            if !found.contains(&resource.jid) {
                found.push(resource.jid);
            }
        }
        Ok(())
    }

    /// Admit `stanza` to the durable detached session of `jid` when that
    /// session satisfies `keep`. Returns the accepting stream.
    pub(super) async fn admit_to_durable_detached(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
        key: Option<&SmIngressAppendKey>,
        keep: impl Fn(&DurableDetachedResource) -> bool,
    ) -> Result<Option<SmSessionId>, SmRegistryError> {
        let Some(sessions) = self.durable_detached.get() else {
            return Ok(None);
        };
        let eligible = self
            .durable_detached_matching(&jid.to_bare(), |resource| {
                resource.jid == *jid && keep(resource)
            })
            .await?;
        if eligible.is_empty() {
            return Ok(None);
        }
        match sessions
            .admit(
                self,
                DurableDetachedFrame {
                    resource: jid,
                    stanza,
                    original_receipt_at,
                    key,
                },
            )
            .await?
        {
            DurableDetachedAdmission::Admitted { accepting_stream } => Ok(Some(accepting_stream)),
            DurableDetachedAdmission::NoSession => Ok(None),
        }
    }
}
