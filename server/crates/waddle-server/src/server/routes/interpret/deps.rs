use super::*;
use waddle_xmpp::protocol::TimerId;

/// Outcome of interpreting a batch of [`OutboundEvent`]s.
///
/// The WebSocket transport uses `frames` to decide what to write back to
/// the client. `close` signals the main loop should drop the connection.
/// `feedback` carries [`InboundEvent`] callback completions the state
/// machine must consume to resume parked dispatches (XEP-0359
/// rich-target lookup, link enrichment). The caller is responsible for
/// pumping `feedback` back through `state_machine.handle(...)` until
/// the resulting outcome's `feedback` drains — see #229 PR9 cutover.
#[derive(Debug, Default)]
pub struct InterpretOutcome {
    /// Serialized XML frames to write to the transport, in order.
    pub frames: Vec<String>,
    /// Set to true when the state machine asked us to close the transport.
    pub close: bool,
    /// Async-callback completions to feed back to the state machine.
    pub feedback: Vec<InboundEvent>,
    /// Number of [`OutboundEvent::SendKeepaliveProbe`] effects in the
    /// batch (RFC 7395 §3.8 / issue #1090). Probes are transport
    /// control frames with no XML form, so they ride the outcome as a
    /// count and the adapter maps each to its native frame (WS
    /// `Ping`). In practice this is 0 or 1 per tick.
    pub keepalive_probes: u32,
    /// Timer effects ([`OutboundEvent::SetTimer`] /
    /// [`OutboundEvent::CancelTimer`]) for the adapter's
    /// connection-local timer wheel. Only the transport adapter owns a
    /// clock; the interpreter just relays the typed commands.
    pub timer_commands: Vec<TimerCommand>,
}

/// Typed timer instruction relayed from the state machine to the
/// transport adapter's timer wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerCommand {
    /// Arm (or re-arm, replacing any existing deadline for `id`) a
    /// one-shot timer that feeds `InboundEvent::Tick(id)` back into
    /// the state machine after `duration_ms`.
    Set { id: TimerId, duration_ms: u64 },
    /// Disarm the timer for `id`; a no-op when none is pending.
    Cancel(TimerId),
}

