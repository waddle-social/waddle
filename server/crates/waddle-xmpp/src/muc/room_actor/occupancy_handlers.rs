use std::convert::Infallible;

use jid::{BareJid, FullJid};
use kameo::message::Context;

use super::{
    JoinExistingOccupant, JoinOutcome, LeaveOutcome, PresenceUpdateOutcome, RoomActor,
    RoomActorError,
};
use crate::muc::RoomConfig;
use crate::types::Affiliation;

pub struct JoinWithAffiliation {
    pub sender_jid: FullJid,
    pub nick: String,
    pub effective_affiliation: Affiliation,
    pub local_domain: String,
}

impl kameo::message::Message<JoinWithAffiliation> for RoomActor {
    type Reply = Result<JoinOutcome, RoomActorError>;

    async fn handle(
        &mut self,
        msg: JoinWithAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.effective_affiliation != Affiliation::None {
            self.room.update_affiliation_from_resolver(
                msg.sender_jid.to_bare(),
                msg.effective_affiliation,
            );
        }

        if !self.room.can_user_join(&msg.sender_jid.to_bare()) {
            return Err(RoomActorError::JoinForbidden {
                members_only: self.room.config.members_only,
            });
        }
        if self.room.is_full() {
            return Err(RoomActorError::RoomFull);
        }

        let mut is_same_bare_multi_session_join = false;
        if let Some(existing) = self.room.get_occupant(&msg.nick) {
            if existing.real_jid != msg.sender_jid {
                if existing.real_jid.to_bare() == msg.sender_jid.to_bare() {
                    is_same_bare_multi_session_join = true;
                } else {
                    return Err(RoomActorError::NickAlreadyInUse(msg.nick));
                }
            }
        }

        let existing_occupants: Vec<JoinExistingOccupant> = self
            .room
            .occupants
            .values()
            .flat_map(|o| {
                let call_extension = self.room.call_state_for_nick(&o.nick).cloned();
                self.room.get_occupant_sessions(&o.nick).into_iter().map({
                    let call_extension = call_extension.clone();
                    move |jid| JoinExistingOccupant {
                        jid,
                        nick: o.nick.clone(),
                        affiliation: o.affiliation,
                        role: o.role,
                        call_extension: call_extension.clone(),
                    }
                })
            })
            .collect();

        let new_occupant = self.room.add_occupant_with_affiliation(
            msg.sender_jid,
            msg.nick.clone(),
            Some(msg.local_domain.as_str()),
        );
        let new_occupant_affiliation = new_occupant.affiliation;
        let new_occupant_role = new_occupant.role;
        let occupant_count = self.room.occupant_count();
        let room_jid = self.room.room_jid.clone();

        let subject_state = self.room.subject.clone();

        Ok(JoinOutcome {
            existing_occupants,
            new_occupant_affiliation,
            new_occupant_role,
            occupant_count,
            room_jid,
            is_same_bare_multi_session_join,
            subject_state,
        })
    }
}

pub struct LeaveByRealJid {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<LeaveByRealJid> for RoomActor {
    type Reply = Result<Option<LeaveOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: LeaveByRealJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(nick) = self
            .room
            .find_nick_by_real_jid(&msg.sender_jid)
            .map(ToOwned::to_owned)
        else {
            return Ok(None);
        };
        let Some(occupant) = self.room.get_occupant(&nick) else {
            return Ok(None);
        };
        let affiliation = occupant.affiliation;
        let leaving_room_jid = self
            .room
            .room_jid
            .clone()
            .with_resource_str(&nick)
            .expect("nick was previously accepted as resource");
        let remaining_occupants: Vec<FullJid> = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .filter(|jid| *jid != msg.sender_jid)
            .collect();
        let removed_last_session = self
            .room
            .remove_occupant_session(&nick, &msg.sender_jid)
            .unwrap_or(false);
        let occupant_count = self.room.occupant_count();
        let is_persistent = self.room.config.persistent;
        Ok(Some(LeaveOutcome {
            nick,
            affiliation,
            leaving_room_jid,
            remaining_occupants,
            removed_last_session,
            occupant_count,
            is_persistent,
        }))
    }
}

pub struct PresenceUpdateData {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PresenceUpdateData> for RoomActor {
    type Reply = Result<Option<PresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: PresenceUpdateData,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        Ok(Some(PresenceUpdateOutcome {
            sender_nick,
            sender_real_jid,
            sender_role,
            sender_affiliation,
            room_jid,
            recipients,
        }))
    }
}

/// Upsert the `<call xmlns='urn:waddle:muc-call:0'/>` advertised
/// state for the calling session's nick. Returns a presence-update
/// outcome (occupant identity + room recipients) and the
/// post-update extension state to embed in the broadcast.
///
/// XEP-0045 §5.1.3 / §7.1: the room is responsible for reflecting
/// in-room presence to every occupant. Sender authentication —
/// "the session is actually an occupant of this room" — happens
/// here via `find_occupant_by_real_jid`; if the sender isn't an
/// occupant, the actor returns `Ok(None)` and the caller falls
/// back to the regular join path.
pub struct UpsertCallPresence {
    pub sender_jid: FullJid,
    pub extension: crate::xep::xep_waddle_muc_call::MucCallExtension,
}

#[derive(Debug, Clone)]
pub struct CallPresenceUpdateOutcome {
    pub update: PresenceUpdateOutcome,
    /// `Some(ext)` if the call indicator should be broadcast as
    /// active; `None` if the extension transitioned to inactive
    /// (the broadcast omits the `<call/>` child entirely, signalling
    /// that the occupant is no longer in a live call).
    pub active_extension: Option<crate::xep::xep_waddle_muc_call::MucCallExtension>,
}

impl kameo::message::Message<UpsertCallPresence> for RoomActor {
    type Reply = Result<Option<CallPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: UpsertCallPresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        // Bind the call advertisement to the specific session that
        // emitted it so a partial-session leave (one resource of a
        // multi-resource occupant) clears the chip even when the
        // user's other sessions remain in the room.
        let active_extension =
            self.room
                .upsert_call_state(&sender_nick, msg.sender_jid.clone(), msg.extension);
        Ok(Some(CallPresenceUpdateOutcome {
            update: PresenceUpdateOutcome {
                sender_nick,
                sender_real_jid,
                sender_role,
                sender_affiliation,
                room_jid,
                recipients,
            },
            active_extension,
        }))
    }
}

pub struct PingSelfCheck {
    pub nick: String,
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PingSelfCheck> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: PingSelfCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let occupant = self
            .room
            .get_occupant(&msg.nick)
            .ok_or_else(|| RoomActorError::OccupantNotFound(msg.nick.clone()))?;
        if occupant.real_jid != msg.sender_jid {
            return Err(RoomActorError::OccupantNotFound(
                "Self-ping only allowed for own occupant".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct ReconcileChannelBackedRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub desired_config: RoomConfig,
}

impl kameo::message::Message<ReconcileChannelBackedRoom> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ReconcileChannelBackedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let instant_name = msg.room_jid.node().map(|node| node.to_string());
        let mut desired_config = msg.desired_config;
        desired_config.description = self.room.config.description.clone();
        if !self.room.config.name.is_empty()
            && instant_name.as_deref() != Some(self.room.config.name.as_str())
        {
            desired_config.name = self.room.config.name.clone();
        }
        self.room.waddle_id = msg.waddle_id;
        self.room.channel_id = msg.channel_id;
        self.room.config = desired_config;
    }
}
