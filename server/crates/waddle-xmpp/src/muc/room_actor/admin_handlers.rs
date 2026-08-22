use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
};

use jid::{BareJid, FullJid};
use kameo::message::Context;
use xmpp_parsers::presence::Presence;

use super::{AdminApplyError, AdminContext, RoomActor};
use crate::muc::admin::{is_role_change_query, AdminItem};
use crate::muc::durable::{AffiliationEntry as DurableAffiliationEntry, RoomDurableMutation};
use crate::muc::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_membership_removal_presence, build_role_change_presence, AdminPresenceKind,
    DestroyReason, MucOccupantNick, MucPresenceStatus, MucRoom, Occupant, OccupantPresenceUpdate,
    OccupantVoiceChange, RoomEffectReservation,
};
use crate::types::{Affiliation, Role, Voice};
use crate::xep::xep0421::{OccupantIdSecret, OccupantIdentity};

const STATUS_AFFILIATION_CHANGE_REMOVAL: &str = "321";
const STATUS_MEMBERS_ONLY_CONFIG_REMOVAL: &str = "322";

fn all_room_sessions(room: &MucRoom) -> Vec<FullJid> {
    room.occupants
        .values()
        .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
        .collect()
}

fn occupied_bare_jids(room: &MucRoom) -> Vec<BareJid> {
    let mut seen = BTreeSet::new();
    room.occupants
        .values()
        .filter_map(|occupant| {
            let bare = occupant.real_jid.to_bare();
            seen.insert(bare.clone()).then_some(bare)
        })
        .collect()
}

fn visible_real_jid_for_recipient<'a>(
    _room: &MucRoom,
    _recipient: &FullJid,
    subject_real_jid: &'a FullJid,
) -> Option<&'a FullJid> {
    Some(subject_real_jid)
}

fn occupants_for_bare(room: &MucRoom, target_jid: &BareJid) -> Vec<Occupant> {
    room.occupants
        .values()
        .filter(|occupant| occupant.real_jid.to_bare() == *target_jid)
        .cloned()
        .collect()
}

fn durable_affiliation_entry(jid: BareJid, affiliation: Affiliation) -> DurableAffiliationEntry {
    DurableAffiliationEntry::new(
        jid,
        match affiliation {
            Affiliation::None => None,
            affiliation => Some(affiliation),
        },
    )
}

#[derive(Debug, Clone, PartialEq)]
struct BuiltPresenceUpdate {
    recipient: FullJid,
    presence: Presence,
    durable: DurableAdminUpdate,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct AppliedAdminMutation {
    applied: AdminItemsApplied,
    durable_updates: Vec<DurableAdminUpdate>,
}

#[derive(Debug, Clone, PartialEq)]
struct DurableAdminUpdate {
    is_self: bool,
    update: OccupantPresenceUpdate,
}

struct DurableAdminUpdateInput<'a> {
    occupant_jid: &'a FullJid,
    recipient: &'a FullJid,
    is_self: bool,
    kind: AdminPresenceKind,
    actor: Option<&'a BareJid>,
    reason: Option<&'a str>,
}

fn durable_admin_update(
    room: &MucRoom,
    occupant: &Occupant,
    input: DurableAdminUpdateInput<'_>,
) -> OccupantPresenceUpdate {
    OccupantPresenceUpdate {
        recipient: input.recipient.clone(),
        is_self: input.is_self,
        occupant: input.occupant_jid.clone(),
        nick: MucOccupantNick::new(occupant.nick.clone()).expect("nick was previously accepted"),
        occupant_bare_jid: occupant.real_jid.to_bare(),
        disclosed_real_jid: visible_real_jid_for_recipient(
            room,
            input.recipient,
            &occupant.real_jid,
        )
        .cloned(),
        affiliation: occupant.affiliation,
        kind: input.kind,
        actor: input.actor.cloned(),
        reason: input
            .reason
            .and_then(|value| DestroyReason::new(value.to_owned())),
    }
}

fn split_admin_effect_updates(
    updates: Vec<DurableAdminUpdate>,
) -> (Vec<OccupantPresenceUpdate>, Vec<OccupantPresenceUpdate>) {
    let mut self_updates = Vec::new();
    let mut remaining_updates = Vec::new();

    for durable in updates {
        if durable.is_self {
            self_updates.push(durable.update);
            continue;
        }
        remaining_updates.push(durable.update);
    }

    (self_updates, remaining_updates)
}

fn admin_effects_for_applied(
    room_jid: BareJid,
    durable_updates: Vec<DurableAdminUpdate>,
    removed_by_moderation: Vec<FullJid>,
    voice_changes: &[(FullJid, Voice)],
) -> crate::muc::RoomMutationEffects {
    if durable_updates.is_empty() && removed_by_moderation.is_empty() && voice_changes.is_empty() {
        return crate::muc::RoomMutationEffects::none();
    }

    let (self_updates, remaining_updates) = split_admin_effect_updates(durable_updates);
    crate::muc::RoomMutationEffects::admin(
        room_jid,
        self_updates,
        remaining_updates,
        removed_by_moderation,
        voice_changes
            .iter()
            .cloned()
            .map(|(session, voice)| OccupantVoiceChange { session, voice })
            .collect(),
    )
}

