pub(crate) use super::test_state::{
    create_test_session, create_test_websocket_state_with_extension_manager,
    empty_extension_manager, register_test_connection, seed_local_account, TestStateOverrides,
};
use super::*;
use super::{
    frame::handle_xmpp_frame,
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    session_init::build_internal_server_error_stream_error,
    state::WsConnState,
    transport_xml::{
        build_stream_features_xml, sasl_failure_xml, sasl_success_xml, websocket_stream_close_xml,
    },
};
use crate::db::{DatabaseConfig, DatabasePool, PoolConfig};
use crate::permissions::{Object, ObjectType, Permission, Relation, Subject, Tuple, WriteTuple};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2_hmac;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::mpsc;
// Handler functions moved to sub-modules but called directly in tests
use handlers::iq::{
    handle_iq, handle_iq_with_conn_state, managed_channel_permission_allowed, IqConnState,
};
use handlers::presence::{
    handle_muc_join as handle_muc_join_real, handle_muc_leave as handle_muc_leave_real,
    parse_room_jid_context,
};
// Types moved out of mod.rs scope but used in tests
use waddle_extensions::ExtensionConfig;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::muc::room_actor::{
    ApplyAdminItems, ChangeAffiliation, GetConfig, GetSnapshot, JoinAffiliationGrant,
    JoinWithAffiliation, SetSubject, UpdateConfig,
};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Affiliation;
use xmpp_parsers::iq::{Iq, IqPayload};
use xmpp_parsers::message::MessageType as XmppMessageType;

mod batch_write;
mod broadcast;
mod custody_ack;
mod disco_trace;
mod dispatch;
mod frame_parsing;
mod ingress_authority;
mod iq;
mod keyed_detach_drain;
mod messages;
mod misc;
mod muc;
mod registration;
mod send;
mod stream_features;
mod xep0045_reconnect_contract;

/// One stable `OccupancySessionGeneration` per simulated connection, keyed by
/// the test's `WebSocketState` and full JID, so test joins, presence updates
/// and leaves that go through the connection-less test wrappers present the
/// SAME generation the join recorded (like a real `WsConnState`) while
/// parallel tests sharing a JID never see each other's entries.
type TestOccupancySessionKey = (usize, String);

pub(crate) fn test_occupancy_sessions() -> &'static std::sync::Mutex<
    std::collections::HashMap<
        TestOccupancySessionKey,
        waddle_xmpp_core::OccupancySessionGeneration,
    >,
> {
    static SESSIONS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<
                TestOccupancySessionKey,
                waddle_xmpp_core::OccupancySessionGeneration,
            >,
        >,
    > = std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn test_occupancy_session_key(
    state: &WebSocketState,
    jid: &jid::FullJid,
) -> TestOccupancySessionKey {
    (std::ptr::from_ref(state) as usize, jid.to_string())
}

/// Mint and record a fresh generation for `jid` (a new simulated connection).
pub(crate) fn record_test_occupancy_session(
    state: &WebSocketState,
    jid: &jid::FullJid,
) -> waddle_xmpp_core::OccupancySessionGeneration {
    let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
    test_occupancy_sessions()
        .lock()
        .expect("test occupancy session lock")
        .insert(test_occupancy_session_key(state, jid), generation);
    generation
}

/// The generation recorded for `jid`, minting one on first use. The guard is
/// dropped before minting: `record_test_occupancy_session` takes the same
/// lock and a guard kept alive across the closure deadlocks.
pub(crate) fn current_test_occupancy_session(
    state: &WebSocketState,
    jid: &jid::FullJid,
) -> waddle_xmpp_core::OccupancySessionGeneration {
    let recorded = test_occupancy_sessions()
        .lock()
        .expect("test occupancy session lock")
        .get(&test_occupancy_session_key(state, jid))
        .copied();
    recorded.unwrap_or_else(|| record_test_occupancy_session(state, jid))
}
mod stream_management;

async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &jid::BareJid,
    sender_jid: &jid::FullJid,
    nick: &str,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    authenticated_session: &Option<crate::auth::Session>,
) -> Vec<String> {
    handle_muc_join_with_occupancy_session(
        state,
        domain,
        room_jid,
        sender_jid,
        nick,
        presence_show,
        (
            record_test_occupancy_session(state, sender_jid),
            authenticated_session,
        ),
    )
    .await
}

