use std::convert::Infallible;

use jid::{BareJid, FullJid};
use kameo::message::Context;
use xmpp_parsers::presence::Presence;

use super::{AdminApplyError, AdminContext, RoomActor};
use crate::muc::admin::{is_role_change_query, AdminItem};
use crate::muc::owner::{
    apply_config_form, build_destroy_notification, ConfigFormData, DestroyRequest,
};
use crate::muc::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_role_change_presence,
};
use crate::types::{Affiliation, Role};
use crate::xep::xep0421::OccupantIdentity;

pub struct GetAdminContext {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<GetAdminContext> for RoomActor {
    type Reply = Result<AdminContext, Infallible>;

    async fn handle(
        &mut self,
        msg: GetAdminContext,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let sender_affiliation = self.room.get_affiliation(&msg.sender_jid.to_bare());
        let sender_occupant = self.room.find_occupant_by_real_jid(&msg.sender_jid);
        let sender_role = sender_occupant.map(|o| o.role).unwrap_or(Role::None);
        Ok(AdminContext {
            affiliation: sender_affiliation,
            role: sender_role,
            nick: sender_occupant.map(|occupant| occupant.nick.clone()),
        })
    }
}

pub struct ApplyAdminItems {
    pub sender_jid: FullJid,
    pub sender_affiliation: Affiliation,
    pub sender_role: Role,
    pub items: Vec<AdminItem>,
}

impl kameo::message::Message<ApplyAdminItems> for RoomActor {
    type Reply = Result<Vec<(FullJid, Presence)>, AdminApplyError>;

