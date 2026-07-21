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
        self.register_with_stream_state(jid, sender, carbons_enabled, false, false)
    }

    /// Register a connection and seed per-stream feature state.
    #[instrument(skip(self, sender), fields(jid = %jid, carbons = carbons_enabled, roster_interested = roster_interested, blocklist_interested = blocklist_interested))]
    pub fn register_with_stream_state(
        &self,
        jid: FullJid,
        sender: mpsc::Sender<OutboundStanza>,
        carbons_enabled: bool,
        roster_interested: bool,
        blocklist_interested: bool,
    ) -> Arc<AtomicBool> {
        let entry = ConnectionEntry::new(sender);
        if carbons_enabled {
            entry.carbons_enabled.store(true, Ordering::Relaxed);
        }
        if roster_interested {
            entry.roster_interested.store(true, Ordering::Relaxed);
        }
        if blocklist_interested {
            entry.blocklist_interested.store(true, Ordering::Relaxed);
        }
        let carbons_handle = entry.carbons_handle();
        let existing = self.connections.insert(jid.clone(), entry);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            crate::metrics::adjust_connections_active(1);
            debug!("Registered new connection");
        }
        carbons_handle
    }

    /// Register an already-constructed connection entry.
    ///
    /// This is used by clustering's owner-side remote-resource mirror: the
    /// owner `UserActor` and owner `ConnectionRegistry` must share the exact
    /// same [`ConnectionEntry`] so registry-backed fanout surfaces and
    /// actor-backed full-JID routing both drain into the remote-resource
    /// forwarder.
    #[instrument(skip(self, entry), fields(jid = %jid))]
    pub fn register_entry(&self, jid: FullJid, entry: ConnectionEntry) -> Arc<AtomicBool> {
        let carbons_handle = entry.carbons_handle();
        let existing = self.connections.insert(jid, entry);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            crate::metrics::adjust_connections_active(1);
            debug!("Registered new connection");
        }
        carbons_handle
    }

    /// Register a prebuilt entry only when the full-JID slot is empty or still
    /// belongs to the provided owner token.
    pub fn register_entry_if_owner_or_absent(
        &self,
        jid: FullJid,
        entry: ConnectionEntry,
        owner: &Arc<AtomicBool>,
    ) -> bool {
        if !Arc::ptr_eq(&entry.carbons_enabled, owner) {
            debug!("Skipped register: entry owner does not match guard owner");
            return false;
        }
        match self.connections.entry(jid) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if !Arc::ptr_eq(&occupied.get().carbons_enabled, owner) {
                    debug!("Skipped register: ownership moved to replacement connection");
                    return false;
                }
                occupied.insert(entry);
                debug!("Replaced owned connection registration");
                true
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(entry);
                crate::metrics::adjust_connections_active(1);
                debug!("Registered new connection");
                true
            }
        }
    }

    /// Unregister a connection.
    ///
    /// Returns the connection entry if the connection was registered, None otherwise.
    #[instrument(skip(self), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        let removed = self.connections.remove(jid);
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
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
            crate::metrics::adjust_connections_active(-1);
            self.presence_states.remove(jid);
            debug!("Unregistered owned connection");
        } else {
            debug!("Skipped unregister: ownership moved to replacement connection");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Unregister a connection only if its published XEP-0198 SM session id
    /// matches `stream_id` — i.e. the current entry still belongs to that exact
    /// SM session.
    ///
    /// Used by the SM-expiry janitor: an expired session S1 must not evict a
    /// replacement session S2 that rebound the same full JID after S1 detached.
    /// S2 carries a different (or no) published stream id, so the predicate
    /// fails and nothing is removed. Returns the removed entry only when it is
    /// genuinely S1's, so the caller's actor mirror is gated on S1's own token
    /// too.
    #[instrument(skip(self), fields(jid = %jid, stream_id = %stream_id))]
    pub fn unregister_if_sm_stream_id(
        &self,
        jid: &FullJid,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            entry.sm_stream_id().as_ref() == Some(stream_id)
        });
        if removed.is_some() {
            crate::metrics::adjust_connections_active(-1);
            self.presence_states.remove(jid);
            debug!("Unregistered SM-owned connection at expiry");
        } else {
            debug!("Skipped expiry unregister: entry belongs to a different session or is absent");
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

    /// Check if a JID is currently connected. ADR-0017 Phase 3 Slice 9
    /// retired its former production caller (the Slice-1 DashMap-liveness
    /// selection filter); it is retained as a connection-state introspection
    /// utility used widely across the registry/websocket test suites to
    /// assert connect/disconnect transitions.
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