fn removal_presence_updates(
    room: &MucRoom,
    occupant_id_secret: &OccupantIdSecret,
    occupant: &Occupant,
    status_code: &'static str,
    kind: AdminPresenceKind,
    actor: Option<&BareJid>,
) -> Vec<BuiltPresenceUpdate> {
    let from_room_jid = room
        .room_jid
        .with_resource_str(&occupant.nick)
        .expect("nick was previously accepted as resource");
    let occupant_bare = occupant.real_jid.to_bare();
    let removed_sessions = room.get_occupant_sessions(&occupant.nick);
    all_room_sessions(room)
        .into_iter()
        .map(|recipient| {
            let occupant_identity = OccupantIdentity {
                bare_jid: &occupant_bare,
                real_jid: visible_real_jid_for_recipient(room, &recipient, &occupant.real_jid),
                secret: occupant_id_secret,
            };
            let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
            let presence = build_membership_removal_presence(
                &from_room_jid,
                &recipient,
                status_code,
                MucPresenceStatus::new(is_self, false),
                actor,
                &occupant_identity,
            );
            BuiltPresenceUpdate {
                recipient: recipient.clone(),
                presence,
                durable: DurableAdminUpdate {
                    is_self,
                    update: durable_admin_update(
                        room,
                        occupant,
                        DurableAdminUpdateInput {
                            occupant_jid: &from_room_jid,
                            recipient: &recipient,
                            is_self,
                            kind,
                            actor,
                            reason: None,
                        },
                    ),
                },
            }
        })
        .collect()
}

fn apply_affiliation_change_with_effects(
    room: &mut MucRoom,
    occupant_id_secret: &OccupantIdSecret,
    target_jid: BareJid,
    new_affiliation: Affiliation,
    actor: Option<&BareJid>,
    reason: Option<&str>,
) -> Result<AppliedAdminMutation, AdminApplyError> {
    if new_affiliation != Affiliation::Owner {
        let target_current_affiliation = room.get_affiliation(&target_jid);
        if target_current_affiliation == Affiliation::Owner {
            let owners = room.get_jids_by_affiliation(Affiliation::Owner);
            if owners.len() == 1 && owners.contains(&target_jid) {
                return Err(AdminApplyError::CannotRemoveLastOwner);
            }
        }
    }

    // An affiliation change re-derives the occupant's role
    // (`MucRoom::set_affiliation_with_provenance`), so it can silently
    // take voice away — e.g. `admin → none` demotes Moderator →
    // Visitor in a moderated room. Snapshot each affected session's
    // voice BEFORE the mutation so the non-removal path below can
    // report what actually changed and callers can converge live SFU
    // grants.
    let voices_before = session_voices(room, &target_jid);

    let change = room.set_affiliation(target_jid.clone(), new_affiliation);
    if change.is_none() {
        return Ok(AppliedAdminMutation::default());
    }

    let affected_occupants = occupants_for_bare(room, &target_jid);
    if affected_occupants.is_empty() {
        return Ok(AppliedAdminMutation::default());
    }

    if new_affiliation == Affiliation::Outcast {
        let mut updates = Vec::new();
        let mut durable_updates = Vec::new();
        let mut removed_by_moderation = Vec::new();
        for occupant in &affected_occupants {
            let from_room_jid = room
                .room_jid
                .with_resource_str(&occupant.nick)
                .expect("nick was previously accepted as resource");
            let occupant_bare = occupant.real_jid.to_bare();
            let removed_sessions = room.get_occupant_sessions(&occupant.nick);
            for recipient in all_room_sessions(room) {
                let occupant_identity = OccupantIdentity {
                    bare_jid: &occupant_bare,
                    real_jid: visible_real_jid_for_recipient(room, &recipient, &occupant.real_jid),
                    secret: occupant_id_secret,
                };
                let is_self = removed_sessions.iter().any(|jid| jid == &recipient);
                let presence = build_ban_presence(
                    &from_room_jid,
                    &recipient,
                    MucPresenceStatus::new(is_self, false),
                    reason,
                    actor,
                    &occupant_identity,
                );
                durable_updates.push(DurableAdminUpdate {
                    is_self,
                    update: durable_admin_update(
                        room,
                        occupant,
                        DurableAdminUpdateInput {
                            occupant_jid: &from_room_jid,
                            recipient: &recipient,
                            is_self,
                            kind: AdminPresenceKind::Banned,
                            actor,
                            reason,
                        },
                    ),
                });
                updates.push((recipient, presence));
            }
            removed_by_moderation.extend(removed_sessions);
        }
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return Ok(AppliedAdminMutation {
            applied: AdminItemsApplied {
                presence_updates: updates,
                removed_by_moderation,
                voice_changes: Vec::new(),
                outbox_reservation: None,
            },
            durable_updates,
        });
    }

    if room.config.members_only && new_affiliation < Affiliation::Member {
        let mut updates = Vec::new();
        let mut durable_updates = Vec::new();
        for occupant in &affected_occupants {
            for built in removal_presence_updates(
                room,
                occupant_id_secret,
                occupant,
                STATUS_AFFILIATION_CHANGE_REMOVAL,
                AdminPresenceKind::AffiliationRemoved,
                actor,
            ) {
                updates.push((built.recipient, built.presence));
                durable_updates.push(built.durable);
            }
        }
        // Status-321 removal (affiliation loss in a members-only room)
        // ends room membership just as surely as a kick or ban, so it
        // must end SFU call participation too: occupancy is the
        // precondition for being in the call at all, and leaving the
        // removed user connected would let a non-occupant keep
        // publishing to — and listening in on — a room they can no
        // longer enter. #935 originally scoped eviction to 307/301;
        // that exemption was a hole.
        let removed_by_moderation: Vec<FullJid> = affected_occupants
            .iter()
            .flat_map(|occupant| room.get_occupant_sessions(&occupant.nick))
            .collect();
        for occupant in affected_occupants {
            room.remove_occupant(&occupant.nick);
        }
        return Ok(AppliedAdminMutation {
            applied: AdminItemsApplied {
                presence_updates: updates,
                removed_by_moderation,
                voice_changes: Vec::new(),
                outbox_reservation: None,
            },
            durable_updates,
        });
    }

    let mut updates = Vec::new();
    let mut durable_updates = Vec::new();
    for occupant in &affected_occupants {
        let from_room_jid = room
            .room_jid
            .with_resource_str(&occupant.nick)
            .expect("nick was previously accepted as resource");
        let occupant_bare = occupant.real_jid.to_bare();
        let affected_sessions = room.get_occupant_sessions(&occupant.nick);
        for recipient in all_room_sessions(room) {
            let occupant_identity = OccupantIdentity {
                bare_jid: &occupant_bare,
                real_jid: visible_real_jid_for_recipient(room, &recipient, &occupant.real_jid),
                secret: occupant_id_secret,
            };
            let is_self = affected_sessions.iter().any(|jid| jid == &recipient);
            let presence = build_affiliation_change_presence(
                &from_room_jid,
                &recipient,
                new_affiliation,
                occupant.role,
                MucPresenceStatus::new(is_self, false),
                &occupant_identity,
            );
            durable_updates.push(DurableAdminUpdate {
                is_self,
                update: durable_admin_update(
                    room,
                    occupant,
                    DurableAdminUpdateInput {
                        occupant_jid: &from_room_jid,
                        recipient: &recipient,
                        is_self,
                        kind: AdminPresenceKind::RoleChanged(occupant.role),
                        actor: None,
                        reason: None,
                    },
                ),
            });
            updates.push((recipient, presence));
        }
    }
    Ok(AppliedAdminMutation {
        applied: AdminItemsApplied {
            presence_updates: updates,
            removed_by_moderation: Vec::new(),
            voice_changes: changed_session_voices(
                &voices_before,
                &session_voices(room, &target_jid),
            ),
            outbox_reservation: None,
        },
        durable_updates,
    })
}

