use super::{
    debug, instrument, ConnectionEntry, ConnectionRegistry, FullJid, LocalBindingPermit,
    LocalConnectionBindingId, Ordering, SocketOutboundEndpoint,
};
use crate::pending_delivery::SmSessionId;

impl ConnectionRegistry {
    /// Register a connection with its opaque outbound socket endpoint.
    ///
    /// Returns the local identity of the published binding.
    ///
    /// If a connection with the same JID already exists, it will be replaced.
    /// This handles reconnection scenarios where a client reconnects with
    /// the same resource before the old connection is cleaned up.
    #[instrument(skip(self, endpoint, jid), fields(jid = %jid))]
    pub fn register(
        &self,
        jid: FullJid,
        endpoint: impl Into<SocketOutboundEndpoint>,
    ) -> LocalConnectionBindingId {
        self.register_with_carbons(jid, endpoint, false)
    }

    /// Register a connection and seed its XEP-0280 carbons opt-in to
    /// `carbons_enabled`. Used by the XEP-0198 stream-resume path so a
    /// resumed stream keeps the carbons flag it negotiated before the
    /// disconnect instead of silently reverting to the disabled default.
    #[instrument(skip(self, endpoint, jid), fields(jid = %jid, carbons = carbons_enabled))]
    pub fn register_with_carbons(
        &self,
        jid: FullJid,
        endpoint: impl Into<SocketOutboundEndpoint>,
        carbons_enabled: bool,
    ) -> LocalConnectionBindingId {
        self.register_with_stream_state(jid, endpoint, carbons_enabled, false, false)
    }

    /// Register a connection and seed per-stream feature state.
    #[instrument(skip(self, endpoint, jid), fields(jid = %jid, carbons = carbons_enabled, roster_interested = roster_interested, blocklist_interested = blocklist_interested))]
    pub fn register_with_stream_state(
        &self,
        jid: FullJid,
        endpoint: impl Into<SocketOutboundEndpoint>,
        carbons_enabled: bool,
        roster_interested: bool,
        blocklist_interested: bool,
    ) -> LocalConnectionBindingId {
        let entry = ConnectionEntry::new(endpoint);
        if carbons_enabled {
            entry.carbons_enabled.store(true, Ordering::Relaxed);
        }
        if roster_interested {
            entry.roster_interested.store(true, Ordering::Relaxed);
        }
        if blocklist_interested {
            entry.blocklist_interested.store(true, Ordering::Relaxed);
        }
        self.register_entry(jid, entry)
    }

