use std::str::FromStr;
use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use waddle_sfu::{
    CallGeneration, CallId, Identity, ListedRoom, LiveKitAdmin, MediaCapabilities,
    ObservedCallSids, ParticipantSid, RoomOccupancy, RoomSid, SfuError, SfuService,
};
use waddle_xmpp::muc::room_actor::UpsertMujiPresence;
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};

use super::*;
use crate::db::Database;
use crate::server::routes::websocket::{
    get_room_actor,
    handlers::presence::handle_muc_join,
    tests::{
        create_test_server_owner_session, create_test_websocket_state_with_clustering,
        create_test_websocket_state_with_sfu, snapshot_room,
    },
};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};

async fn store(name: &str) -> CallTeardownOutboxStore {
    CallTeardownOutboxStore::new(Database::in_memory(name).await.unwrap())
        .await
        .unwrap()
}

/// A raw 1:1 intent with NO sid fences: only its producing node's
/// process-local registry can execute it safely, so it stays behind
/// the producer gate (unlike fenced rows, which survive producer
/// loss — #1612 review round 9).
fn unfenced_participant_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        call_id: CallId::new("alice@example.test:call-1").unwrap(),
        target: TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: None,
        },
        generation: None,
        room_sid: None,
        session: None,
    }
}

fn participant_intent() -> CallTeardownIntent {
    CallTeardownIntent {
        call_id: CallId::new("alice@example.test:call-1").unwrap(),
        target: TeardownTarget::Participant {
            identity: FullJid::from_str("alice@example.test/device").unwrap(),
            participant_sid: Some(ParticipantSid::new("PA_test").unwrap()),
        },
        generation: Some(CallGeneration::try_from(1).unwrap()),
        room_sid: Some(RoomSid::new("RM_test").unwrap()),
        session: None,
    }
}

#[derive(Default)]
struct RecordingAdmin {
    remove_calls: Mutex<Vec<(CallId, Identity)>>,
    remove_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    fail_remove: bool,
    occupancy: Mutex<RoomOccupancy>,
}

impl LiveKitAdmin for RecordingAdmin {
    fn list_rooms(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remove_participant<'a>(
        &'a self,
        room: &'a CallId,
        identity: &'a Identity,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.remove_calls
                .lock()
                .expect("recording lock")
                .push((room.clone(), identity.clone()));
            let remove_gate = self.remove_gate.lock().expect("recording lock").clone();
            if let Some(remove_gate) = remove_gate {
                let _permit = remove_gate
                    .acquire()
                    .await
                    .expect("remove gate remains open");
            }
            if self.fail_remove {
                Err(SfuError::InvalidCallId("simulated admin failure".into()))
            } else {
                Ok(())
            }
        })
    }

    fn delete_room<'a>(
        &'a self,
        _room: &'a CallId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }

    fn update_participant<'a>(
        &'a self,
        _room: &'a CallId,
        _identity: &'a Identity,
        _capabilities: MediaCapabilities,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SfuError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }

    fn room_occupancy<'a>(
        &'a self,
        _room: &'a CallId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(self.occupancy.lock().expect("recording lock").clone()) })
    }
}

fn fixture_config() -> waddle_sfu::SfuConfig {
    waddle_sfu::SfuConfig {
        api_key: waddle_sfu::ApiKey::new("APItestkey"),
        api_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test API secret"),
        webhook_secret: waddle_sfu::ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test webhook secret"),
        ws_url: waddle_sfu::WebsocketUrl::new("wss://livekit.test/".parse().expect("test URL"))
            .expect("test websocket URL"),
        turn_host: waddle_sfu::TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: waddle_sfu::TurnSharedSecret::from_text("turn-secret"),
        token_ttl: chrono::Duration::seconds(3_600),
        turn_ttl: chrono::Duration::seconds(3_600),
    }
}

async fn state_with_executor(
    sfu: Arc<waddle_sfu::LiveKitSfu>,
) -> Arc<crate::server::routes::websocket::WebSocketState> {
    let sfu_service: Arc<dyn SfuService> = sfu.clone();
    let mut state = create_test_websocket_state_with_sfu(sfu_service).await;
    Arc::get_mut(&mut state)
        .expect("test state has one strong reference")
        .deps
        .protocol
        .call_teardown_executor = Some(sfu.teardown_executor());
    state
}

mod drain_basics;
mod muji;
mod retention_fences;
mod session_fence;
mod store_queue;