pub(super) fn apply_affiliation_change(
    room: &mut MucRoom,
    occupant_id_secret: &OccupantIdSecret,
    target_jid: BareJid,
    new_affiliation: Affiliation,
    actor: Option<&BareJid>,
    reason: Option<&str>,
) -> Result<AdminItemsApplied, AdminApplyError> {
    apply_affiliation_change_with_effects(
        room,
        occupant_id_secret,
        target_jid,
        new_affiliation,
        actor,
        reason,
    )
    .map(|applied| applied.applied)
}

/// Current voice of every active session belonging to `target_jid`,
/// keyed by session full JID.
fn session_voices(room: &MucRoom, target_jid: &BareJid) -> Vec<(FullJid, Voice)> {
    let moderation = room.moderation();
    occupants_for_bare(room, target_jid)
        .into_iter()
        .flat_map(|occupant| {
            let voice = occupant.role.voice(moderation);
            room.get_occupant_sessions(&occupant.nick)
                .into_iter()
                .map(move |session| (session, voice))
        })
        .collect()
}

/// The sessions whose voice differs between two [`session_voices`]
/// snapshots, carrying the new value. Sessions that vanished between
/// snapshots are removals and are reported through
/// `removed_by_moderation`, not here.
fn changed_session_voices(
    before: &[(FullJid, Voice)],
    after: &[(FullJid, Voice)],
) -> Vec<(FullJid, Voice)> {
    after
        .iter()
        .filter(|(session, voice)| {
            before
                .iter()
                .find(|(prior_session, _)| prior_session == session)
                .is_none_or(|(_, prior_voice)| prior_voice != voice)
        })
        .cloned()
        .collect()
}

