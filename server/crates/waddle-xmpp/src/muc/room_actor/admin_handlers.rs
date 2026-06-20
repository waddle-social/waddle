use std::{collections::BTreeMap, convert::Infallible};

use jid::{BareJid, FullJid};
use kameo::message::Context;
use xmpp_parsers::presence::Presence;

use super::{AdminApplyError, AdminContext, RoomActor};
use crate::muc::admin::{is_role_change_query, AdminItem};
use crate::muc::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_membership_removal_presence, build_role_change_presence, MucRoom, Occupant,
};
use crate::types::{Affiliation, Role};
use crate::xep::xep0421::{OccupantIdSecret, OccupantIdentity};

const STATUS_AFFILIATION_CHANGE_REMOVAL: &str = "321";
const STATUS_MEMBERS_ONLY_CONFIG_REMOVAL: &str = "322";

fn all_room_sessions(room: &MucRoom) -> Vec<FullJid> {
    room.occupants
        .values()
        .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
        .collect()
}

fn occupants_for_bare(room: &MucRoom, target_jid: &BareJid) -> Vec<Occupant> {
    room.occupants
        .values()
        .filter(|occupant| occupant.real_jid.to_bare() == *target_jid)
        .cloned()
        .collect()
}

fn removal_presence_updates(
    room: &MucRoom,
    occupant_id_secret: &OccupantIdSecret,
    occupant: &Occupant,
    status_code: &'static str,
    actor: Option<&BareJid>,
) -> Vec<(FullJid, Presence)> {
    let from_room_jid = room
        .room_jid
        .with_resource_str(&occupant.nick)
        .expect("nick was previously accepted as resource");
    let occupant_bare = occupant.real_jid.to_bare();
    let occupant_identity = OccupantIdentity {
        bare_jid: &occupant_bare,
        real_jid: Some(&occupant.real_jid),
        secret: occupant_id_secret,
    };
    let removed_sessions = room.get_occupant_sessions(&occupant.nick);
    all_room_sessions(room)
        .into_iter()
        .map(|recipient| {
            let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
            let presence = build_membership_removal_presence(
                &from_room_jid,
                &recipient,
                status_code,
                is_self,
                actor,
                &occupant_identity,
            );
            (recipient, presence)
        })
        .collect()
}

fn apply_affiliation_change(
    room: &mut MucRoom,
    occupant_id_secret: &OccupantIdSecret,
    target_jid: BareJid,
    new_affiliation: Affiliation,
    actor: Option<&BareJid>,
    reason: Option<&str>,
) -> Result<Vec<(FullJid, Presence)>, AdminApplyError> {
    if new_affiliation != Affiliation::Owner {
        let target_current_affiliation = room.get_affiliation(&target_jid);
        if target_current_affiliation == Affiliation::Owner {
            let owners = room.get_jids_by_affiliation(Affiliation::Owner);
            if owners.len() == 1 && owners.contains(&target_jid) {
                return Err(AdminApplyError::CannotRemoveLastOwner);
            }
        }
    }

    let change = room.set_affiliation(target_jid.clone(), new_affiliation);
    if change.is_none() {
        return Ok(Vec::new());
    }

    let affected_occupants = occupants_for_bare(room, &target_jid);
    if affected_occupants.is_empty() {
        return Ok(Vec::new());
    }

    if new_affiliation == Affiliation::Outcast {
        let mut updates = Vec::new();
        for occupant in &affected_occupants {
            let from_room_jid = room
                .room_jid
                .with_resource_str(&occupant.nick)
                .expect("nick was previously accepted as resource");
            let occupant_bare = occupant.real_jid.to_bare();
            let occupant_identity = OccupantIdentity {
                bare_jid: &occupant_bare,
                real_jid: Some(&occupant.real_jid),
                secret: occupant_id_secret,
            };
            let removed_sessions = room.get_occupant_sessions(&occupant.nick);
            updates.extend(all_room_sessions(room).into_iter().map(|recipient| {
                let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
                let presence = build_ban_presence(
                    &from_room_jid,
                    &recipient,
                    is_self,
                    reason,
                    actor,
                    &occupant_identity,
                );
                (recipient, presence)
            }));
        }
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return Ok(updates);
    }

    if room.config.members_only && new_affiliation < Affiliation::Member {
        let mut updates = Vec::new();
        for occupant in &affected_occupants {
            updates.extend(removal_presence_updates(
                room,
                occupant_id_secret,
                occupant,
                STATUS_AFFILIATION_CHANGE_REMOVAL,
                actor,
            ));
        }
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return Ok(updates);
    }

    let mut updates = Vec::new();
    for occupant in &affected_occupants {
        let from_room_jid = room
            .room_jid
            .with_resource_str(&occupant.nick)
            .expect("nick was previously accepted as resource");
        let occupant_bare = occupant.real_jid.to_bare();
        let occupant_identity = OccupantIdentity {
            bare_jid: &occupant_bare,
            real_jid: Some(&occupant.real_jid),
            secret: occupant_id_secret,
        };
        let affected_sessions = room.get_occupant_sessions(&occupant.nick);
        updates.extend(all_room_sessions(room).into_iter().map(|recipient| {
            let is_self = affected_sessions.iter().any(|jid| jid == &recipient);
            let presence = build_affiliation_change_presence(
                &from_room_jid,
                &recipient,
                new_affiliation,
                occupant.role,
                is_self,
                &occupant_identity,
            );
            (recipient, presence)
        }));
    }
    Ok(updates)
}

