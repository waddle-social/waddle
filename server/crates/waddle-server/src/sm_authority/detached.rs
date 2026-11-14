//! Delivery to detached durable sessions.
//!
//! A `Detached` session inside its resume window is a recipient, as a
//! detached legacy session is: a stanza routed to it is admitted to its
//! custody on the detached authority (`AdmissionAuthority::Detached`) and
//! retained without exposure. A resume replays it in ordinal order ahead of
//! anything admitted later; an expiry makes the session terminal and the
//! janitor transfers it to offline delivery once.
//!
//! A session whose resume committed but has not completed
//! (`TransferHydrating`) stays a recipient on the same terms, as a legacy
//! session claimed by a resume does. Its connection is not published yet,
//! so no live route reaches it, and a stanza refused here would be lost or
//! stored offline while the client is about to come back. The stanza is
//! admitted above the replay range, and completing the resume returns it in
//! the tail that is written after the replay and before any live frame.
//!
//! [`DurableDetachedRecipients`] attaches these sessions to the
//! stream-management session registry, whose detached readers and appenders
//! every routing path already consults. Under the durable selector that
//! registry's own maps hold no durable session.
//!
//! A clustered store asserts the session claim on every write. A stanza
//! reaches the node that owns the recipient's account, which need not be
//! the node that holds the session's claim, so the admission takes the
//! claim on this node first when another node holds it. A `Detached` row
//! has no connection: the claim moves by the identity-bound swap a resume
//! uses for a detached session, and the holder is not asked. A resume on any
//! node later moves it again by the same rule, and the expiry sweep of the
//! node holding it expires the session.

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use sha2::{Digest, Sha256};
use tracing::warn;
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, SmSessionKey};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::{DurableSmState, FenceError, SmOperationId};
use waddle_xmpp::stream_management::persistence::{
    PersistedSession, SmClaimFence, SmReplayPayload,
};
use waddle_xmpp::stream_management::{
    DetachedPresenceState, DurableDetachedAdmission, DurableDetachedFrame, DurableDetachedResource,
    DurableDetachedSessions, InMemorySmSessionRegistry, ObservedSessionClaim, SessionClaimEvidence,
    SessionClaimTakeover, SmFrameProvenance, SmIngressAppendKey, SmIngressFrameReceipt,
    SmRegistryError, StorageOutageCause,
};

use super::custody::{
    AdmissionAuthority, AdmissionError, AdmissionFrame, AdmissionRejection, AdmissionRequest,
};
use super::error::DurableSmError;
use super::record::{self, DurableSessionRecord};
use super::store::DurableSmStore;
use crate::sm_persistence::durable_rows;

/// How often one admission is offered again while its commit outcome is
/// unknown. Admission is idempotent under the frame's operation id.
const ADMISSION_ATTEMPTS: usize = 3;

/// A detached session and the resource snapshot it stored when it detached.
#[derive(Debug, Clone)]
pub struct DetachedDurableSession {
    pub record: DurableSessionRecord,
    /// Absent when the session's owner was lost before it detached.
    pub resource: Option<PersistedSession>,
}

/// Whether a session awaits a resume: no connection writes to it, and the
/// detached authority admits frames to it.
fn awaits_resume(state: DurableSmState) -> bool {
    matches!(
        state,
        DurableSmState::Detached | DurableSmState::TransferHydrating
    )
}

/// Whether a detached session can still be resumed at `now`. A session
/// without a detach time has no window to elapse.
fn resumable_at(record: &DurableSessionRecord, now: DateTime<Utc>) -> bool {
    record.detached_at.is_none_or(|detached_at| {
        now.signed_duration_since(detached_at)
            .to_std()
            .map_or(true, |elapsed| elapsed < record.max_resume)
    })
}

impl DurableSmStore {
    /// Every session of `account` that awaits a resume, `Detached` or
    /// `TransferHydrating`, whose resume window has not elapsed at `now`,
    /// with its resource snapshot.
    pub async fn detached_sessions_of_account(
        &self,
        account: &BareJid,
        now: DateTime<Utc>,
    ) -> Result<Vec<DetachedDurableSession>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let mut sessions = Vec::new();
        for stored in record::load_awaiting_resume_account(&mut tx, account).await? {
            if stored.full_jid.to_bare() != *account || !resumable_at(&stored, now) {
                continue;
            }
            let resource =
                durable_rows::load_resource_snapshot(&mut tx, &record::stream_id(stored.session))
                    .await?;
            sessions.push(DetachedDurableSession {
                record: stored,
                resource,
            });
        }
        tx.rollback().await?;
        Ok(sessions)
    }

    /// The session of `resource` that awaits a resume, `Detached` or
    /// `TransferHydrating`, and whose resume window has not elapsed at
    /// `now`. A bind of the resource ends its earlier sessions, so at most
    /// one is expected; the most recently detached one is chosen.
    pub async fn detached_session_of_resource(
        &self,
        resource: &FullJid,
        now: DateTime<Utc>,
    ) -> Result<Option<DurableSessionRecord>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let sessions = record::load_resource(&mut tx, resource).await?;
        tx.rollback().await?;
        Ok(sessions
            .into_iter()
            .filter(|stored| awaits_resume(stored.state) && resumable_at(stored, now))
            .max_by_key(|stored| stored.detached_at))
    }
}

