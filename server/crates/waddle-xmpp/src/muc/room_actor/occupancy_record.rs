//! The room actor's side of the room's durable occupancy record.
//!
//! The actor is the single writer of the room's occupants. It reports every
//! seat it grants or removes to the store's occupancy record, so a node that
//! takes the room's claim can seat the same occupants. The record is written
//! in the actor's own message order: a seat is recorded before the join's
//! projection commits and an unseat after the removal is applied, so the
//! record never lacks an occupant the actor holds.
//!
//! The actor keeps what it last recorded and reports the difference to its
//! live roster after every message that can change occupancy. A write that
//! fails leaves that memory unchanged, so the next report repeats it.

use std::collections::HashMap;

use jid::FullJid;
use waddle_xmpp_core::OccupancySessionGeneration;

use super::{RoomActor, RoomActorError};
use crate::muc::durable::{RoomOccupancyChange, RoomOccupancyRecord, RoomOccupancySeat};
use crate::types::{Affiliation, Role};

impl RoomActor {
    /// Every occupying session with a known connection generation, as the
    /// record should hold it.
    fn live_occupancy_seats(&self) -> HashMap<FullJid, RoomOccupancySeat> {
        let mut seats = HashMap::new();
        for occupant in self.room.occupants.values() {
            for session in self.room.occupant_sessions(&occupant.nick) {
                let Some(generation) = self.room.session_generation(&session) else {
                    continue;
                };
                seats.insert(
                    session.clone(),
                    RoomOccupancySeat {
                        occupant: session,
                        nick: occupant.nick.clone(),
                        role: occupant.role,
                        affiliation: occupant.affiliation,
                        session: generation,
                    },
                );
            }
        }
        seats
    }

    async fn write_occupancy_record(
        &self,
        changes: Vec<RoomOccupancyChange>,
    ) -> Result<RoomOccupancyRecord, ()> {
        let (Some(store), Some(fence)) = (
            self.durable_store.as_ref(),
            self.durable_claim_fence.as_ref(),
        ) else {
            return Ok(RoomOccupancyRecord::Inactive);
        };
        store
            .record_room_occupancy(&self.room.room_jid, fence, changes)
            .await
            .map_err(|error| {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "room occupancy record not written"
                );
            })
    }

    /// Record the seat a join is about to take, before the join's
    /// projection commits. A join whose seat cannot be recorded is refused,
    /// so the actor never holds an occupant the record lacks.
    pub(in crate::muc::room_actor) async fn record_seat_before_join(
        &mut self,
        occupant: &FullJid,
        nick: &str,
        affiliation: Affiliation,
        role: Role,
        session: OccupancySessionGeneration,
    ) -> Result<(), RoomActorError> {
        let seat = RoomOccupancySeat {
            occupant: occupant.clone(),
            nick: nick.to_owned(),
            role,
            affiliation,
            session,
        };
        match self
            .write_occupancy_record(vec![RoomOccupancyChange::Seat(seat.clone())])
            .await
        {
            Ok(RoomOccupancyRecord::Written) => {
                self.recorded_occupancy.insert(occupant.clone(), seat);
                Ok(())
            }
            Ok(RoomOccupancyRecord::Inactive) => Ok(()),
            Err(()) => Err(RoomActorError::OwnershipUnavailable),
        }
    }

    /// Remove every seat this actor recorded, for a room that is about to
    /// be destroyed. A failed write leaves the rows; the room's next owner
    /// does not seat rows of a room it created.
    pub(in crate::muc::room_actor) async fn unseat_recorded_occupancy(&mut self) {
        if self.recorded_occupancy.is_empty() {
            return;
        }
        let changes = self
            .recorded_occupancy
            .keys()
            .map(|occupant| RoomOccupancyChange::Unseat {
                occupant: occupant.clone(),
            })
            .collect();
        if self.write_occupancy_record(changes).await.is_ok() {
            self.recorded_occupancy.clear();
        }
    }

    /// Report the difference between the live roster and what was last
    /// recorded. `true` when the record matches the roster afterwards, or
    /// when the deployment keeps no record.
    pub(in crate::muc::room_actor) async fn report_occupancy(&mut self) -> bool {
        let live = self.live_occupancy_seats();
        let mut changes = Vec::new();
        for occupant in self.recorded_occupancy.keys() {
            if !live.contains_key(occupant) {
                changes.push(RoomOccupancyChange::Unseat {
                    occupant: occupant.clone(),
                });
            }
        }
        for (occupant, seat) in &live {
            if self.recorded_occupancy.get(occupant) != Some(seat) {
                changes.push(RoomOccupancyChange::Seat(seat.clone()));
            }
        }
        if changes.is_empty() {
            return true;
        }
        match self.write_occupancy_record(changes).await {
            Ok(RoomOccupancyRecord::Written) => {
                self.recorded_occupancy = live;
                true
            }
            Ok(RoomOccupancyRecord::Inactive) => true,
            Err(()) => false,
        }
    }
}

/// Report the room's occupancy to its durable record now. The reply is
/// whether the record matches the roster. The room's owner sends this when a
/// reader found the record short of an occupant the actor holds.
pub struct ReportOccupancy;

impl kameo::message::Message<ReportOccupancy> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        _msg: ReportOccupancy,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.report_occupancy().await
    }
}