fn enforce_members_only(
    room: &mut MucRoom,
    occupant_id_secret: &OccupantIdSecret,
) -> Vec<(FullJid, Presence)> {
    if !room.config.members_only {
        return Vec::new();
    }
    let removed_occupants: Vec<Occupant> = room
        .occupants
        .values()
        .filter(|occupant| occupant.affiliation < Affiliation::Member)
        .cloned()
        .collect();
    let mut presence_updates = Vec::new();
    for occupant in &removed_occupants {
        presence_updates.extend(removal_presence_updates(
            room,
            occupant_id_secret,
            occupant,
            STATUS_MEMBERS_ONLY_CONFIG_REMOVAL,
            None,
        ));
    }
    for occupant in removed_occupants {
        room.remove_occupant(&occupant.nick);
    }
    presence_updates
}

fn authorize_role_change(
    sender_affiliation: Affiliation,
    sender_role: Role,
    target_affiliation: Affiliation,
    target_role: Role,
    new_role: Role,
) -> Result<(), AdminApplyError> {
    if sender_affiliation == Affiliation::Owner {
        return Ok(());
    }
    if matches!(target_affiliation, Affiliation::Owner | Affiliation::Admin) {
        return Err(AdminApplyError::CannotModifyPrivilegedRole);
    }
    if sender_affiliation == Affiliation::Admin {
        return Ok(());
    }
    if sender_role == Role::Moderator
        && target_role != Role::Moderator
        && matches!(new_role, Role::Participant | Role::Visitor | Role::None)
    {
        return Ok(());
    }
    Err(AdminApplyError::PermissionDenied(
        "You don't have permission to change this user's role".to_string(),
    ))
}

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
                let Some(target_nick) = item.nick.as_ref() else {
                    continue;
                };
                let Some(new_role) = item.role else {
                    continue;
                };
                let target_occupant = self
                    .room
                    .get_occupant(target_nick)
                    .ok_or_else(|| AdminApplyError::OccupantNotFound(target_nick.clone()))?;
                authorize_role_change(
                    msg.sender_affiliation,
                    msg.sender_role,
                    target_occupant.affiliation,
                    target_occupant.role,
                    new_role,
                )?;
            }
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
                authorize_role_change(
                    msg.sender_affiliation,
                    msg.sender_role,
                    target_occupant.affiliation,
                    target_occupant.role,
                    new_role,
                )?;
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
                let target_sessions = self.room.get_occupant_sessions(&target_nick);
                if new_role == Role::None {
                    for recipient in all_room_sessions(&self.room) {
                        let is_self = target_sessions.iter().any(|jid| jid == &recipient);
                        let presence = build_kick_presence(
                            &from_room_jid,
                            &recipient,
                            target_occupant.affiliation,
                            is_self,
                            item.reason.as_deref(),
                            Some(&msg.sender_jid.to_bare()),
                            &target_identity,
                        );
                        presence_updates.push((recipient, presence));
                    }
                    occupants_to_kick.push(target_nick);
                } else {
                    if let Some(occ) = self.room.occupants.get_mut(&target_nick) {
                        occ.role = new_role;
                    }
                    for recipient in all_room_sessions(&self.room) {
                        let is_self = target_sessions.iter().any(|jid| jid == &recipient);
                        let presence = build_role_change_presence(
                            &from_room_jid,
                            &recipient,
                            target_occupant.affiliation,
                            new_role,
                            is_self,
                            &target_identity,
                        );
                        presence_updates.push((recipient, presence));
                    }
                }
            }
        } else {
            let mut final_affiliations: BTreeMap<BareJid, Affiliation> = self
                .room
                .get_all_affiliations()
                .into_iter()
                .map(|entry| (entry.jid, entry.affiliation))
                .collect();
            let current_owner_count = self.room.get_jids_by_affiliation(Affiliation::Owner).len();
            for item in &msg.items {
                let Some(target_jid) = item.jid.as_ref() else {
                    continue;
                };
                let Some(new_affiliation) = item.affiliation else {
                    continue;
                };
                let target_current_affiliation = self.room.get_affiliation(target_jid);
                let can_modify = match new_affiliation {
                    Affiliation::Owner | Affiliation::Admin => {
                        msg.sender_affiliation == Affiliation::Owner
                    }
                    Affiliation::Member | Affiliation::None | Affiliation::Outcast
                        if target_current_affiliation == Affiliation::Admin =>
                    {
                        msg.sender_affiliation == Affiliation::Owner
                    }
                    Affiliation::Member | Affiliation::None | Affiliation::Outcast => matches!(
                        msg.sender_affiliation,
                        Affiliation::Owner | Affiliation::Admin
                    ),
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(format!(
                        "You don't have permission to set {} affiliation",
                        crate::muc::admin::affiliation_to_str(new_affiliation)
                    )));
                }
                if msg.sender_affiliation == Affiliation::Admin
                    && target_current_affiliation == Affiliation::Owner
                    && new_affiliation != Affiliation::Owner
                {
                    return Err(AdminApplyError::CannotAdminModifyOwner);
                }
                if new_affiliation == Affiliation::None {
                    final_affiliations.remove(target_jid);
                } else {
                    final_affiliations.insert(target_jid.clone(), new_affiliation);
                }
            }
            if current_owner_count > 0
                && !final_affiliations
                    .values()
                    .any(|affiliation| *affiliation == Affiliation::Owner)
            {
                return Err(AdminApplyError::CannotRemoveLastOwner);
            }
            for item in &msg.items {
                let Some(target_jid) = item.jid.clone() else {
                    continue;
                };
                let Some(new_affiliation) = item.affiliation else {
                    continue;
                };
                let target_current_affiliation = self.room.get_affiliation(&target_jid);
                let can_modify = match new_affiliation {
                    Affiliation::Owner | Affiliation::Admin => {
                        msg.sender_affiliation == Affiliation::Owner
                    }
                    Affiliation::Member | Affiliation::None | Affiliation::Outcast
                        if target_current_affiliation == Affiliation::Admin =>
                    {
                        msg.sender_affiliation == Affiliation::Owner
                    }
                    Affiliation::Member | Affiliation::None | Affiliation::Outcast => matches!(
                        msg.sender_affiliation,
                        Affiliation::Owner | Affiliation::Admin
                    ),
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(format!(
                        "You don't have permission to set {} affiliation",
                        crate::muc::admin::affiliation_to_str(new_affiliation)
                    )));
                }
                if msg.sender_affiliation == Affiliation::Admin
                    && target_current_affiliation == Affiliation::Owner
                    && new_affiliation != Affiliation::Owner
                {
                    return Err(AdminApplyError::CannotAdminModifyOwner);
                }
                let actor = msg.sender_jid.to_bare();
                let updates = apply_affiliation_change(
                    &mut self.room,
                    &self.occupant_id_secret,
                    target_jid,
                    new_affiliation,
                    Some(&actor),
                    item.reason.as_deref(),
                )?;
                if target_current_affiliation != new_affiliation {
                    self.admission_revision = self.admission_revision.saturating_add(1);
                }
                presence_updates.extend(updates);
            }
        }
        for nick in occupants_to_kick {
            self.room.remove_occupant(&nick);
        }
        Ok(presence_updates)
    }
}