/// Typed dependency context for the interpreter.
///
/// Grows as later migration steps add storage/actor handles
/// (`extension_manager`, etc.). Threading dependencies through one
/// struct rather than as loose function parameters keeps the call-site
/// churn small.
#[derive(Clone)]
pub struct Deps<'a> {
    pub connection_registry: &'a ConnectionRegistry,
    /// Actor-backed per-user registry (ADR-0017 Phase 1). Threaded so the
    /// [`OutboundEvent::UnregisterConnection`] arm can mirror the DashMap
    /// unregister into the actor tree. `None` in unit tests; supplied in
    /// production via [`super::super::websocket::build_interpret_deps`].
    /// Nothing reads it for delivery yet.
    pub user_registry: Option<&'a ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    /// XEP-0198 stream-management session registry. Used to fan
    /// XEP-0280 carbons out to *detached but resumable* resources so
    /// briefly-disconnected secondary devices don't lose carbon
    /// history while resumable. `None` in unit tests that don't
    /// exercise SM behaviour.
    pub sm_session_registry: Option<&'a Arc<InMemorySmSessionRegistry>>,
    /// XEP-0313 MAM persistence backend. `None` in unit tests that
    /// don't exercise archive writes; production wiring (`iq.rs`)
    /// always supplies it.
    pub mam_storage: Option<&'a Arc<dyn MamStorage>>,
    /// Waddle inbox-projection backend. `None` in unit tests; always
    /// supplied in production.
    pub inbox_storage: Option<&'a Arc<dyn InboxStorage>>,
    /// Wasm-extension manager for XEP-0372/etc. link enrichment.
    /// `None` in unit tests that don't exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    pub extension_manager: Option<&'a Arc<ExtensionManager>>,
    /// MUC room-registry actor. Used by the
    /// [`OutboundEvent::DispatchToRoom`] arm to look up the per-room
    /// actor and ask it for a frozen
    /// [`waddle_xmpp::muc::room_actor::RoomChainSnapshot`].
    pub room_registry: Option<&'a ActorRef<RoomRegistryActor>>,
    /// WebSocket route state. Used by the
    /// [`OutboundEvent::DispatchToRoom`] arm for the managed-room
    /// owner check (announcements room) and by
    /// [`OutboundEvent::ProjectGroupchatInbox`] for inbox upserts +
    /// XEP-0430 push fan-out. `None` in unit tests; always supplied
    /// in production via
    /// [`super::super::websocket::build_interpret_deps`].
    pub web_socket_state: Option<&'a WebSocketState>,
    /// The authenticated `Session` of the connection that emitted the
    /// outbound events being interpreted, when one is available.
    ///
    /// Threaded through so the
    /// [`OutboundEvent::DispatchToRoom`] arm can perform the
    /// managed-room owner check (announcements room admits server
    /// owners only) without re-querying. `None` for unauthenticated
    /// flows (early connection lifecycle, unit tests).
    pub authenticated_session: Option<&'a Session>,
    /// Authoritative local XMPP domain. Used by the
    /// [`OutboundEvent::RouteToConnection`] arm to gate the
    /// headless offline-recipient pass (#229 PR15) to local-domain
    /// bare JIDs only — cross-domain bare JIDs (future s2s) drop
    /// without the recipient pass.
    pub local_domain: &'a str,
    /// XEP-0191 blocklist persistence backend. Used by the headless
    /// recipient-pass runner to seed a transient
    /// [`XmppStateMachine`]'s [`Blocklist`] for an offline local
    /// recipient. `None` in unit tests that don't exercise the
    /// offline-pass; production wiring always supplies it.
    pub blocking_storage: Option<&'a Arc<dyn BlockingStorage>>,
    /// Per-process [`StanzaDispatcher`] handle, cloned cheaply into
    /// each transient [`XmppStateMachine`] the headless
    /// recipient-pass runner constructs. `None` in unit tests that
    /// don't exercise the offline-pass.
    pub message_dispatcher: Option<&'a Arc<StanzaDispatcher>>,
    /// XEP-0160 offline-message storage backend (issue #209). Used by
    /// the [`OutboundEvent::QueueOfflineDelivery`] interpret arm to
    /// persist offline DM stanzas during the headless recipient pass.
    /// `None` in unit tests that don't exercise the offline-pass.
    pub pending_delivery_storage:
        Option<&'a Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>>,
}

impl<'a> Deps<'a> {
    /// Build a minimal `Deps` with only the connection registry — a
    /// test-only convenience for unit tests that don't exercise SM
    /// fan-out, archive, or inbox storage. Defaults `local_domain` to
    /// `"example.com"` to match the test fixtures used throughout
    /// this module's tests.
    #[cfg(test)]
    pub fn registry_only(connection_registry: &'a ConnectionRegistry) -> Self {
        Self {
            connection_registry,
            user_registry: None,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
            pending_delivery_storage: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the storage arms
    /// (`ArchiveDirect`, `ProjectInbox`). SM fan-out is left disabled
    /// so the carbon-detached path stays an independent test concern.
    #[cfg(test)]
    pub fn test_with_storage(
        connection_registry: &'a ConnectionRegistry,
        mam_storage: &'a Arc<dyn MamStorage>,
        inbox_storage: &'a Arc<dyn InboxStorage>,
    ) -> Self {
        Self {
            connection_registry,
            user_registry: None,
            sm_session_registry: None,
            mam_storage: Some(mam_storage),
            inbox_storage: Some(inbox_storage),
            extension_manager: None,
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
            pending_delivery_storage: None,
        }
    }

    /// Build a `Deps` for unit tests that exercise the
    /// [`OutboundEvent::RequestEnrichment`] arm.
    #[cfg(test)]
    pub fn test_with_extension_manager(
        connection_registry: &'a ConnectionRegistry,
        extension_manager: &'a Arc<ExtensionManager>,
    ) -> Self {
        Self {
            connection_registry,
            user_registry: None,
            sm_session_registry: None,
            mam_storage: None,
            inbox_storage: None,
            extension_manager: Some(extension_manager),
            room_registry: None,
            web_socket_state: None,
            authenticated_session: None,
            local_domain: "example.com",
            blocking_storage: None,
            message_dispatcher: None,
            pending_delivery_storage: None,
        }
    }
}
