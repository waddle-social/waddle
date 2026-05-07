use super::*;

impl ConnectionRegistry {
    /// Register a connection with its outbound channel.
    ///
    /// Returns a handle to the carbons_enabled flag that the WebSocket C2S adapter
    /// can use to update the carbons status when enable/disable IQs are received.
    ///
    /// If a connection with the same JID already exists, it will be replaced.
    /// This handles reconnection scenarios where a client reconnects with
    /// the same resource before the old connection is cleaned up.
    #[instrument(skip(self, sender), fields(jid = %jid))]
    pub fn register(&self, jid: FullJid, sender: mpsc::Sender<OutboundStanza>) -> Arc<AtomicBool> {
        self.register_with_carbons(jid, sender, false)
    }

    /// Register a connection and seed its XEP-0280 carbons opt-in to
    /// `carbons_enabled`. Used by the XEP-0198 stream-resume path so a
    /// resumed stream keeps the carbons flag it negotiated before the
    /// disconnect instead of silently reverting to the disabled default.
    #[instrument(skip(self, sender), fields(jid = %jid, carbons = carbons_enabled))]
    pub fn register_with_carbons(
        &self,
        jid: FullJid,
        sender: mpsc::Sender<OutboundStanza>,
        carbons_enabled: bool,
    ) -> Arc<AtomicBool> {
        self.register_with_stream_state(jid, sender, carbons_enabled, false)
    }

    /// Register a connection and seed per-stream feature state.
    #[instrument(skip(self, sender), fields(jid = %jid, carbons = carbons_enabled, roster_interested = roster_interested))]
    pub fn register_with_stream_state(
        &self,
        jid: FullJid,
        sender: mpsc::Sender<OutboundStanza>,
        carbons_enabled: bool,
        roster_interested: bool,
    ) -> Arc<AtomicBool> {
        let entry = ConnectionEntry::new(sender);
        if carbons_enabled {
            entry.carbons_enabled.store(true, Ordering::Relaxed);
        }
        if roster_interested {
            entry.roster_interested.store(true, Ordering::Relaxed);
        }
        let carbons_handle = entry.carbons_handle();
        let existing = self.connections.insert(jid.clone(), entry);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            prometheus::increment_connected_users();
            debug!("Registered new connection");
        }
        carbons_handle
    }

    /// Unregister a connection.
    ///
    /// Returns the connection entry if the connection was registered, None otherwise.
    #[instrument(skip(self), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        let removed = self.connections.remove(jid);
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!("Unregistered connection");
        } else {
            debug!("Connection was not registered");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Unregister a connection only if the current registry entry belongs to
    /// the provided carbons handle (i.e. this actor still owns the slot).
    #[instrument(skip(self, carbons_handle), fields(jid = %jid))]
    pub fn unregister_if_owner(
        &self,
        jid: &FullJid,
        carbons_handle: &Arc<AtomicBool>,
    ) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            Arc::ptr_eq(&entry.carbons_enabled, carbons_handle)
        });
        if removed.is_some() {
            prometheus::decrement_connected_users();
            self.presence_states.remove(jid);
            debug!("Unregistered owned connection");
        } else {
            debug!("Skipped unregister: ownership moved to replacement connection");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Return the current entry only if it still belongs to the provided owner.
    pub fn entry_if_owner(
        &self,
        jid: &FullJid,
        carbons_handle: &Arc<AtomicBool>,
    ) -> Option<ConnectionEntry> {
        self.connections.get(jid).and_then(|entry| {
            if Arc::ptr_eq(&entry.carbons_enabled, carbons_handle) {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    /// Check if a JID is currently connected.
    pub fn is_connected(&self, jid: &FullJid) -> bool {
        self.connections.contains_key(jid)
    }

    /// Look up the [`ConnectionEntry`] for a registered full JID.
    ///
    /// Used by handlers that need to inspect or atomically transition
    /// per-connection flags (e.g. the XEP-0160 offline-flush CAS via
    /// [`ConnectionEntry::claim_offline_flush`]).
    pub fn get_entry(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        self.connections.get(jid).map(|entry| entry.clone())
    }

    /// Get the number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}
