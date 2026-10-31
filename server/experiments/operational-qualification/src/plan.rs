use serde::{Deserialize, Serialize};

use crate::{ClientId, ContractError, Manifest, RoomId};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct MessageId {
    pub repetition: u16,
    pub sequence: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Warmup,
    Measurement,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PlannedMessage {
    pub id: MessageId,
    pub phase: Phase,
    pub due_at_ms: u64,
    pub deadline_ms: u64,
    pub room: RoomId,
    pub sender: ClientId,
    pub recipients: Vec<ClientId>,
    pub payload_bytes: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Totals {
    pub duration_ms: u64,
    pub offered_messages: u64,
    pub expected_pairs: u64,
    pub offered_payload_bytes: u64,
    pub recipient_payload_bytes: u64,
}

/// Constructed only after validation; callers cannot deserialize a forged plan.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Plan {
    manifest: Manifest,
    totals: Totals,
    messages: Vec<PlannedMessage>,
}

impl Plan {
    pub fn build(manifest: Manifest) -> Result<Self, ContractError> {
        manifest.validate()?;
        let active_ms = manifest.phases.warmup_ms + manifest.phases.measurement_ms;
        let repetition_ms = active_ms + manifest.phases.drain_ms;
        let duration_ms = repetition_ms * u64::from(manifest.repetitions)
            + manifest.cooldown_ms * u64::from(manifest.repetitions - 1);
        let per_repetition = active_ms / manifest.offer_interval_ms;
        let offered_messages = per_repetition * u64::from(manifest.repetitions);
        if duration_ms > 86_400_000 || offered_messages > 10_000 {
            return Err(ContractError::Manifest(
                "exceeds 1 day or 10000 offered messages",
            ));
        }
        let mut totals = Totals {
            duration_ms,
            offered_messages,
            expected_pairs: 0,
            offered_payload_bytes: offered_messages * u64::from(manifest.payload_bytes),
            recipient_payload_bytes: 0,
        };
        let mut messages = Vec::new();
        for repetition in 1..=manifest.repetitions {
            let start_ms = u64::from(repetition - 1) * (repetition_ms + manifest.cooldown_ms);
            for sequence in 0..per_repetition {
                // Room order and each room's sender order are part of the manifest.
                let room_count = manifest.rooms.len() as u64;
                let room = &manifest.rooms[(sequence % room_count) as usize];
                let sender =
                    room.senders[((sequence / room_count) % room.senders.len() as u64) as usize];
                let offset_ms = sequence * manifest.offer_interval_ms;
                let due_at_ms = start_ms + offset_ms;
                totals.expected_pairs += room.recipients.len() as u64;
                if totals.expected_pairs > 100_000 {
                    return Err(ContractError::Manifest(
                        "exceeds 100000 expected recipient-message pairs",
                    ));
                }
                messages.push(PlannedMessage {
                    id: MessageId {
                        repetition,
                        sequence: sequence as u32,
                    },
                    phase: if offset_ms < manifest.phases.warmup_ms {
                        Phase::Warmup
                    } else {
                        Phase::Measurement
                    },
                    due_at_ms,
                    deadline_ms: due_at_ms + manifest.delivery_timeout_ms,
                    room: room.id,
                    sender,
                    recipients: room.recipients.clone(),
                    payload_bytes: manifest.payload_bytes,
                });
            }
        }
        totals.recipient_payload_bytes = totals.expected_pairs * u64::from(manifest.payload_bytes);
        if totals.offered_payload_bytes > 64 * 1024 * 1024
            || totals.recipient_payload_bytes > 256 * 1024 * 1024
        {
            return Err(ContractError::Manifest(
                "exceeds 64 MiB offered or 256 MiB recipient payload",
            ));
        }
        Ok(Self {
            manifest,
            totals,
            messages,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn totals(&self) -> &Totals {
        &self.totals
    }

    pub fn messages(&self) -> &[PlannedMessage] {
        &self.messages
    }
}