/// The durable authority's detached sessions as the session registry reads
/// them.
pub struct DurableDetachedRecipients {
    store: DurableSmStore,
}

impl DurableDetachedRecipients {
    pub fn new(store: DurableSmStore) -> Self {
        Self { store }
    }
}

/// A session claim this node holds for one admission.
struct AdmissionClaim {
    /// Keeps this node's identity from rotating until the write is issued.
    _publication: Option<CurrentNodeIdentityGuard>,
    fence: SmClaimFence,
}

/// Hold the claim of the detached session `stored` on this node: the one it
/// holds already, a free one, or one another node holds, moved by the swap
/// a resume of a detached session uses. `None` when the claim could not be
/// held; nothing was written to the session then.
///
/// A hydrating session's claim belongs to the resume that committed it, so
/// it is used only where this node holds it already. Moving it would fence
/// that resume's completion.
async fn claim_for_admission(
    registry: &InMemorySmSessionRegistry,
    stream_id: &SmSessionId,
    stored: &DurableSessionRecord,
) -> Option<AdmissionClaim> {
    let stream = stream_id.as_str();
    if let Some(fence) = registry.current_sm_claim_fence(stream) {
        return Some(AdmissionClaim {
            _publication: None,
            fence,
        });
    }
    if stored.state != DurableSmState::Detached {
        return None;
    }
    if let Some(publication) = registry.ensure_session_claim(stream).await {
        let fence = registry.current_sm_claim_fence(stream)?;
        return Some(AdmissionClaim {
            _publication: Some(publication),
            fence,
        });
    }
    let foreign = match registry.observe_session_claim(stream).await {
        Ok(ObservedSessionClaim::HeldElsewhere(foreign)) => foreign,
        Ok(ObservedSessionClaim::Unclaimed | ObservedSessionClaim::HeldHere) => return None,
        Err(error) => {
            warn!(%stream_id, %error, "detached durable SM session claim could not be read");
            return None;
        }
    };
    // `stored` was read `Detached`: no connection of the holder owns the
    // session. A resume that commits on the holder before the swap keeps
    // the epoch observed here, so the swap supersedes that owner, whose next
    // write fails its claim as any superseded owner's does.
    let evidence = if foreign.owner_lease_fresh() {
        SessionClaimEvidence::OwnerReleased
    } else {
        SessionClaimEvidence::OwnerLeaseExpired
    };
    let account = stored.full_jid.to_bare();
    match registry
        .take_session_claim(&foreign, &account, &account, evidence)
        .await
    {
        SessionClaimTakeover::Taken { publication, fence } => Some(AdmissionClaim {
            _publication: Some(publication),
            fence,
        }),
        outcome => {
            tracing::debug!(%stream_id, ?outcome, "detached durable SM session claim was not moved here");
            None
        }
    }
}

fn storage_unavailable(error: DurableSmError) -> SmRegistryError {
    warn!(%error, "durable SM detached sessions could not be read");
    SmRegistryError::StorageUnavailable(StorageOutageCause::Backend)
}

fn resource_view(session: &DetachedDurableSession) -> DurableDetachedResource {
    let jid = session.record.full_jid.clone();
    let snapshot = session.resource.as_ref();
    DurableDetachedResource {
        stream_id: record::stream_id(session.record.session),
        carbons_enabled: snapshot.is_some_and(|resource| resource.carbons_enabled),
        roster_interested: snapshot.is_some_and(|resource| resource.roster_interested),
        blocklist_interested: snapshot.is_some_and(|resource| resource.blocklist_interested),
        presence_available: snapshot.is_some_and(|resource| resource.presence_available),
        presence: DetachedPresenceState {
            resource: jid.clone(),
            show: snapshot.and_then(|resource| resource.presence_show.clone()),
            status: snapshot.and_then(|resource| resource.presence_status.clone()),
            priority: snapshot.map_or(0, |resource| resource.presence_priority),
            payloads: snapshot
                .map(|resource| resource.presence_payloads.clone())
                .unwrap_or_default(),
        },
        jid,
    }
}