    /// Register an already-constructed connection entry.
    ///
    /// This is used by clustering's owner-side remote-resource mirror: the
    /// owner `UserActor` and owner `ConnectionRegistry` must share the exact
    /// same [`ConnectionEntry`] so registry-backed fanout surfaces and
    /// actor-backed full-JID routing both drain into the remote-resource
    /// forwarder.
    #[instrument(skip(self, entry, jid), fields(jid = %jid))]
    pub fn register_entry(&self, jid: FullJid, entry: ConnectionEntry) -> LocalConnectionBindingId {
        let binding_id = entry.binding_id();
        let retired = match self.connections.entry(jid.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let retire = !occupied.get().same_exact_local_binding(&entry);
                if retire {
                    occupied.get().binding_permit.revoke_deferred();
                }
                if occupied.get().sm_stream_id() != entry.sm_stream_id() {
                    Self::remove_sm_stream_owner(&self.sm_stream_owners, &jid, occupied.get());
                }
                let old = occupied.insert(entry);
                debug!("Replaced existing connection registration");
                retire.then_some(old)
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(entry);
                crate::metrics::adjust_connections_active(1);
                debug!("Registered new connection");
                None
            }
        };
        if let Some(old) = retired {
            old.binding_permit.notify_revoked();
        }
        binding_id
    }

    /// Publish a physical socket's exact binding under its write-stop gate.
    /// A stop that wins first refuses publication. The route shard is held
    /// before the socket gate; no code under the socket gate reads SM state.
    pub fn register_socket_entry(
        &self,
        jid: FullJid,
        entry: ConnectionEntry,
    ) -> Option<LocalConnectionBindingId> {
        let binding_id = entry.binding_id();
        let binding = entry.binding_ref()?.clone();
        let interruption = entry.write_interruption.clone();
        let retired = match self.connections.entry(jid.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let remove_old_stream = occupied.get().sm_stream_id() != entry.sm_stream_id();
                let same_binding = occupied.get().same_exact_local_binding(&entry);
                let old_permit = occupied.get().binding_permit.clone();
                // A start takes old permit -> physical socket gate. Take those
                // locks in the same order even when both entries share a socket.
                // Failed publication leaves the incumbent permit unchanged.
                let publish = || interruption.publish_binding(&binding, || occupied.insert(entry));
                let old = if same_binding {
                    publish()?
                } else {
                    old_permit.revoke_on_success(publish)?
                };
                if remove_old_stream {
                    Self::remove_sm_stream_owner(&self.sm_stream_owners, &jid, &old);
                }
                debug!("Replaced existing connection registration");
                (!same_binding).then_some(old)
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                interruption.publish_binding(&binding, || {
                    drop(vacant.insert(entry));
                })?;
                crate::metrics::adjust_connections_active(1);
                debug!("Registered new connection");
                None
            }
        };
        if let Some(old) = retired {
            old.binding_permit.notify_revoked();
        }
        Some(binding_id)
    }

    /// Register a prebuilt entry only when the full-JID slot is empty or still
    /// belongs to the provided owner token.
    pub fn register_entry_if_owner_or_absent(
        &self,
        jid: FullJid,
        entry: ConnectionEntry,
        owner: &LocalConnectionBindingId,
    ) -> bool {
        if entry.binding_id() != *owner {
            debug!("Skipped register: entry owner does not match guard owner");
            return false;
        }
        let retired = match self.connections.entry(jid.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if occupied.get().binding_id() != *owner {
                    debug!("Skipped register: ownership moved to replacement connection");
                    return false;
                }
                let retire = !occupied.get().same_exact_local_binding(&entry);
                if retire {
                    occupied.get().binding_permit.revoke_deferred();
                }
                if occupied.get().sm_stream_id() != entry.sm_stream_id() {
                    Self::remove_sm_stream_owner(&self.sm_stream_owners, &jid, occupied.get());
                }
                let old = occupied.insert(entry);
                debug!("Replaced owned connection registration");
                retire.then_some(old)
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(entry);
                crate::metrics::adjust_connections_active(1);
                debug!("Registered new connection");
                None
            }
        };
        if let Some(old) = retired {
            old.binding_permit.notify_revoked();
        }
        true
    }

    /// Unregister a connection.
    ///
    /// Returns the connection entry if the connection was registered, None otherwise.
    #[instrument(skip(self, jid), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            entry.binding_permit.revoke_deferred();
            Self::remove_sm_stream_owner(&self.sm_stream_owners, jid, entry);
            Self::remove_retired_presence(&self.presence_states, jid);
            true
        });
        if let Some((_, entry)) = &removed {
            entry.binding_permit.notify_revoked();
            crate::metrics::adjust_connections_active(-1);
            debug!("Unregistered connection");
        } else {
            debug!("Connection was not registered");
        }
        removed.map(|(_, entry)| entry)
    }

    /// Whether the current registry entry for `jid` still belongs to the
    /// connection holding `owner` (its local binding identity). A
    /// connection that lost its slot to a same-FullJID replacement must not
    /// perform FullJID-keyed writes such as a MUC join (#1703).
    pub fn is_owned_by(&self, jid: &FullJid, owner: &LocalConnectionBindingId) -> bool {
        self.connections
            .get(jid)
            .is_some_and(|entry| entry.binding_id() == *owner)
    }

    /// Unregister a connection only if the current registry entry belongs to
    /// the provided local binding identity (i.e. this actor still owns the slot).
    #[instrument(skip(self, owner, jid), fields(jid = %jid))]
    pub fn unregister_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
    ) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            if entry.binding_id() != *owner {
                return false;
            }
            entry.binding_permit.revoke_deferred();
            Self::remove_sm_stream_owner(&self.sm_stream_owners, jid, entry);
            Self::remove_retired_presence(&self.presence_states, jid);
            true
        });
        if let Some((_, entry)) = &removed {
            entry.binding_permit.notify_revoked();
            crate::metrics::adjust_connections_active(-1);
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
    #[instrument(skip(self, jid), fields(jid = %jid, stream_id = %stream_id))]
    pub fn unregister_if_sm_stream_id(
        &self,
        jid: &FullJid,
        stream_id: &SmSessionId,
    ) -> Option<ConnectionEntry> {
        let removed = self.connections.remove_if(jid, |_, entry| {
            if entry.sm_stream_id().as_ref() != Some(stream_id) {
                return false;
            }
            entry.binding_permit.revoke_deferred();
            Self::remove_sm_stream_owner(&self.sm_stream_owners, jid, entry);
            Self::remove_retired_presence(&self.presence_states, jid);
            true
        });
        if let Some((_, entry)) = &removed {
            entry.binding_permit.notify_revoked();
            crate::metrics::adjust_connections_active(-1);
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
        owner: &LocalConnectionBindingId,
    ) -> Option<ConnectionEntry> {
        self.connections.get(jid).and_then(|entry| {
            if entry.binding_id() == *owner {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    /// Capture a permit only for the exact current local binding. The route
    /// shard excludes removal until the shared revocation cell is captured.
    pub fn local_binding_permit_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
    ) -> Option<LocalBindingPermit> {
        let entry = self.connections.get(jid)?;
        if entry.binding_id() != *owner
            || !entry.is_locally_hosted()
            || entry.endpoint.check_active().is_err()
        {
            return None;
        }
        let binding = entry.binding_ref()?.clone();
        if !entry.binding_permit.is_active() {
            return None;
        }
        Some(LocalBindingPermit::new(
            jid.clone(),
            *owner,
            binding,
            entry.binding_permit.clone(),
        ))
    }

    /// Capture the SM enable permit only when the current entry is wired to
    /// the actual socket owner's control and exact full-JID binding. A later
    /// removal still revokes the shared permit before a start can win.
    pub fn local_sm_enable_binding_permit_if_owner(
        &self,
        jid: &FullJid,
        owner: &LocalConnectionBindingId,
        binding: &crate::ownership::BindingRef,
        control: &std::sync::Arc<super::LiveResumeControl>,
    ) -> Option<LocalBindingPermit> {
        let entry = self.entry_if_owner(jid, owner)?;
        if entry.binding_ref() != Some(binding) {
            return None;
        }
        let bound_control = entry.resume_control()?;
        if !std::sync::Arc::ptr_eq(&bound_control, control)
            || control.with_exact_binding(jid, binding, || ()).is_none()
        {
            return None;
        }
        let permit = self.local_binding_permit_if_owner(jid, owner)?;
        if permit.binding() != binding || !permit.shares_state(&entry.binding_permit) {
            return None;
        }
        Some(permit)
    }

    /// Check if a JID is currently connected. ADR-0017 Phase 3 Slice 9
    /// retired its former production caller (the Slice-1 DashMap-liveness
    /// selection filter); it is retained as a connection-state introspection
    /// utility used widely across the registry/websocket test suites to
    /// assert connect/disconnect transitions.
    pub fn is_connected(&self, jid: &FullJid) -> bool {
        self.connections.contains_key(jid)
    }

    /// Whether this node hosts the socket registered for this exact full JID.
    /// Remote routing mirrors and absent entries both return false.
    pub fn is_locally_hosted(&self, jid: &FullJid) -> bool {
        self.connections
            .get(jid)
            .is_some_and(|entry| entry.is_locally_hosted())
    }

    /// Look up the [`ConnectionEntry`] for a registered full JID.
    ///
    /// Used by handlers that need to inspect or atomically transition
    /// per-connection flags (e.g. the XEP-0160 offline-flush CAS via
    /// [`ConnectionEntry::claim_offline_flush`]).
    pub fn entry(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        self.connections.get(jid).map(|entry| entry.clone())
    }

    /// Get the number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}