/// Eject occupants who lack the member affiliation after a room became
/// members-only (XEP-0045 status 322).
///
/// Returns removals as well as presences: a status-322 ejection ends
/// room membership, and occupancy is the precondition for call
/// participation, so the ejected occupant's SFU session must end too.
/// Leaving them connected would let a non-occupant keep publishing into
/// — and listening in on — a room they can no longer enter. Same
/// reasoning as the 307/301/321 paths.
pub fn enforce_members_only_from_room(
    room: &mut MucRoom,
    occupant_id_secret: &OccupantIdSecret,
) -> AdminItemsApplied {
    if !room.config.members_only {
        return AdminItemsApplied::default();
    }
    let removed_occupants: Vec<Occupant> = room
        .occupants
        .values()
        .filter(|occupant| occupant.affiliation < Affiliation::Member)
        .cloned()
        .collect();
    let mut presence_updates = Vec::new();
    let mut removed_by_moderation = Vec::new();
    for occupant in &removed_occupants {
        presence_updates.extend(
            removal_presence_updates(
                room,
                occupant_id_secret,
                occupant,
                STATUS_MEMBERS_ONLY_CONFIG_REMOVAL,
                AdminPresenceKind::MembersOnlyRemoved,
                None,
            )
            .into_iter()
            .map(|built| (built.recipient, built.presence)),
        );
        removed_by_moderation.extend(room.get_occupant_sessions(&occupant.nick));
    }
    for occupant in removed_occupants {
        room.remove_occupant(&occupant.nick);
    }
    AdminItemsApplied {
        presence_updates,
        removed_by_moderation,
        voice_changes: Vec::new(),
        outbox_reservation: None,
    }
}