/// The admission identity of an ingress obligation's copy for one session.
/// A repeated offer of the same copy names the same operation, so it is
/// admitted once while its frame is retained.
fn obligation_operation(session: SmSessionKey, key: &SmIngressAppendKey) -> SmOperationId {
    let mut hasher = Sha256::new();
    hasher.update(b"waddle.sm.detached-admission.v1");
    hasher.update(session.as_uuid().as_bytes());
    hasher.update(key.message_key.to_storage().as_bytes());
    hasher.update(key.kind.to_storage().to_be_bytes());
    hasher.update(key.semantic_identity_hash);
    hasher.update(key.resource.to_string().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    SmOperationId::from_uuid(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

#[async_trait::async_trait]
impl DurableDetachedSessions for DurableDetachedRecipients {
    async fn record_owner_lost(
        &self,
        _registry: &InMemorySmSessionRegistry,
        session: &SmSessionId,
        claim: &SmClaimFence,
    ) -> Result<(), SmRegistryError> {
        use super::{LifecycleError, LifecycleRejection};
        // Only a durable session's stream id is a session key.
        let Ok(key) = uuid::Uuid::parse_str(session.as_str()) else {
            return Ok(());
        };
        let session = SmSessionKey::from_uuid(key);
        let Some(record) = self
            .store
            .session(session)
            .await
            .map_err(storage_unavailable)?
        else {
            return Ok(());
        };
        match self
            .store
            .record_owner_lost(session, record.generation, claim, Utc::now())
            .await
        {
            // A session that is already detached or ended has no owner to
            // lose.
            Ok(())
            | Err(LifecycleError::Rejected(
                LifecycleRejection::Transition(_) | LifecycleRejection::UnknownSession,
            )) => Ok(()),
            Err(error) => {
                warn!(stream_id = %record::stream_id(session), %error, "durable SM owner loss could not be recorded");
                Err(SmRegistryError::StorageUnavailable(
                    StorageOutageCause::Backend,
                ))
            }
        }
    }

    async fn detached_resources(
        &self,
        _registry: &InMemorySmSessionRegistry,
        account: &BareJid,
    ) -> Result<Vec<DurableDetachedResource>, SmRegistryError> {
        let sessions = self
            .store
            .detached_sessions_of_account(account, Utc::now())
            .await
            .map_err(storage_unavailable)?;
        Ok(sessions.iter().map(resource_view).collect())
    }

    async fn admit(
        &self,
        registry: &InMemorySmSessionRegistry,
        frame: DurableDetachedFrame<'_>,
    ) -> Result<DurableDetachedAdmission, SmRegistryError> {
        let now = Utc::now();
        let payload = match SmReplayPayload::from_stanza(frame.stanza) {
            Ok(payload) => payload,
            Err(error) => {
                warn!(resource = %frame.resource, %error, "stanza for a detached durable SM session cannot be retained");
                return Ok(DurableDetachedAdmission::NoSession);
            }
        };
        let mut attempts = 0;
        loop {
            attempts += 1;
            // The row is read again for every offer: a resume that committed
            // since the last read moved the session to a new generation.
            let Some(stored) = self
                .store
                .detached_session_of_resource(frame.resource, now)
                .await
                .map_err(storage_unavailable)?
            else {
                return Ok(DurableDetachedAdmission::NoSession);
            };
            let stream_id = record::stream_id(stored.session);
            let held = if self.store.is_clustered() {
                match claim_for_admission(registry, &stream_id, &stored).await {
                    Some(held) => Some(held),
                    None => return Ok(DurableDetachedAdmission::NoSession),
                }
            } else {
                None
            };
            let claim = held.as_ref().map(|held| held.fence.clone());
            let admission = AdmissionFrame {
                operation: frame.key.map_or_else(SmOperationId::mint, |key| {
                    obligation_operation(stored.session, key)
                }),
                payload: payload.clone(),
                original_receipt_at: frame.original_receipt_at,
                ingress_receipts: frame
                    .key
                    .map(|key| {
                        vec![SmIngressFrameReceipt {
                            message_key: key.message_key,
                            kind: key.kind,
                            semantic_identity_hash: key.semantic_identity_hash,
                        }]
                    })
                    .unwrap_or_default(),
                provenance: frame.key.map_or_else(SmFrameProvenance::default, |key| {
                    SmFrameProvenance::ingress([key.clone()])
                }),
            };
            let admitted = self
                .store
                .admit(
                    AdmissionRequest {
                        authority: AdmissionAuthority::Detached {
                            session: stored.session,
                            generation: stored.generation,
                        },
                        claim: claim.as_ref(),
                        now,
                    },
                    &admission,
                )
                .await;
            match admitted {
                Ok(_) => {
                    return Ok(DurableDetachedAdmission::Admitted {
                        accepting_stream: stream_id,
                    })
                }
                Err(AdmissionError::Indeterminate(error)) if attempts < ADMISSION_ATTEMPTS => {
                    warn!(%stream_id, %error, "detached durable SM admission outcome unknown; offering the frame again");
                }
                Err(AdmissionError::Indeterminate(error)) => {
                    warn!(%stream_id, %error, "detached durable SM admission outcome is still unknown");
                    return Err(SmRegistryError::StorageUnavailable(
                        StorageOutageCause::Backend,
                    ));
                }
                // A resume committed between the read and the admission.
                // Nothing was retained; the session is offered the frame
                // again at the generation it holds now.
                Err(AdmissionError::Rejected(AdmissionRejection::Fence(
                    FenceError::StaleGeneration { .. },
                ))) if attempts < ADMISSION_ATTEMPTS => {
                    tracing::debug!(%stream_id, "detached durable SM session was resumed during an admission; reading it again");
                }
                // A known refusal: the session went live, ended, moved or is
                // full. Nothing is in its custody.
                Err(AdmissionError::Rejected(rejection)) => {
                    tracing::debug!(%stream_id, error = %rejection, "detached durable SM session refused a frame");
                    return Ok(DurableDetachedAdmission::NoSession);
                }
            }
        }
    }
}
