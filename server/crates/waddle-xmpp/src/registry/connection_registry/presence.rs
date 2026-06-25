use super::*;
use chrono::Utc;

impl ConnectionRegistry {
    /// Update presence state for a connected resource.
    ///
    /// Returns true if the resource was found and updated.
    pub fn update_presence(&self, jid: &FullJid, available: bool, priority: i8) -> bool {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .presence_available
                .store(available, Ordering::Relaxed);
            entry
                .value()
                .presence_priority
                .store(priority, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Update full presence state (show/status/priority/idle) for a connected resource.
    pub fn update_presence_state(
        &self,
        jid: &FullJid,
        show: Option<String>,
        status: Option<String>,
        priority: i8,
        idle_since: Option<chrono::DateTime<Utc>>,
    ) {
        self.presence_states.insert(
            jid.clone(),
            PresenceState {
                show,
                status,
                priority,
                idle_since,
            },
        );
    }

    /// Get the stored presence state for a connected resource.
    pub fn get_presence_state(&self, jid: &FullJid) -> Option<PresenceState> {
        self.presence_states.get(jid).map(|r| r.value().clone())
    }

    /// Clear the stored presence state for a resource (e.g. on unavailable presence).
    pub fn clear_presence_state(&self, jid: &FullJid) {
        self.presence_states.remove(jid);
    }

    /// Record last offline activity for a bare JID.
    pub fn record_last_activity(&self, bare_jid: &BareJid, status: Option<String>) {
        self.last_activity.insert(
            bare_jid.clone(),
            LastActivityState {
                timestamp: Utc::now(),
                status,
            },
        );
    }

    /// Get the last recorded offline activity for a bare JID.
    pub fn get_last_activity(&self, bare_jid: &BareJid) -> Option<LastActivityState> {
        self.last_activity
            .get(bare_jid)
            .map(|entry| entry.value().clone())
    }

    /// Clear the last recorded offline activity for a bare JID.
    pub fn clear_last_activity(&self, bare_jid: &BareJid) {
        self.last_activity.remove(bare_jid);
    }

    /// Return the current server uptime in whole seconds.
    pub fn server_uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Get all available resources for a bare JID with their priorities.
    pub fn get_available_resources_for_user(&self, bare_jid: &BareJid) -> Vec<(FullJid, i8)> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid && entry.value().is_presence_available()
            })
            .map(|entry| (entry.key().clone(), entry.value().presence_priority()))
            .collect()
    }

    /// Remove all stale connections (those with closed channels).
    ///
    /// This can be called periodically to clean up connections that
    /// were not properly unregistered.
    pub fn cleanup_stale(&self) -> usize {
        let mut removed = 0;
        let stale: Vec<FullJid> = self
            .connections
            .iter()
            .filter(|entry| entry.value().sender.is_closed())
            .map(|entry| entry.key().clone())
            .collect();

        for jid in stale {
            if self.unregister(&jid).is_some() {
                debug!(jid = %jid, "Removed stale connection");
                removed += 1;
            }
        }

        if removed > 0 {
            info!(count = removed, "Cleaned up stale connections");
        }

        removed
    }
}
