use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use xmpp_parsers::presence::Show;

use super::core::InMemorySmSessionRegistry;
use super::SmRegistryError;
use crate::Stanza;

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

    /// List detached resources for a bare JID that had XEP-0280 carbons enabled.
    pub async fn detached_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        except: &FullJid,
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
                    && session.jid != *except
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
                        && session.jid != *except
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
    ) -> Result<Option<(Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.values().find(|session| {
            !session.is_expired() && session.presence_available && session.jid == *jid
        }) {
            return Ok(Some((
                session.presence_show.clone(),
                session.presence_status.clone(),
                session.presence_priority,
            )));
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
            .map(|session| {
                (
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            }))
    }

    /// Return last known rich presence state for every detached available
    /// resource owned by `bare_jid`.
    pub async fn available_detached_presence_states_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<(FullJid, Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut states: Vec<(FullJid, Option<Show>, Option<String>, i8)> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| {
                (
                    session.jid.clone(),
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            })
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
                .map(|session| {
                    (
                        session.jid.clone(),
                        session.presence_show.clone(),
                        session.presence_status.clone(),
                        session.presence_priority,
                    )
                }),
        );
        Ok(states)
    }
}
