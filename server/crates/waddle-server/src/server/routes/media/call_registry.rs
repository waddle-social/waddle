use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use waddle_xmpp::prometheus;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveCallState {
    Active,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallParticipant {
    pub participant_id: String,
    pub backend_session_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveCall {
    pub call_id: String,
    pub room_id: String,
    pub backend_room_id: String,
    pub backend: String,
    pub state: ActiveCallState,
    pub participant_count: usize,
    pub participants: Vec<CallParticipant>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertParticipant {
    pub room_id: String,
    pub preferred_call_id: Option<String>,
    pub participant_id: String,
    pub backend_session_id: String,
    pub role: String,
    pub backend: String,
    pub backend_room_id: String,
}

#[derive(Debug)]
enum RegistryCommand {
    UpsertParticipant {
        payload: UpsertParticipant,
        respond_to: oneshot::Sender<ActiveCall>,
    },
    ListByRoom {
        room_id: String,
        respond_to: oneshot::Sender<Vec<ActiveCall>>,
    },
    GetByCallId {
        call_id: String,
        respond_to: oneshot::Sender<Option<ActiveCall>>,
    },
    RemoveParticipant {
        call_id: String,
        participant_id: String,
        respond_to: oneshot::Sender<Option<RemoveParticipantResult>>,
    },
}

#[derive(Debug, Clone)]
pub struct RemoveParticipantResult {
    pub removed: bool,
    pub call: Option<ActiveCall>,
}

#[derive(Debug)]
struct ActiveCallRecord {
    call_id: String,
    room_id: String,
    backend_room_id: String,
    backend: String,
    state: ActiveCallState,
    participants: HashMap<String, CallParticipant>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ActiveCallRecord {
    fn snapshot(&self) -> ActiveCall {
        let mut participants = self.participants.values().cloned().collect::<Vec<_>>();
        participants.sort_by(|a, b| a.participant_id.cmp(&b.participant_id));

        ActiveCall {
            call_id: self.call_id.clone(),
            room_id: self.room_id.clone(),
            backend_room_id: self.backend_room_id.clone(),
            backend: self.backend.clone(),
            state: self.state.clone(),
            participant_count: participants.len(),
            participants,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Clone)]
pub struct ActiveCallRegistry {
    sender: mpsc::Sender<RegistryCommand>,
}

impl ActiveCallRegistry {
    pub fn new() -> Self {
        let (sender, mut receiver) = mpsc::channel::<RegistryCommand>(128);

        tokio::spawn(async move {
            let mut calls_by_id: HashMap<String, ActiveCallRecord> = HashMap::new();
            let mut active_call_by_room: HashMap<String, String> = HashMap::new();

            while let Some(command) = receiver.recv().await {
                match command {
                    RegistryCommand::UpsertParticipant {
                        payload,
                        respond_to,
                    } => {
                        let now = Utc::now();
                        let call_id = resolve_call_id(&payload, &calls_by_id, &active_call_by_room);
                        let is_new_call = !calls_by_id.contains_key(&call_id);

                        let record = calls_by_id.entry(call_id.clone()).or_insert_with(|| {
                            ActiveCallRecord {
                                call_id: call_id.clone(),
                                room_id: payload.room_id.clone(),
                                backend_room_id: payload.backend_room_id.clone(),
                                backend: payload.backend.clone(),
                                state: ActiveCallState::Active,
                                participants: HashMap::new(),
                                created_at: now,
                                updated_at: now,
                            }
                        });

                        record.room_id = payload.room_id.clone();
                        record.backend_room_id = payload.backend_room_id.clone();
                        record.backend = payload.backend.clone();
                        record.state = ActiveCallState::Active;

                        let mut is_new_participant = false;
                        if let Some(existing) = record.participants.get_mut(&payload.participant_id)
                        {
                            existing.backend_session_id = payload.backend_session_id.clone();
                            existing.role = payload.role.clone();
                            existing.updated_at = now;
                        } else {
                            is_new_participant = true;
                            record.participants.insert(
                                payload.participant_id.clone(),
                                CallParticipant {
                                    participant_id: payload.participant_id.clone(),
                                    backend_session_id: payload.backend_session_id,
                                    role: payload.role,
                                    joined_at: now,
                                    updated_at: now,
                                },
                            );
                        }

                        record.updated_at = now;
                        active_call_by_room.insert(payload.room_id, call_id);
                        if is_new_call {
                            prometheus::record_call_started();
                        }
                        if is_new_participant {
                            prometheus::record_call_joined();
                        }
                        let snapshot = record.snapshot();
                        let _ = record;
                        let active_call_count = calls_by_id.len() as u64;
                        prometheus::set_active_calls(active_call_count);
                        let _ = respond_to.send(snapshot);
                    }
                    RegistryCommand::ListByRoom {
                        room_id,
                        respond_to,
                    } => {
                        let mut calls = calls_by_id
                            .values()
                            .filter(|call| call.room_id == room_id)
                            .map(ActiveCallRecord::snapshot)
                            .collect::<Vec<_>>();
                        calls.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                        let _ = respond_to.send(calls);
                    }
                    RegistryCommand::GetByCallId {
                        call_id,
                        respond_to,
                    } => {
                        let _ = respond_to
                            .send(calls_by_id.get(&call_id).map(ActiveCallRecord::snapshot));
                    }
                    RegistryCommand::RemoveParticipant {
                        call_id,
                        participant_id,
                        respond_to,
                    } => {
                        let mut removed = false;
                        let mut call = None;
                        let mut emptied_room_id = None;

                        if let Some(record) = calls_by_id.get_mut(&call_id) {
                            removed = record.participants.remove(&participant_id).is_some();
                            record.updated_at = Utc::now();

                            if record.participants.is_empty() {
                                emptied_room_id = Some(record.room_id.clone());
                            } else {
                                call = Some(record.snapshot());
                            }
                        }

                        if let Some(room_id) = emptied_room_id {
                            calls_by_id.remove(&call_id);

                            if active_call_by_room.get(&room_id) == Some(&call_id) {
                                let next_call_id = calls_by_id
                                    .values()
                                    .filter(|candidate| candidate.room_id == room_id)
                                    .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                                    .map(|candidate| candidate.call_id.clone());

                                if let Some(next_call_id) = next_call_id {
                                    active_call_by_room.insert(room_id, next_call_id);
                                } else {
                                    active_call_by_room.remove(&room_id);
                                }
                            }
                        }

                        if removed {
                            prometheus::record_call_left();
                            prometheus::set_active_calls(calls_by_id.len() as u64);
                        }

                        let _ = respond_to.send(Some(RemoveParticipantResult { removed, call }));
                    }
                }
            }
        });

        Self { sender }
    }

    pub async fn upsert_participant(&self, payload: UpsertParticipant) -> Option<ActiveCall> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RegistryCommand::UpsertParticipant {
                payload,
                respond_to: tx,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn list_calls_by_room(&self, room_id: &str) -> Option<Vec<ActiveCall>> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RegistryCommand::ListByRoom {
                room_id: room_id.to_string(),
                respond_to: tx,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn get_call(&self, call_id: &str) -> Option<Option<ActiveCall>> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RegistryCommand::GetByCallId {
                call_id: call_id.to_string(),
                respond_to: tx,
            })
            .await
            .ok()?;
        rx.await.ok()
    }

    pub async fn remove_participant(
        &self,
        call_id: &str,
        participant_id: &str,
    ) -> Option<RemoveParticipantResult> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RegistryCommand::RemoveParticipant {
                call_id: call_id.to_string(),
                participant_id: participant_id.to_string(),
                respond_to: tx,
            })
            .await
            .ok()?;
        rx.await.ok()?
    }
}

impl Default for ActiveCallRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_call_id(
    payload: &UpsertParticipant,
    calls_by_id: &HashMap<String, ActiveCallRecord>,
    active_call_by_room: &HashMap<String, String>,
) -> String {
    if let Some(preferred_call_id) = &payload.preferred_call_id {
        if calls_by_id.contains_key(preferred_call_id) {
            return preferred_call_id.clone();
        }
    }

    if let Some(call_id) = active_call_by_room.get(&payload.room_id) {
        if calls_by_id.contains_key(call_id) {
            return call_id.clone();
        }
    }

    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upsert_reuses_active_call_for_room() {
        let registry = ActiveCallRegistry::new();

        let call_a = registry
            .upsert_participant(UpsertParticipant {
                room_id: "room-a".to_string(),
                preferred_call_id: None,
                participant_id: "user-1".to_string(),
                backend_session_id: "session-1".to_string(),
                role: "publisher".to_string(),
                backend: "webrtc-rs-sfu".to_string(),
                backend_room_id: "waddle-room-a".to_string(),
            })
            .await
            .unwrap();

        let call_b = registry
            .upsert_participant(UpsertParticipant {
                room_id: "room-a".to_string(),
                preferred_call_id: None,
                participant_id: "user-2".to_string(),
                backend_session_id: "session-2".to_string(),
                role: "subscriber".to_string(),
                backend: "webrtc-rs-sfu".to_string(),
                backend_room_id: "waddle-room-a".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(call_a.call_id, call_b.call_id);
        assert_eq!(call_b.participant_count, 2);
    }

    #[tokio::test]
    async fn remove_participant_ends_call_when_last_participant_leaves() {
        let registry = ActiveCallRegistry::new();

        let call = registry
            .upsert_participant(UpsertParticipant {
                room_id: "room-a".to_string(),
                preferred_call_id: None,
                participant_id: "user-1".to_string(),
                backend_session_id: "session-1".to_string(),
                role: "publisher".to_string(),
                backend: "webrtc-rs-sfu".to_string(),
                backend_room_id: "waddle-room-a".to_string(),
            })
            .await
            .unwrap();

        let removed = registry
            .remove_participant(&call.call_id, "user-1")
            .await
            .unwrap();
        assert!(removed.removed);
        assert!(removed.call.is_none());

        let call_after = registry.get_call(&call.call_id).await.unwrap();
        assert!(call_after.is_none());
    }

    #[tokio::test]
    async fn remove_participant_is_idempotent_for_missing_participant_or_call() {
        let registry = ActiveCallRegistry::new();

        let call = registry
            .upsert_participant(UpsertParticipant {
                room_id: "room-a".to_string(),
                preferred_call_id: None,
                participant_id: "user-1".to_string(),
                backend_session_id: "session-1".to_string(),
                role: "publisher".to_string(),
                backend: "webrtc-rs-sfu".to_string(),
                backend_room_id: "waddle-room-a".to_string(),
            })
            .await
            .unwrap();

        let missing_participant = registry
            .remove_participant(&call.call_id, "user-2")
            .await
            .unwrap();
        assert!(!missing_participant.removed);
        assert!(missing_participant.call.is_some());

        let missing_call = registry
            .remove_participant("does-not-exist", "user-1")
            .await
            .unwrap();
        assert!(!missing_call.removed);
        assert!(missing_call.call.is_none());
    }
}
