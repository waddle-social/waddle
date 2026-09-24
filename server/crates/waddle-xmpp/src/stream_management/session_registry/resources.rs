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

    /// Allocate one ingress obligation exactly once, even after resume or rebind.
    /// The ledger is consulted before looking at any detached session.
    ///
    /// Takes `Arc<Self>` because the durable write must outlive caller
    /// cancellation. Post-commit execution runs under a deadline that can drop
    /// this future mid-`COMMIT`, and the SQLite driver completes an already
    /// submitted commit on its own worker thread regardless. Releasing the
    /// stream shard at that moment would let the next writer read pre-commit
    /// state and overwrite the committed replay entry. Custody would survive,
    /// but the replay snapshot and counters must still remain coherent.
    /// The persist therefore runs in a task that owns the shard guard, so the
    /// lock is released only once the write has actually resolved.
    pub async fn record_keyed_stanza_for_detached_bound_resource(
        self: &Arc<Self>,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
        key: crate::stream_management::SmIngressAppendKey,
    ) -> Result<crate::stream_management::SmKeyedAppendOutcome, SmRegistryError> {
        use crate::stream_management::SmKeyedAppendOutcome;
        match self.consult_ingress_append_ledger(&key).await? {
            LedgerDecision::Allocated { accepting_stream } => {
                return Ok(SmKeyedAppendOutcome::AlreadyAppended { accepting_stream })
            }
            LedgerDecision::Unallocated => {}
        };
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
        let stanza_xml = Self::stanza_to_replay_xml(stanza);
        self.commit_keyed_detached_entry(stream_id, key, move |session| {
            session.record_detached_outbound(stanza_xml, original_receipt_at);
            session.unacked_stanzas.last().map(|entry| entry.sequence)
        })
        .await
    }

    /// Key a detach-drained frame at the connection's own sequence (issue #1789).
    ///
    /// The drain mirrors the connection-local counter, so unlike
    /// [`Self::record_keyed_stanza_for_detached_bound_resource`] the sequence is the
    /// caller's. Call this *before* counting the frame locally: on
    /// [`SmKeyedAppendOutcome::AlreadyAppended`] the frame must be dropped uncounted,
    /// because nothing on the drain path reached a wire and the client's `h` can never
    /// include it. `NoSession` also covers a slot the session cannot take (already
    /// acknowledged or occupied); nothing was written and no proof exists.
    pub async fn record_keyed_outbound_for_detached_stream_at(
        self: &Arc<Self>,
        stream_id: &str,
        sequence: u32,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
        key: crate::stream_management::SmIngressAppendKey,
    ) -> Result<crate::stream_management::SmKeyedAppendOutcome, SmRegistryError> {
        let stanza_xml = Self::stanza_to_replay_xml(stanza);
        use crate::stream_management::SmKeyedAppendOutcome;
        match self.consult_ingress_append_ledger(&key).await? {
            LedgerDecision::Allocated { accepting_stream } => {
                return Ok(SmKeyedAppendOutcome::AlreadyAppended { accepting_stream })
            }
            LedgerDecision::Unallocated => {}
        };
        self.commit_keyed_detached_entry(stream_id.to_owned(), key, move |session| {
            session
                .record_detached_outbound_at(sequence, stanza_xml, original_receipt_at)
                .then_some(sequence)
        })
        .await
    }

    /// Read the ledger for a frame about to be drained from a detaching socket.
    ///
    /// `None` means the obligation already has durable custody: drop the
    /// frame uncounted. `Some` is unallocated as of this read; bind it to the frame's
    /// sequence and hand it to [`Self::store_session_with_drained_ingress_appends`].
    pub async fn reserve_drained_ingress_append(
        &self,
        key: crate::stream_management::SmIngressAppendKey,
    ) -> Result<Option<crate::stream_management::SmDrainedAppendTicket>, SmRegistryError> {
        Ok(match self.consult_ingress_append_ledger(&key).await? {
            LedgerDecision::Allocated { .. } => None,
            LedgerDecision::Unallocated => {
                Some(crate::stream_management::SmDrainedAppendTicket { key })
            }
        })
    }

    /// Proof belongs to the obligation, not the currently bound stream, so the
    /// ledger is consulted before looking at any detached session.
    async fn consult_ingress_append_ledger(
        &self,
        key: &crate::stream_management::SmIngressAppendKey,
    ) -> Result<LedgerDecision, SmRegistryError> {
        let storage = self
            .persistence
            .as_ref()
            .ok_or(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ))?;
        let Some(proof) = storage
            .get_ingress_append(key)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?
        else {
            return Ok(LedgerDecision::Unallocated);
        };
        Ok(LedgerDecision::Allocated {
            accepting_stream: proof.accepting_stream,
        })
    }

    /// Record one queue entry and its ledger proof in a single transaction.
    ///
    /// `record` mutates a clone of the snapshot and returns the sequence it
    /// allocated, or `None` when it allocated nothing.
    async fn commit_keyed_detached_entry(
        self: &Arc<Self>,
        stream_id: String,
        key: crate::stream_management::SmIngressAppendKey,
        record: impl FnOnce(&mut super::super::DetachedSession) -> Option<u32>,
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
        let accepting_stream = SmSessionId::new(stream_id);
        let shard = self.stream_lock(accepting_stream.as_str())?;
        let guard = shard.lock_owned().await;
        self.reconcile_stale_session_locked(&accepting_stream)
            .await?;
        let Some(mut updated) = self.detached_snapshot_matching(&accepting_stream, |session| {
            !session.is_expired() && session.jid == key.resource
        })?
        else {
            return Ok(SmKeyedAppendOutcome::NoSession);
        };
        if [
            crate::ingress::IngressEffectKind::Carbons,
            crate::ingress::IngressEffectKind::RelayCarbons,
        ]
        .iter()
        .any(|kind| kind.storage_tag() == key.kind.to_storage())
            && !updated.carbons_enabled
        {
            // Evaluate opt-in under the stream shard after reconciliation,
            // against the exact session that would receive the append.
            return Ok(SmKeyedAppendOutcome::Suppressed);
        }
        let Some(sequence) = record(&mut updated) else {
            return Ok(SmKeyedAppendOutcome::NoSession);
        };
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
        let entry = rows
            .iter()
            .find(|entry| entry.sequence == sequence)
            .ok_or_else(|| {
                SmRegistryError::Internal("allocated payload missing from snapshot".into())
            })?;
        let append = PersistedIngressAppend {
            key,
            accepting_stream: accepting_stream.clone(),
            sequence,
            appended_at: Utc::now(),
            payload: *entry.stanza.clone(),
            original_receipt_at: entry.original_receipt_at,
            disposition: crate::stream_management::persistence::IngressCustodyDisposition::Pending,
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
        // Scoped to the exact full JID: #1803 runs this probe per roster-absent
        // occupant, locally and on every cluster peer, inside a bounded fan-out
        // budget, so enumerating `sm_sessions` here would cost
        // `occupants x peers x stored sessions` row decodes per pass.
        match persistence.list_sessions_for_full_jid(jid).await {
            Ok(rows) => {
                let now = chrono::Utc::now();
                if rows.iter().any(|row| {
                    now.signed_duration_since(row.detached_at).to_std().ok()
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

/// What the `sm_ingress_appends` ledger says about one obligation.
enum LedgerDecision {
    Allocated { accepting_stream: SmSessionId },
    Unallocated,
}
