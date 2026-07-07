use super::*;

impl ConnectionRegistry {
    /// Mark a connected resource as interested in roster pushes.
    ///
    /// RFC 6121 defines interested resources as those that requested the
    /// roster during this session. Roster pushes are sent only to these
    /// resources.
    pub fn mark_roster_interested(&self, jid: &FullJid) {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .roster_interested
                .store(true, Ordering::Relaxed);
        }
    }

    /// Check whether a connected resource is interested in roster pushes.
    pub fn is_roster_interested(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .map(|entry| entry.value().roster_interested.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Get all connected interested resources for a bare JID.
    pub fn get_roster_interested_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid
                    && entry.value().roster_interested.load(Ordering::Relaxed)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Mark a connected resource as interested in XEP-0191 blocklist pushes.
    ///
    /// XEP-0191 sends block/unblock pushes only to resources that requested the
    /// blocklist during the current session.
    pub fn mark_blocklist_interested(&self, jid: &FullJid) {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .blocklist_interested
                .store(true, Ordering::Relaxed);
        }
    }

    /// Check whether a connected resource is interested in XEP-0191 blocklist pushes.
    pub fn is_blocklist_interested(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .map(|entry| entry.value().blocklist_interested.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Get all connected resources for a bare JID that requested the blocklist.
    pub fn get_blocklist_interested_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid
                    && entry.value().blocklist_interested.load(Ordering::Relaxed)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Send a stanza to multiple recipients.
    ///
    /// Returns a vector of (jid, result) pairs for each recipient.
    pub async fn send_to_many<'a, I>(
        &self,
        recipients: I,
        stanza: Stanza,
    ) -> Vec<(FullJid, SendResult)>
    where
        I: IntoIterator<Item = &'a FullJid>,
    {
        let mut results = Vec::new();

        for jid in recipients {
            let result = self.send_to(jid, stanza.clone()).await;
            results.push((jid.clone(), result));
        }

        results
    }

    /// List all connected JIDs.
    ///
    /// Useful for debugging and monitoring.
    pub fn list_connections(&self) -> Vec<FullJid> {
        self.connections.iter().map(|r| r.key().clone()).collect()
    }

    /// Snapshot every active connection's published XEP-0198 SM
    /// session id. Used by the `pending_delivery` claim-expiry
    /// janitor (issue #209 PR #360) to extend its "live SM session"
    /// set with currently-connected sessions — the
    /// `sm_session_registry` only knows about detached/resumable
    /// sessions, not active ones, so without this the janitor would
    /// wrongly treat actively-claimed-but-not-yet-acked rows as
    /// orphaned and release them. (Codex/Qodo review on PR #360.)
    pub fn active_sm_stream_ids(&self) -> Vec<crate::pending_delivery::SmSessionId> {
        self.connections
            .iter()
            .filter_map(|entry| entry.value().sm_stream_id())
            .collect()
    }

    /// Get all connected resources for a bare JID, excluding a specific full JID.
    ///
    /// Returns all full JIDs that match the bare JID except the excluded one.
    /// This does NOT filter by carbons status — callers that are routing
    /// XEP-0280 carbon copies should use [`Self::get_other_carbon_resources_for_user`]
    /// instead so that non-opted-in resources are not sent carbon-wrapped stanzas.
    pub fn get_other_resources_for_user(
        &self,
        bare_jid: &BareJid,
        exclude_jid: &FullJid,
    ) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                let jid = entry.key();
                // Match bare JID but exclude the specific full JID
                jid.to_bare() == *bare_jid && jid != exclude_jid
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get all resources for a bare JID that have XEP-0280 Message Carbons
    /// enabled, excluding every full JID in `exclude_jids`.
    ///
    /// Per XEP-0280 §5, carbons must be enabled per-resource. The server must
    /// only deliver `<sent>` and `<received>` carbon copies to resources that
    /// have explicitly opted in via `<enable xmlns='urn:xmpp:carbons:2'/>`.
    /// `exclude_jids` is the original stanza's delivery set (XEP-0280 §6.3:
    /// clients addressed by the original MUST NOT also get a forwarded copy).
    pub fn get_other_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        exclude_jids: &[FullJid],
    ) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                let jid = entry.key();
                jid.to_bare() == *bare_jid
                    && !exclude_jids.contains(jid)
                    && entry.value().is_carbons_enabled()
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Check whether the given full JID has XEP-0280 Message Carbons enabled.
    ///
    /// Returns false if the JID is not connected.
    pub fn is_carbons_enabled(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .map(|entry| entry.value().is_carbons_enabled())
            .unwrap_or(false)
    }

    /// Update the XEP-0280 Message Carbons opt-in flag for a connected resource.
    ///
    /// Returns false when the resource is not currently connected.
    pub fn set_carbons_enabled(&self, jid: &FullJid, enabled: bool) -> bool {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .carbons_enabled
                .store(enabled, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}