pub struct ApplyAffiliationChange {
    pub actor: Option<BareJid>,
    pub jid: BareJid,
    pub affiliation: Affiliation,
}

impl kameo::message::Message<ApplyAffiliationChange> for RoomActor {
    type Reply = Result<Vec<(FullJid, Presence)>, AdminApplyError>;

    async fn handle(
        &mut self,
        msg: ApplyAffiliationChange,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let previous_affiliation = self.room.get_affiliation(&msg.jid);
        let updates = apply_affiliation_change(
            &mut self.room,
            &self.occupant_id_secret,
            msg.jid,
            msg.affiliation,
            msg.actor.as_ref(),
            None,
        )?;
        if previous_affiliation != msg.affiliation {
            self.admission_revision = self.admission_revision.saturating_add(1);
        }
        Ok(updates)
    }
}

pub struct EnforceMembersOnly;

impl kameo::message::Message<EnforceMembersOnly> for RoomActor {
    type Reply = Vec<(FullJid, Presence)>;

    async fn handle(
        &mut self,
        _msg: EnforceMembersOnly,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        enforce_members_only(&mut self.room, &self.occupant_id_secret)
    }
}

pub struct EnforceMembersOnlyAffiliations {
    pub affiliations: Vec<(BareJid, Affiliation)>,
}

impl kameo::message::Message<EnforceMembersOnlyAffiliations> for RoomActor {
    type Reply = Vec<(FullJid, Presence)>;

    async fn handle(
        &mut self,
        msg: EnforceMembersOnlyAffiliations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let affiliations: BTreeMap<BareJid, Affiliation> = msg.affiliations.into_iter().collect();
        let occupied_jids: Vec<BareJid> = self
            .room
            .occupants
            .values()
            .map(|occupant| occupant.real_jid.to_bare())
            .collect();
        for jid in occupied_jids {
            let affiliation = affiliations.get(&jid).copied().unwrap_or(Affiliation::None);
            if self.room.set_affiliation(jid, affiliation).is_some() {
                self.admission_revision = self.admission_revision.saturating_add(1);
            }
        }
        enforce_members_only(&mut self.room, &self.occupant_id_secret)
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
