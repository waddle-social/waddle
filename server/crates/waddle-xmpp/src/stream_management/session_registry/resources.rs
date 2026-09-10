use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use xmpp_parsers::presence::Show;

use super::core::InMemorySmSessionRegistry;
use super::session::DetachedSession;
use super::SmRegistryError;
use crate::{pending_delivery::SmSessionId, Stanza};

/// Outcome of probing whether a full JID still owns resumable XEP-0198 state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumableSessionProbe {
    Present,
    Absent,
    Failed,
}

/// Last broadcast rich presence of a detached available resource.
///
/// `payloads` are the resource's own presence extension elements
/// (XEP-0115 caps, XEP-0319 idle, anything else) exactly as last
/// advertised, so probe/subscription delivery can relay them verbatim
/// while the XEP-0198 stream awaits resume (issue #1103).
#[derive(Debug, Clone)]
pub struct DetachedPresenceState {
    pub resource: FullJid,
    pub show: Option<Show>,
    pub status: Option<String>,
    pub priority: i8,
    pub payloads: Vec<minidom::Element>,
}

impl DetachedPresenceState {
    fn from_session(session: &DetachedSession) -> Self {
        Self {
            resource: session.jid.clone(),
            show: session.presence_show.clone(),
            status: session.presence_status.clone(),
            priority: session.presence_priority,
            payloads: session.presence_payloads.clone(),
        }
    }
}

impl InMemorySmSessionRegistry {
    /// Conservatively probe lifecycle ownership before retiring durable stream metadata.
    /// Includes rows not hydrated after a failed startup read and foreign claims.
    pub async fn has_retirement_protection(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<bool, SmRegistryError> {
        let unavailable =
            || SmRegistryError::StorageUnavailable(super::traits::StorageOutageCause::Backend);
        let locally_owned = || {
            self.locally_owned_claim_ids()
                .ok_or_else(unavailable)
                .map(|ids| ids.iter().any(|id| id == stream_id.as_str()))
        };
        if locally_owned()? {
            return Ok(true);
        }
        let entity = crate::ownership::Entity::new(
            crate::ownership::EntityType::SmSession,
            stream_id.as_str().to_owned(),
        );
        if self
            .claim_store
            .current_claim(&entity)
            .await
            .map_err(|_| unavailable())?
            .is_some()
        {
            return Ok(true);
        }
        if let Some(storage) = &self.persistence {
            if storage
                .get_session(stream_id)
                .await
                .map_err(|_| unavailable())?
                .is_some()
            {
                return Ok(true);
            }
        }
        // A fresh enrollment may have acquired ownership during either read.
        locally_owned()
    }

    fn stanza_to_replay_xml(stanza: &Stanza) -> String {
        let element = stanza.to_element();
        let mut buffer = Vec::new();
        element
            .write_to(&mut buffer)
            .expect("serializing typed stanza should not fail");
        String::from_utf8(buffer).expect("serialized typed stanza is UTF-8")
    }
    /// List detached resources for `bare_jid` that had requested the roster.
    pub async fn interested_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.roster_interested
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.roster_interested
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for `bare_jid` that requested the XEP-0191 blocklist.
    pub async fn blocklist_interested_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.blocklist_interested
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.blocklist_interested
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached interested resource.
    async fn record_outbound_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let Some(stream_id) = self.find_session_id_matching(|session| {
            !session.is_expired() && session.roster_interested && session.jid == *jid
        })?
        else {
            return Ok(false);
        };
        self.update_detached_session_snapshot(
            &stream_id,
            |session| !session.is_expired() && session.roster_interested && session.jid == *jid,
            |session| {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                true
            },
        )
        .await
    }

