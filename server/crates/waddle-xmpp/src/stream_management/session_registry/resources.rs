use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use xmpp_parsers::presence::Show;

use super::core::InMemorySmSessionRegistry;
use super::session::DetachedSession;
use super::SmRegistryError;
use crate::Stanza;

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
            |session| session.record_detached_outbound(stanza_xml, original_receipt_at),
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
            |session| session.record_detached_outbound(stanza_xml, original_receipt_at),
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
            |session| session.record_detached_outbound(stanza_xml, original_receipt_at),
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
            |session| session.record_detached_outbound(stanza_xml, original_receipt_at),
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
                session.record_detached_outbound_at(sequence, stanza_xml, original_receipt_at);
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
            |session| session.record_detached_outbound(stanza_xml, original_receipt_at),
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
