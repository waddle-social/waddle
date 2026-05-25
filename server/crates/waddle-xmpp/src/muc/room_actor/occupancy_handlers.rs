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
                self.room
                    .get_occupant_sessions(&o.nick)
                    .into_iter()
                    .map(|jid| {
                        let muji = self.room.muji_for_session(&o.nick, &jid);
                        JoinExistingOccupant {
                            jid,
                            nick: o.nick.clone(),
                            affiliation: o.affiliation,
                            role: o.role,
                            muji,
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
        let role = occupant.role;
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
        let cleared_muji_state = self
            .room
            .muji_state
            .get(&nick)
            .is_some_and(|entries| entries.contains_key(&msg.sender_jid));
        let removed_last_session = self
            .room
            .remove_occupant_session(&nick, &msg.sender_jid)
            .unwrap_or(false);
        let remaining_muji = if removed_last_session {
            None
        } else {
            self.room.muji_for_nick(&nick)
        };
        let remaining_muji_sessions = if removed_last_session {
            Vec::new()
        } else {
            self.room.muji_sessions_for_nick(&nick)
        };
        let remaining_nick_real_jid = if removed_last_session {
            None
        } else {
            self.room
                .get_occupant(&nick)
                .map(|occupant| occupant.real_jid.clone())
        };
        let occupant_count = self.room.occupant_count();
        let is_persistent = self.room.config.persistent;
        Ok(Some(LeaveOutcome {
            nick,
            affiliation,
            role,
            leaving_room_jid,
            remaining_occupants,
            removed_last_session,
            cleared_muji_state,
            remaining_muji,
            remaining_muji_sessions,
            remaining_nick_real_jid,
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

/// Upsert the `<muji xmlns='urn:xmpp:jingle:muji:0'/>` advertised
/// state (XEP-0272) for the calling session's nick. Returns a
/// presence-update outcome (occupant identity + room recipients) and
/// the post-update Muji state to embed in the broadcast.
///
/// XEP-0045 §5.1.3 / §7.1: the room is responsible for reflecting
/// in-room presence to every occupant. Sender authentication —
/// "the session is actually an occupant of this room" — happens
/// here via `find_occupant_by_real_jid`; if the sender isn't an
/// occupant, the actor returns `Ok(None)` and the caller falls
/// back to the regular join path.
pub struct UpsertMujiPresence {
    pub sender_jid: FullJid,
    pub muji: crate::xep::xep0272::Muji,
}

pub struct ClearMujiPresence {
    pub sender_jid: FullJid,
}

#[derive(Debug, Clone)]
pub struct MujiPresenceUpdateOutcome {
    pub update: PresenceUpdateOutcome,
    /// Exact Muji payload to reflect to the sending session. This
    /// preserves the XEP-0272 preparing echo even when another
    /// same-nick resource already has active contents.
    pub sender_muji: Option<crate::xep::xep0272::Muji>,
    /// Aggregate Muji payload for the occupant nick. Other occupants
    /// receive this authoritative state so one resource's preparing
    /// update cannot hide a sibling resource's active call.
    pub active_muji: Option<crate::xep::xep0272::Muji>,
    /// Exact per-session Muji payloads still advertised for this nick
    /// after the update.
    pub session_mujis: Vec<(FullJid, crate::xep::xep0272::Muji)>,
}

impl kameo::message::Message<UpsertMujiPresence> for RoomActor {
    type Reply = Result<Option<MujiPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: UpsertMujiPresence,
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
        let muji_state =
            self.room
                .upsert_muji_presence(&sender_nick, msg.sender_jid.clone(), msg.muji);
        Ok(Some(MujiPresenceUpdateOutcome {
            update: PresenceUpdateOutcome {
                sender_nick,
                sender_real_jid,
                sender_role,
                sender_affiliation,
                room_jid,
                recipients,
            },
            sender_muji: muji_state.sender_muji,
            active_muji: muji_state.room_muji,
            session_mujis: muji_state.session_mujis,
        }))
    }
}

impl kameo::message::Message<ClearMujiPresence> for RoomActor {
    type Reply = Result<Option<MujiPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: ClearMujiPresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let muji_state = self.room.clear_muji_presence(&sender_nick, &msg.sender_jid);
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        Ok(Some(MujiPresenceUpdateOutcome {
            update: PresenceUpdateOutcome {
                sender_nick,
                sender_real_jid,
                sender_role,
                sender_affiliation,
                room_jid,
                recipients,
            },
            sender_muji: muji_state.sender_muji,
            active_muji: muji_state.room_muji,
            session_mujis: muji_state.session_mujis,
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