    async fn record_outbound_for_detached_blocklist_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let Some(stream_id) = self.find_session_id_matching(|session| {
            !session.is_expired() && session.blocklist_interested && session.jid == *jid
        })?
        else {
            return Ok(false);
        };
        self.update_detached_session_snapshot(
            &stream_id,
            |session| !session.is_expired() && session.blocklist_interested && session.jid == *jid,
            |session| {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                true
            },
        )
        .await
    }

    async fn record_outbound_for_detached_bound_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let Some(stream_id) =
            self.find_session_id_matching(|session| !session.is_expired() && session.jid == *jid)?
        else {
            return Ok(false);
        };
        self.update_detached_session_snapshot(
            &stream_id,
            |session| !session.is_expired() && session.jid == *jid,
            |session| {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                true
            },
        )
        .await
    }

    /// Record a typed stanza for one detached interested resource.
    pub async fn record_stanza_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Record a typed stanza for one detached XEP-0191 blocklist-interested resource.
    pub async fn record_stanza_for_detached_blocklist_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_blocklist_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Record a typed stanza for one detached resource by exact FullJID,
    /// regardless of roster-interest or presence-availability flags.
    pub async fn record_stanza_for_detached_bound_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_bound_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Record a typed stanza for an exact detached resource and return the
    /// actual stream whose replay buffer accepted it. Callers that persist an
    /// effect identity must use this rather than inferring a stream from the
    /// target JID, which can be rebound while routing is in flight.
    pub async fn record_stanza_for_detached_bound_resource_with_stream(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<Option<SmSessionId>, SmRegistryError> {
        let stanza_xml = Self::stanza_to_replay_xml(stanza);
        let Some(stream_id) =
            self.find_session_id_matching(|session| !session.is_expired() && session.jid == *jid)?
        else {
            return Ok(None);
        };
        let recorded = self
            .update_detached_session_snapshot(
                &stream_id,
                |session| !session.is_expired() && session.jid == *jid,
                |session| {
                    session.record_detached_outbound(stanza_xml, original_receipt_at);
                    true
                },
            )
            .await?;
        Ok(recorded.then(|| SmSessionId::new(stream_id)))
    }

    /// Whether existing proof stands for a payload that can no longer be delivered.
    ///
    /// An acknowledged entry simply leaves the queue, so its absence is not loss.
    /// An *evicted* one is different: the bounded queue drops the oldest entry and
    /// records a replay gap through its sequence, which is exactly the durable
    /// marker that the payload is gone. Proof covered by that gap is void, because
    /// neither resume nor promotion can produce the stanza any more.
    ///
    /// Decided entirely from durable state, never from the in-memory map. Memory
    /// can still show the entry after another append evicted it, committed, and
    /// was cancelled before publishing; and a same-JID replacement moves the old
    /// stream off both maps into promotion ownership while its durable row still
    /// exists, so a missing map entry is not evidence that delivery completed.
    async fn void_allocation(
        &self,
        storage: &Arc<dyn crate::stream_management::persistence::SmPersistenceStorage>,
        proof: &crate::stream_management::persistence::PersistedIngressAppend,
    ) -> Result<
        Option<crate::stream_management::persistence::PriorIngressAllocation>,
        SmRegistryError,
    > {
        use crate::stream_management::persistence::PriorIngressAllocation;
        use crate::stream_management::sequence::sequence_gt;

        // No durable session left: promotion drained the queue and confirmation
        // retired the row, which only happens once the handed-out queue covered
        // durable state, so the obligation was discharged rather than lost.
        let Some(session) = storage
            .get_session(&proof.accepting_stream)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?
        else {
            return Ok(None);
        };
        // One row decides it. Reading the queue separately would tear: a
        // concurrent eviction committing between the two reads pairs the old gap
        // with the new queue. The queue read is also redundant — eviction always
        // drops the OLDEST entry and marks the gap through its sequence, so every
        // retained sequence is strictly above the gap. Gap-covered therefore
        // already implies not retained.
        //
        // Acknowledged allocations are excluded first. Progress can fail after an
        // append, the client can then resume and acknowledge the stanza, and a
        // later overflow on the re-detached stream can advance the gap past that
        // sequence. Gap coverage alone would misread it as lost and append a
        // duplicate of a stanza the client has already acknowledged.
        if !sequence_gt(proof.sequence, session.last_acked) {
            return Ok(None);
        }
        let evicted = session
            .replay_gap_through
            .is_some_and(|gap| !sequence_gt(proof.sequence, gap));
        Ok(evicted.then(|| PriorIngressAllocation {
            accepting_stream: proof.accepting_stream.clone(),
            sequence: proof.sequence,
        }))
    }

    /// Allocate one ingress obligation exactly once, even after resume or rebind.
    /// The ledger is consulted before looking at any detached session.
    ///
    /// Takes `Arc<Self>` because the durable write must outlive caller
    /// cancellation. Post-commit execution runs under a deadline that can drop
    /// this future mid-`COMMIT`, and the SQLite driver completes an already
    /// submitted commit on its own worker thread regardless. Releasing the
    /// stream shard at that moment would let the next writer read pre-commit
    /// state and overwrite the committed entry while its ledger proof survived,
    /// which is unrecoverable message loss: the proof suppresses every retry.
    /// The persist therefore runs in a task that owns the shard guard, so the
    /// lock is released only once the write has actually resolved.
    pub async fn record_keyed_stanza_for_detached_bound_resource(
        self: &Arc<Self>,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
        key: crate::stream_management::SmIngressAppendKey,
    ) -> Result<crate::stream_management::SmKeyedAppendOutcome, SmRegistryError> {
        use crate::stream_management::{
            persistence::{KeyedSnapshotOutcome, PersistedIngressAppend},
            SmKeyedAppendOutcome,
        };
        let storage = self
            .persistence
            .as_ref()
            .ok_or(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ))?;
        // Proof belongs to the obligation, not the currently bound stream.
        let mut supersedes = None;
        if let Some(proof) = storage
            .get_ingress_append(&key)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?
        {
            match self.void_allocation(storage, &proof).await? {
                // The allocated payload is still deliverable, or the session it
                // belonged to was already promoted or expired away.
                None => {
                    return Ok(SmKeyedAppendOutcome::AlreadyAppended {
                        accepting_stream: proof.accepting_stream,
                    })
                }
                // The payload was evicted from the bounded queue, so the proof
                // stands for a stanza nothing can deliver any more. Suppressing
                // the retry against it would terminalize a lost message, so this
                // allocation is replaced — gated on that exact row, so a racing
                // writer that already replaced it still wins.
                Some(prior) => supersedes = Some(prior),
            }
        }
        if key.resource != *jid {
            return Err(SmRegistryError::Internal(
                "Ingress append resource does not match target JID".to_owned(),
            ));
        }
        let Some(stream_id) =
            self.find_session_id_matching(|session| !session.is_expired() && session.jid == *jid)?
        else {
            return Ok(SmKeyedAppendOutcome::NoSession);
        };
        let accepting_stream = SmSessionId::new(stream_id);
        let shard = self.stream_lock(accepting_stream.as_str())?;
        let guard = shard.lock_owned().await;
        self.reconcile_stale_session_locked(&accepting_stream)
            .await?;
        let Some(mut updated) = self.detached_snapshot_matching(&accepting_stream, |session| {
            !session.is_expired() && session.jid == *jid
        })?
        else {
            return Ok(SmKeyedAppendOutcome::NoSession);
        };
        updated.record_detached_outbound(Self::stanza_to_replay_xml(stanza), original_receipt_at);
        let persisted = super::persistence_codec::detached_to_persisted(&updated)?;
        let rows = updated
            .unacked_stanzas
            .iter()
            .map(|entry| {
                super::persistence_codec::parse_xml_to_persisted_unacked(
                    accepting_stream.as_str(),
                    entry.sequence,
                    &entry.stanza_xml,
                    entry.original_receipt_at,
                    entry.ingress_receipts.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.mark_snapshot_stale(&accepting_stream)?;
        let registry = Arc::clone(self);
        let storage = Arc::clone(storage);
        let sequence = updated
            .unacked_stanzas
            .last()
            .map(|entry| entry.sequence)
            .ok_or_else(|| {
                SmRegistryError::Internal("keyed append produced no queue entry".to_owned())
            })?;
        let append = PersistedIngressAppend {
            key,
            accepting_stream: accepting_stream.clone(),
            sequence,
            appended_at: Utc::now(),
            supersedes,
        };
        // `guard` moves into the task: the shard stays locked until the write
        // resolves, even if this future is dropped first.
        let settle = tokio::spawn(async move {
            let _guard = guard;
            let outcome = storage
                .store_session_atomic_with_ingress_append(persisted, rows, append)
                .await
                .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
            match outcome {
                KeyedSnapshotOutcome::Committed => {
                    // Publication may report displacement, but the transaction still
                    // allocated the entry. Promotion reconciles the captured queue.
                    registry.publish_detached_snapshot(&accepting_stream, updated)?;
                    Ok(SmKeyedAppendOutcome::Appended { accepting_stream })
                }
                KeyedSnapshotOutcome::ObligationAlreadyAllocated { accepting_stream } => {
                    // Discard the clone, including evictions and counters. Keep the
                    // stale mark: the winning writer may have updated durable state.
                    Ok(SmKeyedAppendOutcome::AlreadyAppended { accepting_stream })
                }
            }
        });
        settle
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?
    }

    /// Record a stanza directly against a detached stream id, regardless of
    /// roster-interest or presence-availability flags.
    pub async fn record_outbound_for_detached_stream(
        &self,
        stream_id: &str,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.update_detached_session_snapshot(
            stream_id,
            |session| !session.is_expired(),
            |session| {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                true
            },
        )
        .await
    }

    pub async fn record_outbound_for_detached_stream_at(
        &self,
        stream_id: &str,
        sequence: u32,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.update_detached_session_snapshot(
            stream_id,
            |session| !session.is_expired(),
            |session| {
                session.record_detached_outbound_at(sequence, stanza_xml, original_receipt_at)
            },
        )
        .await
    }

    /// Whether ANY resumable session exists for this exact full JID —
    /// in this node's memory (detached or resume-claimed) OR in the
    /// durable persistence shared across the cluster (#1249, SM-hunter
    /// review on PR #1277). The remote-MUC reconciliation janitor gates
    /// occupancy re-drives on this: a session that detached on this
    /// node and was later resume-stolen by another node leaves no local
    /// trace, but its durable row (owned by the stealing node) proves
    /// the occupancy is still legitimately resumable and MUST NOT be
    /// evicted. Fail-closed: a durable read error reports `true` so the
    /// caller skips the eviction.
    pub async fn any_resumable_session_for_full_jid(&self, jid: &FullJid) -> bool {
        matches!(
            self.probe_resumable_session_for_full_jid(jid).await,
            ResumableSessionProbe::Present | ResumableSessionProbe::Failed
        )
    }

    /// Typed variant of [`Self::any_resumable_session_for_full_jid`] for
    /// sweep callers that must distinguish fail-closed reads from live state.
    pub async fn probe_resumable_session_for_full_jid(
        &self,
        jid: &FullJid,
    ) -> ResumableSessionProbe {
        let in_memory = {
            let matches_memory =
                |sessions: &std::collections::HashMap<String, super::super::DetachedSession>| {
                    sessions
                        .values()
                        .any(|session| !session.is_expired() && session.jid == *jid)
                };
            let sessions = self.sessions.read();
            let claimed = self.claimed_sessions.read();
            match (sessions, claimed) {
                (Ok(sessions), Ok(claimed)) => {
                    matches_memory(&sessions) || matches_memory(&claimed)
                }
                // Poisoned lock: fail closed.
                _ => return ResumableSessionProbe::Failed,
            }
        };
        if in_memory {
            return ResumableSessionProbe::Present;
        }
        let Some(persistence) = self.persistence.as_ref() else {
            return ResumableSessionProbe::Absent;
        };
        match persistence.list_all_sessions().await {
            Ok(rows) => {
                let now = chrono::Utc::now();
                if rows.iter().any(|row| {
                    row.jid == *jid
                        && now.signed_duration_since(row.detached_at).to_std().ok()
                            <= Some(row.max_resume_duration)
                }) {
                    ResumableSessionProbe::Present
                } else {
                    ResumableSessionProbe::Absent
                }
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    %error,
                    "any_resumable_session_for_full_jid: durable read failed; failing closed"
                );
                ResumableSessionProbe::Failed
            }
        }
    }

    /// List all detached resources for a bare JID, including resources that
    /// were not available at detach time.
    pub async fn detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for a bare JID that had XEP-0280 carbons
    /// enabled, excluding every full JID in `except`.
    ///
    /// `except` is the original stanza's delivery set (XEP-0280 §6.3:
    /// clients addressed by the original MUST NOT also get a forwarded
    /// copy).
    pub async fn detached_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        except: &[FullJid],
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                session.carbons_enabled
                    && !session.is_expired()
                    && session.jid.to_bare() == *bare_jid
                    && !except.contains(&session.jid)
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    session.carbons_enabled
                        && !session.is_expired()
                        && session.jid.to_bare() == *bare_jid
                        && !except.contains(&session.jid)
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for `bare_jid` that were available at detach.
    pub async fn available_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached resource that was available at detach.
    async fn record_outbound_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let Some(stream_id) = self.find_session_id_matching(|session| {
            !session.is_expired() && session.presence_available && session.jid == *jid
        })?
        else {
            return Ok(false);
        };
        self.update_detached_session_snapshot(
            &stream_id,
            |session| !session.is_expired() && session.presence_available && session.jid == *jid,
            |session| {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                true
            },
        )
        .await
    }

    /// Record a typed stanza for one detached resource that was available at detach.
    pub async fn record_stanza_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_available_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Return last known rich presence state for a detached available resource.
    pub async fn detached_presence_state(
        &self,
        jid: &FullJid,
    ) -> Result<Option<DetachedPresenceState>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.values().find(|session| {
            !session.is_expired() && session.presence_available && session.jid == *jid
        }) {
            return Ok(Some(DetachedPresenceState::from_session(session)));
        }
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .values()
            .find(|session| {
                !session.is_expired() && session.presence_available && session.jid == *jid
            })
            .map(DetachedPresenceState::from_session))
    }

    /// Return last known rich presence state for every detached available
    /// resource owned by `bare_jid`.
    pub async fn available_detached_presence_states_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<DetachedPresenceState>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut states: Vec<DetachedPresenceState> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(DetachedPresenceState::from_session)
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        states.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(DetachedPresenceState::from_session),
        );
        Ok(states)
    }
}
