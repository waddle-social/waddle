use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ContractError;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ClientId(pub u16);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct RoomId(pub u16);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Room {
    pub id: RoomId,
    pub senders: Vec<ClientId>,
    pub recipients: Vec<ClientId>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Phases {
    pub warmup_ms: u64,
    pub measurement_ms: u64,
    pub drain_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    Disabled,
}

/// Numeric identities are synthetic; this format accepts no URLs or credentials.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u16,
    pub run_id: u32,
    pub client_count: u16,
    pub rooms: Vec<Room>,
    pub phases: Phases,
    pub offer_interval_ms: u64,
    pub payload_bytes: u32,
    pub delivery_timeout_ms: u64,
    pub max_generator_lag_ms: u64,
    pub retries: RetryPolicy,
    pub repetitions: u16,
    pub cooldown_ms: u64,
}

impl Manifest {
    pub(crate) fn validate(&self) -> Result<(), ContractError> {
        let invalid = ContractError::Manifest;
        if self.schema_version != 1 || self.run_id == 0 {
            return Err(invalid(
                "schema_version must be 1 and run_id must be positive",
            ));
        }
        if !(1..=1_000).contains(&self.client_count) || !(1..=100).contains(&self.rooms.len()) {
            return Err(invalid("requires 1..=1000 clients and 1..=100 rooms"));
        }
        if !(1..=10).contains(&self.repetitions) || self.cooldown_ms > 3_600_000 {
            return Err(invalid(
                "requires 1..=10 repetitions and cooldown <= 1 hour",
            ));
        }
        if !(1..=65_536).contains(&self.payload_bytes) {
            return Err(invalid("payload_bytes must be 1..=65536"));
        }
        let phases = &self.phases;
        if self.offer_interval_ms == 0
            || phases.measurement_ms == 0
            || phases.warmup_ms > 3_600_000
            || phases.measurement_ms > 3_600_000
            || phases.drain_ms > 3_600_000
        {
            return Err(invalid(
                "requires positive interval/measurement and phases <= 1 hour",
            ));
        }
        if !phases.warmup_ms.is_multiple_of(self.offer_interval_ms)
            || !phases.measurement_ms.is_multiple_of(self.offer_interval_ms)
        {
            return Err(invalid(
                "warmup and measurement must be whole offer intervals",
            ));
        }
        if self.delivery_timeout_ms == 0
            || self.delivery_timeout_ms > phases.drain_ms
            || self.max_generator_lag_ms > self.delivery_timeout_ms
        {
            return Err(invalid(
                "requires 0 < timeout <= drain and generator lag budget <= timeout",
            ));
        }
        let mut rooms = BTreeSet::new();
        for room in &self.rooms {
            if room.id.0 == 0 || !rooms.insert(room.id) {
                return Err(invalid("room IDs must be positive and unique"));
            }
            for clients in [&room.senders, &room.recipients] {
                let unique: BTreeSet<_> = clients.iter().collect();
                if clients.is_empty()
                    || unique.len() != clients.len()
                    || clients
                        .iter()
                        .any(|id| id.0 == 0 || id.0 > self.client_count)
                {
                    return Err(invalid(
                        "room client lists must be nonempty, unique and in range",
                    ));
                }
            }
        }
        Ok(())
    }
}