async fn handle_muc_join_with_occupancy_session(
    state: &WebSocketState,
    domain: &str,
    room_jid: &jid::BareJid,
    sender_jid: &jid::FullJid,
    nick: &str,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    connection: (
        waddle_xmpp_core::OccupancySessionGeneration,
        &Option<crate::auth::Session>,
    ),
) -> Vec<String> {
    let (occupancy_session, authenticated_session) = connection;
    handle_muc_join_real(
        state,
        domain,
        room_jid,
        sender_jid,
        nick,
        presence_show,
        handlers::presence::MucJoinConnectionContext {
            registry_owner: None,
            occupancy_session,
            authenticated_session,
        },
    )
    .await
}

async fn handle_muc_leave_with_occupancy_session(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    sender_jid: &jid::FullJid,
    nick: &str,
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> Vec<String> {
    handle_muc_leave_real(
        state,
        room_jid,
        sender_jid,
        nick,
        occupancy_session,
        ordered_relay_origin,
    )
    .await
}

/// Seed an OIDC-provisioned local account directly into the `users`
/// table, the way the OIDC login flow does. Needed since #1246: a
/// message routed to a local bare JID with no registered account is
/// bounced with `<service-unavailable/>` (RFC 6121 §8.5.1) instead of
/// being persisted, so tests that message an offline recipient must
/// give that recipient an account first.
pub(crate) async fn create_test_websocket_state() -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await
}

/// Like [`create_test_websocket_state`], but the protocol room registry IS the
/// app-state registry (production wiring), so admin commands and janitor
/// sweeps observe the same room actors.
pub(crate) async fn create_test_websocket_state_sharing_app_room_registry() -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            share_app_room_registry: true,
            ..Default::default()
        },
    )
    .await
}

pub(crate) async fn create_test_websocket_state_with_db_pool(
    db_pool: Arc<DatabasePool>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Share projection storage with a caller-owned ingress transaction fixture.
pub(crate) async fn create_test_websocket_state_with_db_pool_and_ingress(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<crate::ingress::IngressAuthority>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            ingress: Some(ingress),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Exercise ingress invitation settlement against the same database as its pending queue.
pub(crate) async fn create_test_websocket_state_with_durable_ingress(
    fixture: &crate::ingress::test_support::IngressFixture,
) -> Arc<WebSocketState> {
    let db_pool = Arc::new(
        DatabasePool::new(
            DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            PoolConfig,
        )
        .await
        .expect("shared ingress pool"),
    );
    let pending = crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        fixture.db.clone(),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("canonical pending storage");
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            ingress: Some(Arc::new(fixture.authority().await)),
            pending_delivery_storage: Some(Arc::new(pending)),
            ..TestStateOverrides::default()
        },
    )
    .await
}

pub(crate) async fn create_test_websocket_state_with_sm_registry(
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(sm_session_registry),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Build a test state with explicit SM registry and pending-delivery storage.
/// This keeps recovery tests on the production cleanup path while allowing a
/// single promotion attempt to model a transient storage outage.
pub(crate) async fn create_test_websocket_state_with_sm_registry_and_pending_storage(
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
    pending_delivery_storage: Arc<
        dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    >,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(sm_session_registry),
            pending_delivery_storage: Some(pending_delivery_storage),
            ..TestStateOverrides::default()
        },
    )
    .await
}

