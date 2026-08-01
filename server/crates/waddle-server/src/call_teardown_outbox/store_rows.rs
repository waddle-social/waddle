use std::str::FromStr;

use jid::{BareJid, FullJid};
use waddle_sfu::{CallGeneration, CallId, ParticipantSid, RoomSid};

use super::{
    CallTeardownIntent, CallTeardownIntentId, CallTeardownJob, CallTeardownLastError,
    CallTeardownOutboxError, CallTeardownProducingNode, CallTeardownStatus, ClaimToken,
    TeardownTarget,
};
use crate::db::Row;

pub(super) fn decode_job(row: &Row) -> Result<CallTeardownJob, CallTeardownOutboxError> {
    let action = row.get::<String>(4)?;
    let identity = row.get::<Option<String>>(2)?;
    let room_jid = row.get::<Option<String>>(3)?;
    let participant_sid = row.get::<Option<String>>(7)?;
    let target = decode_target(&action, identity, room_jid, participant_sid)?;
    let generation = row
        .get::<Option<i64>>(5)?
        .map(|value| {
            if value <= 0 {
                return Err(CallTeardownOutboxError::InvalidGeneration(value));
            }
            let value = u64::try_from(value)
                .map_err(|_| CallTeardownOutboxError::InvalidGeneration(value))?;
            Ok(CallGeneration::try_from(value)?)
        })
        .transpose()?;
    Ok(CallTeardownJob {
        intent_id: CallTeardownIntentId::from_stored(row.get(0)?),
        intent: CallTeardownIntent {
            call_id: CallId::new(row.get::<String>(1)?)?,
            target,
            generation,
            room_sid: row
                .get::<Option<String>>(6)?
                .map(RoomSid::new)
                .transpose()?,
        },
        producing_node: row
            .get::<Option<String>>(8)?
            .map(CallTeardownProducingNode::from_db_value)
            .transpose()?,
        status: CallTeardownStatus::from_db_value(row.get(9)?)?,
        attempt_count: row.get(10)?,
        last_error: row
            .get::<Option<String>>(11)?
            .map(CallTeardownLastError::from_db_value),
        next_attempt_at_ms: row.get(12)?,
        claim_token: row.get::<Option<String>>(13)?.map(ClaimToken::from_stored),
        created_at_ms: row.get(14)?,
    })
}

fn decode_target(
    action: &str,
    identity: Option<String>,
    room_jid: Option<String>,
    participant_sid: Option<String>,
) -> Result<TeardownTarget, CallTeardownOutboxError> {
    match action {
        "remove_participant" => match (identity, room_jid) {
            (Some(identity), None) if !identity.is_empty() => Ok(TeardownTarget::Participant {
                identity: FullJid::from_str(&identity)
                    .map_err(|_| CallTeardownOutboxError::InvalidFullJid(identity))?,
                participant_sid: participant_sid.map(ParticipantSid::new).transpose()?,
            }),
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "delete_room" if identity.is_none() && room_jid.is_none() && participant_sid.is_none() => {
            Ok(TeardownTarget::Room)
        }
        "muji_presence_clear" => match (identity, room_jid) {
            (Some(departed), Some(room_jid)) if !departed.is_empty() => {
                Ok(TeardownTarget::MujiPresenceClear {
                    departed: FullJid::from_str(&departed)
                        .map_err(|_| CallTeardownOutboxError::InvalidFullJid(departed))?,
                    room_jid: BareJid::from_str(&room_jid)
                        .map_err(|_| CallTeardownOutboxError::InvalidBareJid(room_jid))?,
                    participant_sid: participant_sid.map(ParticipantSid::new).transpose()?,
                })
            }
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "muji_room_sweep" => match (identity, room_jid, participant_sid) {
            (None, Some(room_jid), None) => Ok(TeardownTarget::MujiRoomSweep {
                room_jid: BareJid::from_str(&room_jid)
                    .map_err(|_| CallTeardownOutboxError::InvalidBareJid(room_jid))?,
            }),
            _ => Err(CallTeardownOutboxError::InvalidTargetShape(
                action.to_owned(),
            )),
        },
        "delete_room" => Err(CallTeardownOutboxError::InvalidTargetShape(
            action.to_owned(),
        )),
        _ => Err(CallTeardownOutboxError::InvalidAction(action.to_owned())),
    }
}