    async fn handle(
        &mut self,
        msg: ApplyAdminItems,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut presence_updates: Vec<(FullJid, Presence)> = Vec::new();
        let mut occupants_to_kick: Vec<String> = Vec::new();
        if is_role_change_query(&msg.items) {
            for item in &msg.items {
                let Some(target_nick) = item.nick.clone() else {
                    continue;
                };
                let Some(new_role) = item.role else {
                    continue;
                };
                let target_occupant = self
                    .room
                    .get_occupant(&target_nick)
                    .cloned()
                    .ok_or_else(|| AdminApplyError::OccupantNotFound(target_nick.clone()))?;
                let can_modify = match (
                    msg.sender_affiliation,
                    msg.sender_role,
                    target_occupant.affiliation,
                    new_role,
                ) {
                    (Affiliation::Owner, _, _, _) => true,
                    (Affiliation::Admin, _, target_aff, _) if target_aff != Affiliation::Owner => {
                        true
                    }
                    (_, Role::Moderator, target_aff, _)
                        if !matches!(target_aff, Affiliation::Owner | Affiliation::Admin) =>
                    {
                        true
                    }
                    _ => false,
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(
                        "You don't have permission to change this user's role".to_string(),
                    ));
                }
                let from_room_jid = self
                    .room
                    .room_jid
                    .with_resource_str(&target_nick)
                    .expect("nick was previously accepted as resource");
                let target_bare = target_occupant.real_jid.to_bare();
                let target_identity = OccupantIdentity {
                    bare_jid: &target_bare,
                    real_jid: Some(&target_occupant.real_jid),
                    secret: &self.occupant_id_secret,
                };
                if new_role == Role::None {
                    for (nick, occupant) in self.room.occupants.iter() {
                        let is_self = nick == &target_nick;
                        let presence = build_kick_presence(
                            &from_room_jid,
                            &occupant.real_jid,
                            target_occupant.affiliation,
                            is_self,
                            item.reason.as_deref(),
                            Some(&msg.sender_jid.to_bare()),
                            &target_identity,
                        );
                        presence_updates.push((occupant.real_jid.clone(), presence));
                    }
                    occupants_to_kick.push(target_nick);
                } else {
                    if let Some(occ) = self.room.occupants.get_mut(&target_nick) {
                        occ.role = new_role;
                    }
                    for (nick, occupant) in self.room.occupants.iter() {
                        let is_self = nick == &target_nick;
                        let presence = build_role_change_presence(
                            &from_room_jid,
                            &occupant.real_jid,
                            target_occupant.affiliation,
                            new_role,
                            is_self,
                            &target_identity,
                        );
                        presence_updates.push((occupant.real_jid.clone(), presence));
                    }
                }
            }
        } else {
            for item in &msg.items {
                let Some(target_jid) = item.jid.clone() else {
                    continue;
                };
                let Some(new_affiliation) = item.affiliation else {
                    continue;
                };
                let can_modify = match new_affiliation {
                    Affiliation::Owner => msg.sender_affiliation == Affiliation::Owner,
                    Affiliation::Admin
                    | Affiliation::Member
                    | Affiliation::None
                    | Affiliation::Outcast => {
                        matches!(
                            msg.sender_affiliation,
                            Affiliation::Owner | Affiliation::Admin
                        )
                    }
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(format!(
                        "You don't have permission to set {} affiliation",
                        crate::muc::admin::affiliation_to_str(new_affiliation)
                    )));
                }
                if new_affiliation != Affiliation::Owner {
                    let target_current_affiliation = self.room.get_affiliation(&target_jid);
                    if target_current_affiliation == Affiliation::Owner {
                        let owners = self.room.get_jids_by_affiliation(Affiliation::Owner);
                        if owners.len() == 1 && owners.contains(&target_jid) {
                            return Err(AdminApplyError::CannotRemoveLastOwner);
                        }
                    }
                }
                let change = self
                    .room
                    .set_affiliation(target_jid.clone(), new_affiliation);
                if change.is_none() {
                    continue;
                }
                let affected_occupant = self
                    .room
                    .occupants
                    .values()
                    .find(|o| o.real_jid.to_bare() == target_jid)
                    .cloned();
                if let Some(occupant) = affected_occupant {
                    let from_room_jid = self
                        .room
                        .room_jid
                        .with_resource_str(&occupant.nick)
                        .expect("nick was previously accepted as resource");
                    let occupant_bare = occupant.real_jid.to_bare();
                    let occupant_identity = OccupantIdentity {
                        bare_jid: &occupant_bare,
                        real_jid: Some(&occupant.real_jid),
                        secret: &self.occupant_id_secret,
                    };
                    if new_affiliation == Affiliation::Outcast {
                        for (nick, occ) in self.room.occupants.iter() {
                            let is_self = nick == &occupant.nick;
                            let presence = build_ban_presence(
                                &from_room_jid,
                                &occ.real_jid,
                                is_self,
                                item.reason.as_deref(),
                                Some(&msg.sender_jid.to_bare()),
                                &occupant_identity,
                            );
                            presence_updates.push((occ.real_jid.clone(), presence));
                        }
                        occupants_to_kick.push(occupant.nick.clone());
                    } else {
                        for (nick, occ) in self.room.occupants.iter() {
                            let is_self = nick == &occupant.nick;
                            let presence = build_affiliation_change_presence(
                                &from_room_jid,
                                &occ.real_jid,
                                new_affiliation,
                                occupant.role,
                                is_self,
                                &occupant_identity,
                            );
                            presence_updates.push((occ.real_jid.clone(), presence));
                        }
                    }
                }
            }
        }
        for nick in occupants_to_kick {
            self.room.remove_occupant(&nick);
        }
        Ok(presence_updates)
    }
}

pub struct IsOwner {
    pub jid: BareJid,
}

impl kameo::message::Message<IsOwner> for RoomActor {
    type Reply = bool;

    async fn handle(&mut self, msg: IsOwner, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.room.get_affiliation(&msg.jid) == Affiliation::Owner
    }
}

pub struct ApplyOwnerConfig {
    pub form_data: ConfigFormData,
}

impl kameo::message::Message<ApplyOwnerConfig> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ApplyOwnerConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        apply_config_form(&mut self.room.config, &msg.form_data);
    }
}

pub struct DestroyWithNotifications {
    pub sender_jid: FullJid,
    pub request: DestroyRequest,
}

impl kameo::message::Message<DestroyWithNotifications> for RoomActor {
    type Reply = Result<Vec<(FullJid, Presence)>, Infallible>;

    async fn handle(
        &mut self,
        msg: DestroyWithNotifications,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut updates = Vec::new();
        for (nick, occupant) in self.room.occupants.iter() {
            let is_self = occupant.real_jid == msg.sender_jid;
            let presence = build_destroy_notification(
                &self.room.room_jid,
                nick,
                &occupant.real_jid,
                &msg.request,
                is_self,
            );
            updates.push((occupant.real_jid.clone(), presence));
        }
        self.room.occupants.clear();
        Ok(updates)
    }
}