pub(crate) async fn create_test_websocket_state_with_sm_registry_pending_and_blocking(
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
    pending_delivery_storage: Arc<
        dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    >,
    blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(sm_session_registry),
            blocking_storage: Some(blocking_storage),
            pending_delivery_storage: Some(pending_delivery_storage),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Build a test [`WebSocketState`] with clustering-enabled
/// [`crate::clustering::ClusteringHandles`] and a caller-supplied SM-session
/// registry (e.g. one backed by [`crate::sm_persistence_fenced::PostgresFencedSmPersistence`]
/// pointed at the same Postgres database as the clustering claims tables) —
/// used by `session_janitors.rs`'s orphan-reaper Postgres-gated end-to-end
/// test, the only fixture that needs `state.deps.app_state.clustering_claims`
/// populated and `state.deps.protocol.sm_session_registry` backed by a real
/// durable, claim-fenced store rather than every other fixture in this
/// module's plain in-memory default.
pub(crate) async fn create_test_websocket_state_with_clustering(
    clustering: crate::clustering::ClusteringHandles,
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            clustering: Some(clustering),
            sm_session_registry: Some(sm_session_registry),
            ..TestStateOverrides::default()
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn create_test_websocket_state_with_sm_registry_and_ingress(
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
    ingress: Arc<crate::ingress::IngressAuthority>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            sm_session_registry: Some(sm_session_registry),
            ingress: Some(ingress),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Register a connection into BOTH the DashMap `ConnectionRegistry` and the
/// actor tree, sharing the `Arc`-backed `ConnectionEntry` exactly as the
/// production dual-registration path does (ADR-0017 Phase 1).
///
/// Tests that drive delivery or bare-JID selection through the actor cutover
/// MUST use this instead of a bare `connection_registry.register(...)`;
/// otherwise the actor tree is empty and the cutover paths resolve no target.
///
/// Returns the same owner token `connection_registry.register(...)` would —
/// callers exercising the owner-gated presence/SM writes (#1208) carry it on
/// their fixture's `registry_owner` exactly like real registration does.
/// Build a test [`WebSocketState`] with an arbitrary [`SfuService`]
/// plugged into the protocol services — used with [`RecordingSfu`] to
/// assert which SFU teardown surface a handler actually invoked.
pub(crate) async fn create_test_websocket_state_with_sfu(
    sfu: Arc<dyn waddle_sfu::SfuService>,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            call_sfu: Some(sfu),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// [`create_test_websocket_state_with_sfu`] plus caller-supplied
/// clustering handles — used by the #1594 webhook tests, which need
/// BOTH an observable SFU (the enforcement side effect) and a claim
/// store (the cross-node routing decision).
#[cfg(feature = "clustering")]
pub(crate) async fn create_test_websocket_state_with_sfu_and_clustering(
    sfu: Arc<dyn waddle_sfu::SfuService>,
    clustering: crate::clustering::ClusteringHandles,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            call_sfu: Some(sfu),
            clustering: Some(clustering),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// Recording fake: captures `(call_id, identity)` separately for each
/// teardown dispatch — `unregister_call_participant` (the admin-evict
/// path) into `calls`, `note_participant_left` (the webhook-bridge
/// local-only path) into `note_calls`. Splitting the vecs lets tests
/// assert which trait method was actually invoked, mirroring how
/// `waddle-sfu`'s `RecordingAdmin` splits `remove_calls` from
/// `delete_calls`. The other trait methods are unimplemented because
/// the production code paths under test only touch the teardown
/// surfaces.
pub(crate) struct RecordingSfu {
    /// Registrations keyed by (call, identity): `Some(generation)` for a
    /// connection-bound registration, `None` for an unbound one (the shape a
    /// webhook/probe restore leaves behind).
    occupant_sessions: std::sync::Mutex<
        std::collections::HashMap<
            (waddle_sfu::CallId, waddle_sfu::Identity),
            Option<waddle_xmpp_core::OccupancySessionGeneration>,
        >,
    >,
    registered_calls: std::sync::Mutex<
        Vec<(
            waddle_sfu::CallId,
            waddle_sfu::Identity,
            waddle_sfu::ObservedCallSids,
        )>,
    >,
    calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
    note_calls: std::sync::Mutex<
        Vec<(
            waddle_sfu::CallId,
            waddle_sfu::Identity,
            waddle_sfu::ObservedCallSids,
        )>,
    >,
    note_disposition: std::sync::Mutex<Option<waddle_sfu::TeardownDisposition>>,
    register_disposition: std::sync::Mutex<Option<waddle_sfu::SidObservationDisposition>>,
    observed_calls: std::sync::Mutex<
        Vec<(
            waddle_sfu::CallId,
            waddle_sfu::Identity,
            waddle_sfu::ObservedCallSids,
        )>,
    >,
    participants: std::sync::Mutex<Vec<waddle_sfu::Identity>>,
    update_calls: std::sync::Mutex<
        Vec<(
            waddle_sfu::CallId,
            waddle_sfu::Identity,
            waddle_sfu::MediaCapabilities,
        )>,
    >,
}

impl Default for RecordingSfu {
    fn default() -> Self {
        Self {
            occupant_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            registered_calls: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::Mutex::new(Vec::new()),
            note_calls: std::sync::Mutex::new(Vec::new()),
            note_disposition: std::sync::Mutex::new(None),
            register_disposition: std::sync::Mutex::new(None),
            observed_calls: std::sync::Mutex::new(Vec::new()),
            participants: std::sync::Mutex::new(Vec::new()),
            update_calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl RecordingSfu {
    pub(crate) fn registered_with_sids_snapshot(
        &self,
    ) -> Vec<(
        waddle_sfu::CallId,
        waddle_sfu::Identity,
        waddle_sfu::ObservedCallSids,
    )> {
        self.registered_calls
            .lock()
            .expect("recording lock")
            .clone()
    }

    pub(crate) fn snapshot(&self) -> Vec<(waddle_sfu::CallId, waddle_sfu::Identity)> {
        self.calls.lock().expect("recording lock").clone()
    }

    pub(crate) fn note_snapshot(&self) -> Vec<(waddle_sfu::CallId, waddle_sfu::Identity)> {
        self.note_calls
            .lock()
            .expect("recording lock")
            .iter()
            .map(|(call_id, identity, _)| (call_id.clone(), identity.clone()))
            .collect()
    }

    pub(crate) fn note_with_sids_snapshot(
        &self,
    ) -> Vec<(
        waddle_sfu::CallId,
        waddle_sfu::Identity,
        waddle_sfu::ObservedCallSids,
    )> {
        self.note_calls.lock().expect("recording lock").clone()
    }

    pub(crate) fn set_note_disposition(&self, disposition: waddle_sfu::TeardownDisposition) {
        *self.note_disposition.lock().expect("recording lock") = Some(disposition);
    }

    pub(crate) fn set_register_disposition(
        &self,
        disposition: waddle_sfu::SidObservationDisposition,
    ) {
        *self.register_disposition.lock().expect("recording lock") = Some(disposition);
    }

    pub(crate) fn observed_with_sids_snapshot(
        &self,
    ) -> Vec<(
        waddle_sfu::CallId,
        waddle_sfu::Identity,
        waddle_sfu::ObservedCallSids,
    )> {
        self.observed_calls.lock().expect("recording lock").clone()
    }

    pub(crate) fn set_participants(&self, participants: Vec<waddle_sfu::Identity>) {
        *self.participants.lock().expect("recording lock") = participants;
    }

    pub(crate) fn update_snapshot(
        &self,
    ) -> Vec<(
        waddle_sfu::CallId,
        waddle_sfu::Identity,
        waddle_sfu::MediaCapabilities,
    )> {
        self.update_calls.lock().expect("recording lock").clone()
    }
}

impl waddle_sfu::SfuService for RecordingSfu {
    fn issue_join_token(
        &self,
        _: &waddle_sfu::CallId,
        _: &waddle_sfu::Identity,
        _: waddle_sfu::MediaCapabilities,
    ) -> Result<waddle_sfu::JoinToken, waddle_sfu::SfuError> {
        unimplemented!("not exercised by these tests")
    }

    fn issue_turn_credentials(
        &self,
        _: &waddle_sfu::Identity,
    ) -> Result<waddle_sfu::TurnCredential, waddle_sfu::SfuError> {
        unimplemented!("not exercised by these tests")
    }

    fn register_call_participant(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
    ) {
        self.occupant_sessions
            .lock()
            .expect("recording lock")
            .insert((call_id.clone(), identity.clone()), None);
    }

    fn register_call_participant_with_session(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        _: &waddle_sfu::SessionBinding,
        occupant: waddle_xmpp_core::OccupancySessionGeneration,
    ) {
        self.occupant_sessions
            .lock()
            .expect("recording lock")
            .insert((call_id.clone(), identity.clone()), Some(occupant));
    }

    fn register_call_participant_observed(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        observed_sids: &waddle_sfu::ObservedCallSids,
    ) -> waddle_sfu::SidObservationDisposition {
        if let Some(disposition) = *self.register_disposition.lock().expect("recording lock") {
            return disposition;
        }
        if matches!(
            *self.note_disposition.lock().expect("recording lock"),
            Some(waddle_sfu::TeardownDisposition::StaleSid)
        ) {
            return waddle_sfu::SidObservationDisposition::StaleSid;
        }
        self.registered_calls.lock().expect("recording lock").push((
            call_id.clone(),
            identity.clone(),
            observed_sids.clone(),
        ));
        waddle_sfu::SidObservationDisposition::Applied
    }

    fn has_call_participant(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
    ) -> bool {
        self.occupant_sessions
            .lock()
            .expect("recording lock")
            .contains_key(&(call_id.clone(), identity.clone()))
    }

    fn participant_occupant_session(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
    ) -> Option<waddle_xmpp_core::OccupancySessionGeneration> {
        self.occupant_sessions
            .lock()
            .expect("recording lock")
            .get(&(call_id.clone(), identity.clone()))
            .copied()
            .flatten()
    }

    fn unregister_call_participant_if_occupant_matches(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        presented: waddle_xmpp_core::OccupancySessionGeneration,
        unbound: waddle_sfu::UnboundOccupantPolicy,
        _: waddle_sfu::SidEvidence<'_>,
        _: Option<&waddle_sfu::ObservedCallSids>,
    ) -> waddle_sfu::SessionScopedTeardown {
        let mut sessions = self.occupant_sessions.lock().expect("recording lock");
        let key = (call_id.clone(), identity.clone());
        match sessions.get(&key).copied().flatten() {
            Some(bound) if bound == presented => {}
            None if unbound == waddle_sfu::UnboundOccupantPolicy::TearDown => {}
            _ => return waddle_sfu::SessionScopedTeardown::SessionMismatch,
        }
        sessions.remove(&key);
        drop(sessions);
        self.calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
        waddle_sfu::SessionScopedTeardown::Applied(waddle_sfu::TeardownDisposition::Applied(
            waddle_sfu::CallState::Ended,
        ))
    }

    fn revoke_issued_token(
        &self,
        _: &waddle_sfu::CallId,
        _: &waddle_sfu::Identity,
        _: &waddle_sfu::Jti,
    ) {
        unimplemented!("not exercised by these tests")
    }

    fn unregister_call_participant(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        _: Option<&waddle_sfu::ObservedCallSids>,
    ) -> waddle_sfu::TeardownDisposition {
        self.occupant_sessions
            .lock()
            .expect("recording lock")
            .remove(&(call_id.clone(), identity.clone()));
        self.calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
        waddle_sfu::TeardownDisposition::Applied(waddle_sfu::CallState::Ended)
    }

    fn note_participant_left(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        observed_sids: Option<&waddle_sfu::ObservedCallSids>,
    ) -> waddle_sfu::TeardownDisposition {
        // Recorded into `note_calls`, NOT `calls`: the two trait
        // methods imply different downstream effects (admin
        // RemoveParticipant vs. local-only bookkeeping) and tests
        // need to distinguish them.
        self.note_calls.lock().expect("recording lock").push((
            call_id.clone(),
            identity.clone(),
            observed_sids.cloned().unwrap_or_default(),
        ));
        self.note_disposition
            .lock()
            .expect("recording lock")
            .unwrap_or(waddle_sfu::TeardownDisposition::Applied(
                waddle_sfu::CallState::Ended,
            ))
    }

    fn note_participant_left_if_occupant_matches(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        observed_sids: Option<&waddle_sfu::ObservedCallSids>,
        presented: waddle_xmpp_core::OccupancySessionGeneration,
        unbound: waddle_sfu::UnboundOccupantPolicy,
        _: waddle_sfu::SidEvidence<'_>,
    ) -> waddle_sfu::SessionScopedTeardown {
        let stored = self
            .occupant_sessions
            .lock()
            .expect("recording lock")
            .get(&(call_id.clone(), identity.clone()))
            .copied()
            .flatten();
        match stored {
            Some(bound) if bound == presented => {}
            None if unbound == waddle_sfu::UnboundOccupantPolicy::TearDown => {}
            _ => return waddle_sfu::SessionScopedTeardown::SessionMismatch,
        }
        waddle_sfu::SessionScopedTeardown::Applied(self.note_participant_left(
            call_id,
            identity,
            observed_sids,
        ))
    }

    fn observe_call_participant_sids(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        observed_sids: Option<&waddle_sfu::ObservedCallSids>,
        _: waddle_sfu::SidObservationDirection,
    ) -> waddle_sfu::SidObservationDisposition {
        self.observed_calls.lock().expect("recording lock").push((
            call_id.clone(),
            identity.clone(),
            observed_sids.cloned().unwrap_or_default(),
        ));
        if matches!(
            *self.note_disposition.lock().expect("recording lock"),
            Some(waddle_sfu::TeardownDisposition::StaleSid)
        ) {
            waddle_sfu::SidObservationDisposition::StaleSid
        } else {
            waddle_sfu::SidObservationDisposition::Applied
        }
    }

    fn update_participant_capabilities(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        capabilities: waddle_sfu::MediaCapabilities,
    ) {
        self.update_calls.lock().expect("recording lock").push((
            call_id.clone(),
            identity.clone(),
            capabilities,
        ));
    }

    fn is_revoked(&self, _: &waddle_sfu::Jti) -> bool {
        false
    }

    fn ws_url(&self) -> &waddle_sfu::WebsocketUrl {
        unimplemented!("not exercised by these tests")
    }

    fn turn_host(&self) -> &waddle_sfu::TurnHost {
        unimplemented!("not exercised by these tests")
    }

    fn webhook_secret(&self) -> &waddle_sfu::ApiSecret {
        static SECRET: std::sync::OnceLock<waddle_sfu::ApiSecret> = std::sync::OnceLock::new();
        SECRET.get_or_init(|| {
            waddle_sfu::ApiSecret::from_text("recording-webhook-secret-32-bytes")
                .expect("recording webhook secret meets minimum length")
        })
    }

    fn participants_for_call(&self, _: &waddle_sfu::CallId) -> Vec<waddle_sfu::Identity> {
        self.participants.lock().expect("recording lock").clone()
    }
}

/// A self-contained [`waddle_sfu::LiveKitSfu`] for tests. Mints real
/// JWTs locally (no network), so the XEP-0166 Jingle handler can
/// rewrite the Waddle LiveKit transport exactly as it does in
/// production. Mirrors the fixture used by the `waddle-xmpp` Jingle
/// unit tests.
fn fixture_call_sfu() -> Arc<dyn waddle_sfu::SfuService> {
    let cfg = waddle_sfu::SfuConfig {
        api_key: waddle_sfu::ApiKey::new("APItestkey"),
        api_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test api secret meets min length"),
        webhook_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test webhook secret meets min length"),
        ws_url: waddle_sfu::WebsocketUrl::new("wss://livekit.test/".parse().expect("ws url"))
            .expect("ws url valid"),
        turn_host: waddle_sfu::TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: waddle_sfu::TurnSharedSecret::from_text("turn-secret"),
        token_ttl: chrono::Duration::seconds(3600),
        turn_ttl: chrono::Duration::seconds(3600),
    };
    Arc::new(waddle_sfu::LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
}

/// Build a test [`WebSocketState`] whose dispatcher has the XEP-0166
/// Jingle + XEP-0215 extdisco handlers registered — i.e. the
/// production wiring when `LIVEKIT_*` env is configured (see
/// `http.rs::register_call_handlers`). Required to exercise 1:1 DM
/// calling through the real IQ-handler path.
pub(crate) async fn create_test_websocket_state_with_calls() -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            call_sfu: Some(fixture_call_sfu()),
            ..TestStateOverrides::default()
        },
    )
    .await
}

/// The plugin id of the one real component [`create_test_websocket_state_with_fixture_bot`]
/// loads, so `FIXTURE_BOT_PLUGIN@extensions.example.com` is a bot with a manifest.
pub(crate) const FIXTURE_BOT_PLUGIN: &str = "message-hook-fixture";

/// A test state whose extension manager really loads [`FIXTURE_BOT_PLUGIN`].
pub(crate) async fn create_test_websocket_state_with_fixture_bot() -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(
        Arc::new(fixture_bot_extension_manager().await),
        TestStateOverrides::default(),
    )
    .await
}

/// [`create_test_websocket_state_with_fixture_bot`] whose manifest profile
/// declares `avatar`.
pub(crate) async fn create_test_websocket_state_with_fixture_bot_avatar(
    avatar: waddle_extensions::ArtifactReference,
) -> Arc<WebSocketState> {
    let plugin = waddle_extensions::PluginId::new(FIXTURE_BOT_PLUGIN).expect("plugin id");
    create_test_websocket_state_with_extension_manager(
        Arc::new(
            fixture_bot_extension_manager()
                .await
                .with_profile_avatar(&plugin, avatar),
        ),
        TestStateOverrides::default(),
    )
    .await
}

async fn fixture_bot_extension_manager() -> ExtensionManager {
    ExtensionManager::from_config(ExtensionConfig {
        enabled: true,
        modules: vec![waddle_extensions::ExtensionModuleConfig {
            room_observation: None,
            runtime_limits: Default::default(),
            name: FIXTURE_BOT_PLUGIN.into(),
            namespace: "urn:test:message-hook".into(),
            registry: Default::default(),
            digest: None,
            tag: None,
            config: serde_json::json!(0),
            capability_grants: vec![waddle_extensions::ExtensionCapability::MessageEnrich],
            allowed_http_origins: vec![],
            provider_room_grants: vec![],
            config_secret_files: Default::default(),
            local_path: Some(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../waddle-extensions/tests/fixtures/message_hook.wat")
                    .to_string_lossy()
                    .into_owned(),
            ),
        }],
        ..Default::default()
    })
    .await
    .expect("fixture bot extension manager")
}

/// Optional fixture substitutions for one test websocket state; unset fields
/// take the shared defaults.
pub(crate) async fn store_resumable_detached_session(
    state: &WebSocketState,
    session: &Session,
    detached: waddle_xmpp::stream_management::DetachedSession,
) {
    crate::occupancy_authority::publish(
        state.deps.app_state.db_pool.global(),
        &detached.jid,
        detached.occupancy_session,
    )
    .await
    .expect("seed authoritative generation for resumable test session");
    state
        .deps
        .protocol
        .ingress
        .enroll_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(
            detached.stream_id.clone(),
        ))
        .await
        .expect("enroll resumable test stream");
    let principal = session
        .authenticated_principal_ref()
        .expect("test session carries an auth context");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session_with_principal(detached, principal)
        .await
        .expect("store detached session with principal");
}

pub(crate) async fn create_test_server_owner_session(
    state: &WebSocketState,
    username: &str,
) -> Session {
    let session = create_test_session(state, username).await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("owner"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("server owner tuple");
    session
}

async fn register_test_native_user(state: &WebSocketState, username: &str, password: &str) {
    let native_user_store =
        NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
    native_user_store
        .register(crate::auth::native::RegisterRequest {
            username: username.to_string(),
            domain: state.deps.auth_state.xmpp_domain.clone(),
            password: password.to_string(),
            email: None,
        })
        .await
        .expect("native user");
}

fn scram_client_final_from_challenge(
    username: &str,
    password: &str,
    client_nonce: &str,
    challenge_b64: &str,
) -> String {
    type HmacSha256 = Hmac<Sha256>;

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn sha256(data: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    let challenge = String::from_utf8(
        BASE64_STANDARD
            .decode(challenge_b64)
            .expect("challenge base64"),
    )
    .expect("challenge utf8");
    let mut combined_nonce = None;
    let mut salt_b64 = None;
    let mut iterations = None;
    for attr in challenge.split(',') {
        if let Some(value) = attr.strip_prefix("r=") {
            combined_nonce = Some(value.to_string());
        } else if let Some(value) = attr.strip_prefix("s=") {
            salt_b64 = Some(value.to_string());
        } else if let Some(value) = attr.strip_prefix("i=") {
            iterations = Some(value.parse::<u32>().expect("iterations"));
        }
    }

    let combined_nonce = combined_nonce.expect("combined nonce");
    let salt = BASE64_STANDARD
        .decode(salt_b64.expect("salt"))
        .expect("salt base64");
    let iterations = iterations.expect("iterations");

    let mut salted_password = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut salted_password);
    let client_key = hmac_sha256(&salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let channel_binding = BASE64_STANDARD.encode("n,,");
    let client_final_without_proof = format!("c={channel_binding},r={combined_nonce}");
    let client_first_bare = format!(
        "n={},r={client_nonce}",
        waddle_xmpp::auth::encode_sasl_name(username)
    );
    let auth_message = format!("{client_first_bare},{challenge},{client_final_without_proof}");
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(left, right)| left ^ right)
        .collect();

    format!(
        "{client_final_without_proof},p={}",
        BASE64_STANDARD.encode(client_proof)
    )
}

pub(crate) async fn snapshot_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> waddle_xmpp::muc::room_actor::RoomSnapshot {
    let room_actor = get_room_actor(state, room_jid).await.expect("room actor");
    room_actor.ask(GetSnapshot).await.expect("room snapshot")
}

fn parse_message_for_test(xml: &str) -> xmpp_parsers::message::Message {
    match parse_frame(xml).expect("message parses") {
        InboundFrame::Stanza(stanza) => match *stanza {
            Stanza::Message(msg) => msg,
            _ => panic!("expected message stanza"),
        },
        _ => panic!("expected message stanza"),
    }
}

fn message_frame_xml_with_id(id: String) -> String {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id(id));
    stanza_to_xml(&Stanza::Message(message))
}

fn assert_sample_payload(xml: &str, element_name: &str, url: &str, owner: &str, name: &str) {
    let parsed = parse_message_for_test(xml);
    let payload = parsed
        .payloads
        .iter()
        .find(|payload| {
            payload.name() == element_name && payload.ns() == "urn:waddle:test-extension:1"
        })
        .unwrap_or_else(|| panic!("missing {element_name} sample payload"));
    assert_eq!(payload.attr("url"), Some(url));
    assert_eq!(payload.attr("owner"), Some(owner));
    assert_eq!(payload.attr("name"), Some(name));
}

fn parse_iq_for_test(xml: &str) -> xmpp_parsers::iq::Iq {
    match parse_frame(xml).expect("iq parses") {
        InboundFrame::Stanza(stanza) => match *stanza {
            Stanza::Iq(iq) => *iq,
            _ => panic!("expected iq stanza"),
        },
        _ => panic!("expected iq stanza"),
    }
}

fn disco_items_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
    let mut query =
        xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_ITEMS_NS);
    if let Some(node) = node {
        query = query.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    stanza_to_xml(&Stanza::Iq(Box::new(Iq::Get {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload: query.build(),
    })))
}

fn disco_info_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
    let mut query =
        xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_INFO_NS);
    if let Some(node) = node {
        query = query.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    stanza_to_xml(&Stanza::Iq(Box::new(Iq::Get {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload: query.build(),
    })))
}

fn iq_set_frame(id: &str, to: &str, payload: xmpp_parsers::minidom::Element) -> String {
    stanza_to_xml(&Stanza::Iq(Box::new(Iq::Set {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload,
    })))
}

fn ready_phase(jid: &FullJid) -> ConnectionPhase {
    ConnectionPhase::ready(jid.clone(), false)
}

/// Construct a per-connection [`XmppStateMachine`] seeded with the
/// shared test dispatcher (registered with the default message
/// handler chain) and drive the given message through the new
/// thin-adapter [`handle_message`]. Mirrors the production main
/// loop's bind-time wiring (#229 PR11/PR13) closely enough for the
/// unit-level tests in this module to assert end-to-end semantics
/// against the dispatcher path.
async fn handle_message_for_test(
    state: &WebSocketState,
    sender_jid: &FullJid,
    session: Option<&Session>,
    message: xmpp_parsers::message::Message,
) -> Vec<String> {
    let mut sm = XmppStateMachine::new(
        state.deps.auth_state.xmpp_domain.clone(),
        (*state.deps.protocol.dispatcher).clone(),
    );
    sm.transition_to_ready(sender_jid.clone(), false);
    sm.set_blocklist(Blocklist::empty());
    let phase = ConnectionPhase::ready(sender_jid.clone(), false);
    handlers::message::handle_message(message, state, &phase, Some(&mut sm), session, None, None)
        .await
}

fn authenticated_phase_for_session(session: &Session, domain: &str) -> ConnectionPhase {
    let pending_jid: FullJid = format!("{}@{domain}/pending", session.xmpp_localpart)
        .parse()
        .expect("pending jid");
    ConnectionPhase::authenticated(&pending_jid)
}

// ---- B: Non-blocking broadcast ------------------------------------

// ---- C: MUC nick handling -----------------------------------------

// ---- D: stream feature advertisement --------------------------------