/// XEP-0045 role-change authorization: target protection first, actor
/// privilege second.
///
/// The target-protection matrix applies to EVERY actor — owners
/// included (#1262):
/// - §8.4 (revoke voice): "a service MUST NOT allow the voice
///   privileges of an admin or owner to be removed by anyone", and "a
///   moderator MUST NOT be able to revoke voice from a user whose
///   affiliation is at or above the moderator's level".
/// - §9.7 (revoke moderator): moderator status "cannot be revoked from
///   a room owner or room admin".
/// - §8.2 (kick): "a user cannot be kicked by a moderator with a lower
///   affiliation" — an owner may kick an admin, but an admin may never
///   kick an owner.
///
/// Role and affiliation are orthogonal; without these checks an owner
/// could force an admin/owner into `visitor`, a state the XEP declares
/// impossible.
fn authorize_role_change(
    sender_affiliation: Affiliation,
    sender_role: Role,
    target_affiliation: Affiliation,
    target_role: Role,
    new_role: Role,
) -> Result<(), AdminApplyError> {
    let target_privileged = matches!(target_affiliation, Affiliation::Owner | Affiliation::Admin);
    let sender_privileged = matches!(sender_affiliation, Affiliation::Owner | Affiliation::Admin);
    match new_role {
        // §8.2 kick (role='none'): only a target with a strictly higher
        // affiliation than the sender is protected from being kicked.
        Role::None => {
            if target_affiliation > sender_affiliation {
                return Err(AdminApplyError::CannotModifyPrivilegedRole);
            }
        }
        // §8.4 revoke voice (role='visitor'): admins/owners keep voice
        // against everyone; a plain moderator additionally may not
        // devoice a target at or above their own affiliation level.
        Role::Visitor => {
            if target_privileged || (!sender_privileged && target_affiliation >= sender_affiliation)
            {
                return Err(AdminApplyError::CannotModifyPrivilegedRole);
            }
        }
        // §9.7 revoke moderator (role='participant'): admins/owners
        // keep moderator status against everyone, the room owner
        // included.
        Role::Participant => {
            if target_privileged && target_role == Role::Moderator {
                return Err(AdminApplyError::CannotModifyPrivilegedRole);
            }
        }
        // Granting moderator status never demotes a protected target.
        Role::Moderator => {}
    }
    if sender_affiliation == Affiliation::Owner {
        return Ok(());
    }
    if target_privileged {
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

/// Outcome of a moderation action that may involuntarily remove
/// occupants.
///
/// `removed_by_moderation` carries the full JID of every session
/// removed by a kick (XEP-0045 §8.2, status 307) or ban (§9.1, status
/// 301). Callers owning an SFU handle must end these sessions' call
/// participation — Membership-scoped visibility says a call is only
/// joinable (and stayable) by current occupants. Voluntary leaves and
/// presence loss never appear here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AdminItemsApplied {
    pub presence_updates: Vec<(FullJid, Presence)>,
    pub removed_by_moderation: Vec<FullJid>,
    /// Every session whose XEP-0045 voice changed *without* leaving
    /// the room, with the voice now in effect. Callers owning an SFU
    /// handle must converge these sessions' live media grants — losing
    /// voice revokes publish rights on the SFU, regaining it restores
    /// them.
    ///
    /// Both triggers are covered: an explicit `<item role='…'/>`
    /// change, and an affiliation change that re-derives the
    /// occupant's role (e.g. `admin → none` demotes Moderator →
    /// Visitor in a moderated room — see
    /// [`MucRoom::set_affiliation_with_provenance`]). Removals
    /// (kick/ban) never appear here; they are terminal and carried by
    /// `removed_by_moderation` instead.
    pub voice_changes: Vec<(FullJid, Voice)>,
    pub outbox_reservation: Option<RoomEffectReservation>,
}

impl kameo::message::Message<ApplyAdminItems> for RoomActor {
    type Reply = Result<AdminItemsApplied, AdminApplyError>;

    async fn handle(
        &mut self,
        msg: ApplyAdminItems,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.items.iter().any(|item| {
            item.affiliation.is_some()
                && item
                    .jid
                    .as_ref()
                    .is_some_and(|jid| self.invite_rollback_pending(jid))
        }) {
            return Err(AdminApplyError::InviteRollbackPending);
        }
        if is_role_change_query(&msg.items) {
            self.gate_mutation().await?;
            let mut presence_updates: Vec<(FullJid, Presence)> = Vec::new();
            let mut removed_by_moderation: Vec<FullJid> = Vec::new();
            let mut voice_changes: Vec<(FullJid, Voice)> = Vec::new();
            let mut occupants_to_kick: Vec<String> = Vec::new();
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
                let target_sessions = self.room.get_occupant_sessions(&target_nick);
                if new_role == Role::None {
                    for recipient in all_room_sessions(&self.room) {
                        let target_identity = OccupantIdentity {
                            bare_jid: &target_bare,
                            real_jid: visible_real_jid_for_recipient(
                                &self.room,
                                &recipient,
                                &target_occupant.real_jid,
                            ),
                            secret: &self.occupant_id_secret,
                        };
                        let is_self = target_sessions.iter().any(|jid| jid == &recipient);
                        let presence = build_kick_presence(
                            &from_room_jid,
                            &recipient,
                            target_occupant.affiliation,
                            MucPresenceStatus::new(is_self, false),
                            item.reason.as_deref(),
                            Some(&msg.sender_jid.to_bare()),
                            &target_identity,
                        );
                        presence_updates.push((recipient, presence));
                    }
                    removed_by_moderation.extend(target_sessions.iter().cloned());
                    occupants_to_kick.push(target_nick);
                } else {
                    if let Some(occ) = self.room.occupants.get_mut(&target_nick) {
                        occ.role = new_role;
                    }
                    let new_voice = new_role.voice(self.room.moderation());
                    if target_occupant.role.voice(self.room.moderation()) != new_voice {
                        voice_changes.extend(
                            target_sessions
                                .iter()
                                .map(|session| (session.clone(), new_voice)),
                        );
                    }
                    for recipient in all_room_sessions(&self.room) {
                        let target_identity = OccupantIdentity {
                            bare_jid: &target_bare,
                            real_jid: visible_real_jid_for_recipient(
                                &self.room,
                                &recipient,
                                &target_occupant.real_jid,
                            ),
                            secret: &self.occupant_id_secret,
                        };
                        let is_self = target_sessions.iter().any(|jid| jid == &recipient);
                        let presence = build_role_change_presence(
                            &from_room_jid,
                            &recipient,
                            target_occupant.affiliation,
                            new_role,
                            MucPresenceStatus::new(is_self, false),
                            &target_identity,
                        );
                        presence_updates.push((recipient, presence));
                    }
                }
            }
            for nick in occupants_to_kick {
                self.room.remove_occupant(&nick);
            }
            return Ok(AdminItemsApplied {
                presence_updates,
                removed_by_moderation,
                voice_changes,
                outbox_reservation: None,
            });
        }

        let current_affiliations: BTreeMap<BareJid, Affiliation> = self
            .room
            .get_all_affiliations()
            .into_iter()
            .map(|entry| (entry.jid, entry.affiliation))
            .collect();
        let mut final_affiliations = current_affiliations.clone();
        let mut touched_jids = Vec::new();
        let current_owner_count = self.room.get_jids_by_affiliation(Affiliation::Owner).len();
        // Pre-validation simulates the mutation loop step by step
        // against an evolving affiliation map. It must reject
        // every set the loop below would reject — the loop
        // mutates the room (bans remove occupants) as it goes, so
        // a mid-loop error would leave a partially-applied batch
        // whose removals and 301 presences are silently dropped
        // (#935 review). Erroring here keeps the set atomic.
        for item in &msg.items {
            let Some(target_jid) = item.jid.as_ref() else {
                continue;
            };
            let Some(new_affiliation) = item.affiliation else {
                continue;
            };
            if !touched_jids.contains(target_jid) {
                touched_jids.push(target_jid.clone());
            }
            let target_current_affiliation = final_affiliations
                .get(target_jid)
                .copied()
                .unwrap_or(Affiliation::None);
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
            if new_affiliation != Affiliation::Owner
                && target_current_affiliation == Affiliation::Owner
            {
                let sole_owner = final_affiliations
                    .iter()
                    .filter(|(_, affiliation)| **affiliation == Affiliation::Owner)
                    .all(|(jid, _)| jid == target_jid);
                if sole_owner {
                    return Err(AdminApplyError::CannotRemoveLastOwner);
                }
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

        let durable_delta: Vec<DurableAffiliationEntry> = touched_jids
            .iter()
            .filter_map(|jid| {
                let current = current_affiliations
                    .get(jid)
                    .copied()
                    .unwrap_or(Affiliation::None);
                let final_affiliation = final_affiliations
                    .get(jid)
                    .copied()
                    .unwrap_or(Affiliation::None);
                (current != final_affiliation)
                    .then(|| durable_affiliation_entry(jid.clone(), final_affiliation))
            })
            .collect();

        let mut staged_room = self.room.clone();
        let actor = msg.sender_jid.to_bare();
        let mut presence_updates: Vec<(FullJid, Presence)> = Vec::new();
        let mut removed_by_moderation: Vec<FullJid> = Vec::new();
        let mut voice_changes: Vec<(FullJid, Voice)> = Vec::new();
        let mut durable_updates = Vec::new();
        let mut invalidated_jids = Vec::new();
        let mut changed_affiliations = Vec::new();
        let mut requested_affiliations = Vec::new();
        for item in &msg.items {
            let Some(target_jid) = item.jid.clone() else {
                continue;
            };
            let Some(new_affiliation) = item.affiliation else {
                continue;
            };
            invalidated_jids.push(target_jid.clone());
            requested_affiliations.push((target_jid.clone(), new_affiliation));
            let target_current_affiliation = staged_room.get_affiliation(&target_jid);
            let AppliedAdminMutation {
                applied,
                durable_updates: applied_durable_updates,
            } = apply_affiliation_change_with_effects(
                &mut staged_room,
                &self.occupant_id_secret,
                target_jid.clone(),
                new_affiliation,
                Some(&actor),
                item.reason.as_deref(),
            )?;
            if target_current_affiliation != new_affiliation {
                changed_affiliations.push((target_jid.clone(), new_affiliation));
            }
            presence_updates.extend(applied.presence_updates);
            removed_by_moderation.extend(applied.removed_by_moderation);
            voice_changes.extend(applied.voice_changes);
            durable_updates.extend(applied_durable_updates);
        }

        let outbox_reservation = if durable_delta.is_empty() {
            self.gate_pre_mutation_ownership()
                .await
                .map_err(super::RoomMutationError::from)?;
            None
        } else {
            let (_, reservation) = self
                .commit_durable(
                    RoomDurableMutation::AffiliationBatch(durable_delta),
                    admin_effects_for_applied(
                        self.room.room_jid.clone(),
                        durable_updates,
                        removed_by_moderation.clone(),
                        &voice_changes,
                    ),
                )
                .await?;
            reservation
        };

        let mut needs_rehydration = false;
        for jid in invalidated_jids {
            self.invalidate_invite_grant(&jid);
        }
        for (jid, affiliation) in requested_affiliations {
            needs_rehydration |= self.prune_durable_recipient_if_removed(&jid, affiliation);
        }
        for (jid, _) in changed_affiliations {
            self.advance_member_admission_revision(&jid);
        }
        self.room = staged_room;
        if needs_rehydration {
            // R1: converge the durable-recipient mirror to the durable
            // channel∪space truth after any removal to `None` — a
            // space-entitled member must not lose fan-out (see
            // `RoomActor::refresh_durable_recipients_from_source`).
            self.refresh_durable_recipients_from_source().await;
        }
        Ok(AdminItemsApplied {
            presence_updates,
            removed_by_moderation,
            voice_changes,
            outbox_reservation,
        })
    }
}

pub struct ApplyAffiliationChange {
    pub actor: Option<BareJid>,
    pub jid: BareJid,
    pub affiliation: Affiliation,
}

impl kameo::message::Message<ApplyAffiliationChange> for RoomActor {
    type Reply = Result<AdminItemsApplied, AdminApplyError>;

    async fn handle(
        &mut self,
        msg: ApplyAffiliationChange,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.invite_rollback_pending(&msg.jid) {
            return Err(AdminApplyError::InviteRollbackPending);
        }
        let previous_affiliation = self.room.get_affiliation(&msg.jid);
        let mut staged_room = self.room.clone();
        let AppliedAdminMutation {
            mut applied,
            durable_updates,
        } = apply_affiliation_change_with_effects(
            &mut staged_room,
            &self.occupant_id_secret,
            msg.jid.clone(),
            msg.affiliation,
            msg.actor.as_ref(),
            None,
        )?;
        let outbox_reservation = if previous_affiliation != msg.affiliation {
            let (_, reservation) = self
                .commit_durable(
                    RoomDurableMutation::Affiliation(durable_affiliation_entry(
                        msg.jid.clone(),
                        msg.affiliation,
                    )),
                    admin_effects_for_applied(
                        self.room.room_jid.clone(),
                        durable_updates,
                        applied.removed_by_moderation.clone(),
                        &applied.voice_changes,
                    ),
                )
                .await?;
            reservation
        } else {
            self.gate_pre_mutation_ownership()
                .await
                .map_err(super::RoomMutationError::from)?;
            None
        };
        self.invalidate_invite_grant(&msg.jid);
        let needs_rehydration = self.prune_durable_recipient_if_removed(&msg.jid, msg.affiliation);
        if previous_affiliation != msg.affiliation {
            self.advance_member_admission_revision(&msg.jid);
        }
        self.room = staged_room;
        if needs_rehydration {
            self.refresh_durable_recipients_from_source().await;
        }
        applied.outbox_reservation = outbox_reservation;
        Ok(applied)
    }
}

pub struct EnforceMembersOnly;

impl kameo::message::Message<EnforceMembersOnly> for RoomActor {
    type Reply = Result<AdminItemsApplied, super::RoomMutationError>;

    async fn handle(
        &mut self,
        _msg: EnforceMembersOnly,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.effectful_work_is_permitted().await {
            return Err(super::RoomMutationError::OwnershipUnavailable);
        }
        Ok(enforce_members_only_from_room(
            &mut self.room,
            &self.occupant_id_secret,
        ))
    }
}

pub struct EnforceMembersOnlyAffiliations {
    pub affiliations: Vec<(BareJid, Affiliation)>,
    pub fallback_reservation: Option<RoomEffectReservation>,
    pub config_status_codes: Vec<crate::muc::MucConfigStatusCode>,
}

impl kameo::message::Message<EnforceMembersOnlyAffiliations> for RoomActor {
    type Reply = Result<AdminItemsApplied, super::AffiliationMutationError>;

    async fn handle(
        &mut self,
        msg: EnforceMembersOnlyAffiliations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let EnforceMembersOnlyAffiliations {
            affiliations,
            fallback_reservation,
            config_status_codes,
        } = msg;
        let affiliations: BTreeMap<BareJid, Affiliation> = affiliations.into_iter().collect();
        let occupied_jids = occupied_bare_jids(&self.room);
        if occupied_jids
            .iter()
            .any(|jid| self.invite_rollback_pending(jid))
        {
            return Err(super::AffiliationMutationError::InviteRollbackPending);
        }
        // These `set_affiliation` calls re-derive occupant roles, so an
        // occupant who is NOT ejected below can still silently lose
        // voice. Snapshot before the loop so the caller can converge
        // their live SFU grants.
        let voices_before: Vec<(FullJid, Voice)> = occupied_jids
            .iter()
            .flat_map(|jid| session_voices(&self.room, jid))
            .collect();
        let mut staged_room = self.room.clone();
        let mut changed_affiliations = Vec::new();
        let mut requested_affiliations = Vec::new();
        let mut durable_delta = Vec::new();
        for jid in &occupied_jids {
            let affiliation = affiliations.get(jid).copied().unwrap_or(Affiliation::None);
            requested_affiliations.push((jid.clone(), affiliation));
            if staged_room
                .set_affiliation(jid.clone(), affiliation)
                .is_some()
            {
                changed_affiliations.push((jid.clone(), affiliation));
                durable_delta.push(durable_affiliation_entry(jid.clone(), affiliation));
            }
        }
        let mut needs_rehydration = false;
        for jid in &occupied_jids {
            self.invalidate_invite_grant(jid);
        }
        for (jid, affiliation) in requested_affiliations {
            needs_rehydration |= self.prune_durable_recipient_if_removed(&jid, affiliation);
        }
        for (jid, _) in changed_affiliations {
            self.advance_member_admission_revision(&jid);
        }
        let mut applied =
            enforce_members_only_from_room(&mut staged_room, &self.occupant_id_secret);
        // Report voice losses for occupants who survived the
        // members-only sweep; ejected sessions are carried by
        // `removed_by_moderation` and must not be double-reported.
        // Expand per occupant (not per bare JID) so a bare seated under
        // two nicks is not counted twice.
        let moderation = staged_room.moderation();
        let voices_after: Vec<(FullJid, Voice)> = staged_room
            .occupants
            .values()
            .flat_map(|occupant| {
                let voice = occupant.role.voice(moderation);
                staged_room
                    .get_occupant_sessions(&occupant.nick)
                    .into_iter()
                    .map(move |session| (session, voice))
            })
            .collect();
        applied.voice_changes = changed_session_voices(&voices_before, &voices_after)
            .into_iter()
            .filter(|(session, _)| !applied.removed_by_moderation.contains(session))
            .collect();
        let should_commit = !durable_delta.is_empty() || !config_status_codes.is_empty();
        let durable_effect_updates = if config_status_codes.is_empty() {
            Vec::new()
        } else {
            occupied_jids
                .iter()
                .filter_map(|jid| {
                    let affiliation = affiliations.get(jid).copied().unwrap_or(Affiliation::None);
                    (affiliation < Affiliation::Member).then(|| occupants_for_bare(&self.room, jid))
                })
                .flatten()
                .flat_map(|occupant| {
                    removal_presence_updates(
                        &self.room,
                        &self.occupant_id_secret,
                        &occupant,
                        STATUS_MEMBERS_ONLY_CONFIG_REMOVAL,
                        AdminPresenceKind::MembersOnlyRemoved,
                        None,
                    )
                })
                .map(|built| built.durable)
                .collect()
        };
        let effects = if config_status_codes.is_empty() {
            crate::muc::RoomMutationEffects::none()
        } else {
            let (self_updates, remaining_updates) =
                split_admin_effect_updates(durable_effect_updates);
            let effects = crate::muc::RoomMutationEffects::members_only_enforcement(
                self.room.room_jid.clone(),
                self_updates,
                remaining_updates,
                applied.removed_by_moderation.clone(),
                applied
                    .voice_changes
                    .iter()
                    .cloned()
                    .map(|(session, voice)| OccupantVoiceChange { session, voice })
                    .collect(),
                config_status_codes,
                all_room_sessions(&staged_room),
            );
            match fallback_reservation {
                Some(reservation) => effects.with_superseding_reservation(reservation),
                None => effects,
            }
        };
        let reservation = if !should_commit {
            self.gate_pre_mutation_ownership()
                .await
                .map_err(super::RoomMutationError::from)?;
            None
        } else {
            let (_, reservation) = self
                .commit_durable(
                    RoomDurableMutation::MembersOnlyEnforcement {
                        config: self.room.config.clone(),
                        affiliations: durable_delta,
                    },
                    effects,
                )
                .await?;
            reservation
        };
        self.room = staged_room;
        self.config_durable_coordinates = self.durable_coordinates;
        if needs_rehydration {
            // R1: see `RoomActor::refresh_durable_recipients_from_source`.
            self.refresh_durable_recipients_from_source().await;
        }
        applied.outbox_reservation = reservation;
        Ok(applied)
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

#[cfg(test)]
mod voice_change_tests {
    use super::*;
    use crate::muc::RoomConfig;
    use crate::xep::xep0421::OccupantIdSecret;

    fn moderated_room() -> MucRoom {
        MucRoom::new(
            "voiceroom@muc.example.com".parse().expect("room jid"),
            "waddle-1".to_string(),
            "channel-1".to_string(),
            // An OPEN moderated room: `members_only` defaults to true,
            // and affiliation loss in a members-only room is a removal
            // (status 321) rather than a devoice, which is a different
            // path.
            RoomConfig {
                moderated: true,
                members_only: false,
                ..RoomConfig::default()
            },
        )
    }

    fn secret() -> OccupantIdSecret {
        OccupantIdSecret::new(b"occupant-id-secret-at-least-32-bytes".to_vec())
            .expect("test secret meets min length")
    }

    fn seat(room: &mut MucRoom, nick: &str, jid: &str, affiliation: Affiliation, role: Role) {
        let real_jid: FullJid = jid.parse().expect("occupant full jid");
        room.set_affiliation(real_jid.to_bare(), affiliation);
        room.add_occupant(Occupant {
            real_jid,
            nick: nick.to_string(),
            role,
            affiliation,
            is_remote: false,
            home_server: None,
        });
    }

    /// An affiliation change re-derives the occupant's role, so
    /// `admin → none` in a moderated room silently takes voice away.
    /// That MUST surface as a voice change or the occupant keeps
    /// publishing on the SFU while XMPP treats them as a visitor.
    #[test]
    fn affiliation_demotion_that_removes_voice_reports_a_voice_change() {
        let mut room = moderated_room();
        seat(
            &mut room,
            "mallory",
            "mallory@example.com/web",
            Affiliation::Admin,
            Role::Moderator,
        );
        // Keep an owner seated so the last-owner guard doesn't fire.
        seat(
            &mut room,
            "owner",
            "owner@example.com/web",
            Affiliation::Owner,
            Role::Moderator,
        );

        let applied = apply_affiliation_change(
            &mut room,
            &secret(),
            "mallory@example.com".parse().expect("bare jid"),
            Affiliation::None,
            None,
            None,
        )
        .expect("affiliation change applies");

        assert_eq!(
            applied.voice_changes.len(),
            1,
            "the demoted session's lost voice must be reported: {:?}",
            applied.voice_changes
        );
        assert_eq!(
            applied.voice_changes[0].0.to_string(),
            "mallory@example.com/web"
        );
        assert_eq!(applied.voice_changes[0].1, Voice::Muted);
        assert!(
            applied.removed_by_moderation.is_empty(),
            "an affiliation demotion is not a removal"
        );
    }

    /// The same demotion in an UNMODERATED room leaves voice intact
    /// (XEP-0045 §5.1.2 footnote), so there is nothing to converge and
    /// no wasted SFU round-trip.
    #[test]
    fn affiliation_demotion_in_unmoderated_room_reports_no_voice_change() {
        let mut room = MucRoom::new(
            "openroom@muc.example.com".parse().expect("room jid"),
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig {
                members_only: false,
                ..RoomConfig::default()
            },
        );
        assert!(!room.config.moderated, "fixture must be unmoderated");
        seat(
            &mut room,
            "mallory",
            "mallory@example.com/web",
            Affiliation::Admin,
            Role::Moderator,
        );
        seat(
            &mut room,
            "owner",
            "owner@example.com/web",
            Affiliation::Owner,
            Role::Moderator,
        );

        let applied = apply_affiliation_change(
            &mut room,
            &secret(),
            "mallory@example.com".parse().expect("bare jid"),
            Affiliation::None,
            None,
            None,
        )
        .expect("affiliation change applies");

        assert!(
            applied.voice_changes.is_empty(),
            "an unmoderated room's participant keeps voice: {:?}",
            applied.voice_changes
        );
    }

    /// A members-only room's affiliation-loss removal (status 321)
    /// ends room membership, so it must also end SFU participation —
    /// otherwise a non-occupant keeps publishing into a room they can
    /// no longer enter.
    #[test]
    fn members_only_affiliation_removal_evicts_from_the_call() {
        let mut room = MucRoom::new(
            "privateroom@muc.example.com".parse().expect("room jid"),
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
        );
        seat(
            &mut room,
            "owner",
            "owner@example.com/web",
            Affiliation::Owner,
            Role::Moderator,
        );
        seat(
            &mut room,
            "bob",
            "bob@example.com/web",
            Affiliation::Member,
            Role::Participant,
        );

        let applied = apply_affiliation_change(
            &mut room,
            &secret(),
            "bob@example.com".parse().expect("bare jid"),
            Affiliation::None,
            None,
            None,
        )
        .expect("affiliation change applies");

        assert_eq!(
            applied
                .removed_by_moderation
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["bob@example.com/web".to_string()],
            "a 321 removal must evict the removed occupant's SFU session"
        );
    }
}
