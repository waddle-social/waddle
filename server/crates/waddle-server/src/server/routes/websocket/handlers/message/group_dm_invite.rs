use std::collections::BTreeMap;

use tracing::warn;
use waddle_xmpp::{
    ingress::{
        FrozenStanzaError, GroupDmHistoryVisibility, GroupDmMembershipGrant, IngressEffectIntent,
    },
    muc::room_actor::{ChangeAffiliation, GetAdminContext, GetConfig, GetSnapshot},
    muc::room_registry_actor::GetRoom,
    protocol::handlers::errors::message_error_reply,
};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::auth::Session;
use crate::db::ValueExt;
use crate::server::routes::interpret::effects::{
    AuthorizationDeniedReason, PlanRejection, PolicyDeniedReason,
};
use crate::server::routes::interpret::effects::{
    Effect, EffectOutcome, ExternalEffect, PlannedEffect,
};
use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_group_dm_mediated_invite(
    incoming: &xmpp_parsers::message::Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    authenticated_session: Option<&Session>,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> Option<Vec<waddle_xmpp::Stanza>> {
    if incoming.type_ != xmpp_parsers::message::MessageType::Normal {
        return None;
    }
    let room_jid = incoming.to.as_ref()?.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }

    let (invitee, inbound_invite) = super::muc_invite::mediated_invitee(incoming)?;
    let channel_id = waddle_xmpp::parse_managed_room_jid(&room_jid)?;
    let channel = crate::server::xmpp_state::get_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &channel_id,
    )
    .await
    .ok()
    .flatten()?;
    if channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
        return None;
    }
    let Some(room_actor) = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
        .ok()
        .flatten()
    else {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(context) = room_actor
        .ask(GetAdminContext {
            sender_jid: bound_jid.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    };
    if context.affiliation < waddle_xmpp::Affiliation::Member {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::Forbidden,
            "Only group-DM members may invite people.",
        )]);
    }
    let invitee_blocklist = match state
        .deps
        .protocol
        .blocking_storage
        .list_blocked_jid_entries(&invitee)
        .await
    {
        Ok(entries) => waddle_xmpp::protocol::Blocklist::new(entries),
        Err(error) => {
            warn!(
                invitee = %invitee,
                error = %error,
                "Suppressing group-DM invite because blocklist lookup failed"
            );
            deps.effects.set_rejection(PlanRejection::PolicyDenied(
                PolicyDeniedReason::OperationalFenceLoss,
            ));
            return Some(vec![]);
        }
    };
    if invitee_blocklist.contains_jid(&jid::Jid::from(bound_jid.clone())) {
        deps.effects
            .set_rejection(PlanRejection::AuthorizationDenied(
                AuthorizationDeniedReason::BlockedSender,
            ));
        return Some(vec![]);
    }

    let Some(_session) = authenticated_session else {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::NotAuthorized,
            "Authentication required.",
        )]);
    };
    if let Err(error) = crate::admin::channels::validate_group_dm_invitee(
        &state.deps.app_state,
        &bound_jid.to_bare(),
        &invitee,
    )
    .await
    {
        return Some(vec![xmpp_error_reply(incoming, bound_jid, deps, error)]);
    }
    let Ok(invitee_context) = room_actor
        .ask(GetAdminContext {
            sender_jid: invitee
                .clone()
                .with_resource_str("group-dm-invite-check")
                .expect("static resource is valid"),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    };
    if invitee_context.affiliation >= waddle_xmpp::Affiliation::Member {
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::Conflict,
            "Invitee is already a group-DM member.",
        )]);
    }

    let requested_access =
        waddle_xmpp::xep::xep_waddle_group_dm::history_access_from_mediated_invite(&inbound_invite)
            .unwrap_or(waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::FromJoin);
    let inviter_has_full_history =
        group_dm_archive_boundary(state, &room_jid, &bound_jid.to_bare())
            .await
            .map(|boundary| boundary.is_none())
            .unwrap_or(false);
    let access = match requested_access {
        waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::Full
            if inviter_has_full_history =>
        {
            waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::Full
        }
        _ => waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::FromJoin,
    };
    let history_visibility = match access {
        waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::Full => {
            GroupDmHistoryVisibility::Full
        }
        waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess::FromJoin => {
            GroupDmHistoryVisibility::FromJoin {
                visible_after: chrono::Utc::now(),
            }
        }
    };
    let grant = GroupDmMembershipGrant {
        room: room_jid.clone(),
        invitee: invitee.clone(),
        inviter: bound_jid.to_bare(),
        history_visibility,
    };
    let mutation = GroupDmMembershipMutation {
        grant: grant.clone(),
        incoming: incoming.clone(),
        sender: bound_jid.clone(),
        actor: room_actor.clone(),
    };
    let membership = match deps
        .effects
        .execute(
            PlannedEffect::new(Effect::External(ExternalEffect::RoomMembershipMutation(
                crate::server::routes::interpret::effects::early::RoomMembershipMutation::GroupDm(
                    Box::new(mutation.clone()),
                ),
            ))),
            deps,
        )
        .await
    {
        EffectOutcome::Membership(outcome) => outcome,
        EffectOutcome::Frames(frames) => return Some(frames),
        _ => return Some(vec![]),
    };
    let ledger = crate::server::routes::websocket::muc_invites::OutstandingInvite {
        room: room_jid.clone(),
        invitee: invitee.clone(),
        inviter: bound_jid.to_bare(),
    };
    let grant_failure = (matches!(
        membership,
        crate::server::routes::interpret::effects::MembershipOutcome::Granted { .. }
    ))
    .then(|| {
        crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RollbackGroupDm {
            grant: Box::new(mutation),
            invite: ledger.clone(),
        }
    });
    // #1264: record the outstanding invite so a later XEP-0045 §7.8.2
    // `<decline/>` from this invitee verifies against the ledger and
    // routes to this inviter.
    match deps
        .effects
        .execute(
            PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
                super::muc_invite::InviteLedgerMutation::Record {
                    invite: ledger.clone(),
                    recorded_at: chrono::Utc::now(),
                    failure: grant_failure.clone().map(Box::new),
                },
            ))).with_dependency(crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomMembership {
                room: room_jid.clone(), member: invitee.clone(),
            }),
            deps,
        )
        .await
    {
        EffectOutcome::InviteLedger(Ok(super::muc_invite::InviteLedgerOutcome::Recorded(
            crate::server::routes::websocket::muc_invites::RecordOutcome::New { .. },
        ))) => {}
        // #1276 Greptile P1 "Duplicate Invites Redeliver": an identical
        // unexpired re-invite is a silent success with NO second
        // delivery. The member tuple was already granted by the first
        // invite (this call's grant is idempotent), so re-sending would
        // only flood the invitee or exhaust their offline
        // pending-delivery quota. Matches the sibling `muc_invite`
        // mediated-invite flow; no rollback — the desired end state
        // (member + outstanding invite) already holds.
        EffectOutcome::InviteLedger(Ok(super::muc_invite::InviteLedgerOutcome::Recorded(
            crate::server::routes::websocket::muc_invites::RecordOutcome::AlreadyOutstanding,
        ))) => {
            return Some(vec![]);
        }
        EffectOutcome::InviteLedger(Err(error)) => {
            warn!(
                room = %room_jid,
                invitee = %invitee,
                error = %error,
                "Failed to record outstanding group-DM invite; rolling back grant"
            );
            return Some(vec![error_reply(
                incoming,
                bound_jid,
                deps,
                GroupDmInviteError::InternalServerError,
                "Internal server error.",
            )]);
        }
        _ => return Some(vec![]),
    }

    let mut invite = incoming.clone();
    invite.from = Some(jid::Jid::from(room_jid.clone()));
    invite.to = Some(jid::Jid::from(invitee.clone()));
    invite.payloads = vec![build_server_mediated_invite_payload(
        &bound_jid.to_bare(),
        &invitee,
        &inbound_invite,
        access,
    )];
    let delivery_sink = crate::server::routes::interpret::effects::ScopedInviteSink {
        inner: deps.effects,
        invite: ledger.clone(),
        failure: Some(grant_failure.unwrap_or_else(|| {
            crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RemoveLedger(
                ledger,
            )
        })),
    };
    let mut delivery_deps = deps.clone();
    delivery_deps.effects = &delivery_sink;
    // Shared delivery path (#1248/#1264): sends to every connected
    // resource and falls back to the durable pending-delivery queue
    // when the invitee is offline OR every listed session refused the
    // write — a stale resource list must not lose the invitation.
    if let Err(error) =
        super::muc_invite::deliver_muc_user_message(state, &invitee, invite, &delivery_deps).await
    {
        warn!(
            invitee = %invitee,
            error = %error,
            "Group-DM invite could not be delivered or queued; rolling back member grant"
        );
        let error_kind = match error {
            super::muc_invite::MucUserDeliveryError::QuotaExceeded => {
                GroupDmInviteError::ServiceUnavailable
            }
            super::muc_invite::MucUserDeliveryError::Storage(_)
            | super::muc_invite::MucUserDeliveryError::Unavailable => {
                GroupDmInviteError::InternalServerError
            }
        };
        return Some(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            error_kind,
            "Internal server error.",
        )]);
    }

    deps.capture_intent(IngressEffectIntent::GroupDmMembershipGrant {
        grant: grant.clone(),
    });
    deps.capture_intent(IngressEffectIntent::GroupDmInviteLedger { grant });

    Some(vec![])
}

#[derive(Clone, Debug)]
pub struct GroupDmMembershipMutation {
    pub grant: GroupDmMembershipGrant,
    pub incoming: xmpp_parsers::message::Message,
    pub sender: jid::FullJid,
    pub actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
}

pub(crate) async fn execute_group_dm_membership(
    mutation: GroupDmMembershipMutation,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let incoming = &mutation.incoming;
    let bound_jid = &mutation.sender;
    let room_jid = mutation.grant.room.clone();
    let invitee = mutation.grant.invitee.clone();
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return EffectOutcome::Unavailable;
    };
    let mut room_actor = mutation.actor;
    let visible_after = match mutation.grant.history_visibility {
        GroupDmHistoryVisibility::Full => None,
        GroupDmHistoryVisibility::FromJoin { visible_after } => Some(visible_after),
    };
    let Ok(snapshot) = room_actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return EffectOutcome::Frames(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    };
    if snapshot.room.get_affiliation(&invitee) >= waddle_xmpp::Affiliation::Member {
        return EffectOutcome::Membership(
            crate::server::routes::interpret::effects::MembershipOutcome::Preserved,
        );
    }
    if record_group_dm_archive_boundary(state, &room_jid, &invitee, visible_after)
        .await
        .is_err()
    {
        return EffectOutcome::Frames(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    }
    if crate::admin::channels::persist_group_dm_member_tuple(
        &state.deps.app_state,
        &channel_id,
        &invitee,
    )
    .await
    .is_err()
    {
        let _ = delete_group_dm_archive_boundary(state, &room_jid, &invitee).await;
        return EffectOutcome::Frames(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    }
    match room_actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
    {
        Ok(()) => {}
        Err(error) => {
            if matches!(
                error,
                kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::AffiliationMutationError::CommitOutcomeUnknown
                )
            ) {
                let Some(recovered_actor) =
                    super::muc_invite::recover_actor_after_ambiguous_invite_grant(
                        state,
                        &room_jid,
                        &room_actor,
                    )
                    .await
                else {
                    warn!(
                        room = %room_jid,
                        invitee = %invitee,
                        "group-DM invite grant remains inconclusive; retaining coupled effects for reconciliation"
                    );
                    return EffectOutcome::Frames(vec![error_reply(
                        incoming,
                        bound_jid,
                        deps,
                        GroupDmInviteError::InternalServerError,
                        "Internal server error.",
                    )]);
                };
                let committed = recovered_actor
                    .ask(GetSnapshot)
                    .reply_timeout(std::time::Duration::from_secs(5))
                    .await
                    .is_ok_and(|snapshot| {
                        snapshot.room.get_affiliation(&invitee) >= waddle_xmpp::Affiliation::Member
                    });
                if !committed {
                    rollback_group_dm_invite_grant(
                        state,
                        recovered_actor,
                        &channel_id,
                        &room_jid,
                        &invitee,
                        &bound_jid.to_bare(),
                    )
                    .await;
                    return EffectOutcome::Frames(vec![error_reply(
                        incoming,
                        bound_jid,
                        deps,
                        GroupDmInviteError::InternalServerError,
                        "Internal server error.",
                    )]);
                }
                room_actor = recovered_actor;
                warn!(
                    room = %room_jid,
                    invitee = %invitee,
                    "reconciled committed group-DM invite grant after ambiguous outcome"
                );
            } else if super::muc_invite::should_compensate_failed_affiliation_grant(&error) {
                let _ = delete_group_dm_archive_boundary(state, &room_jid, &invitee).await;
                crate::admin::channels::rollback_group_dm_member_tuple(
                    &state.deps.app_state,
                    &channel_id,
                    &invitee,
                )
                .await;
                return EffectOutcome::Frames(vec![error_reply(
                    incoming,
                    bound_jid,
                    deps,
                    GroupDmInviteError::InternalServerError,
                    "Internal server error.",
                )]);
            } else {
                return EffectOutcome::Frames(vec![error_reply(
                    incoming,
                    bound_jid,
                    deps,
                    GroupDmInviteError::InternalServerError,
                    "Internal server error.",
                )]);
            }
        }
    }
    let room_name = match room_actor
        .ask(GetConfig)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    {
        Ok(config) => config.name,
        Err(_) => room_jid.to_string(),
    };
    let shared_room_name = {
        let trimmed = room_name.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    };
    if crate::admin::channels::publish_group_dm_bookmark(
        &state.deps.app_state,
        &invitee,
        &room_jid,
        shared_room_name,
    )
    .await
    .is_err()
    {
        rollback_group_dm_invite_grant(
            state,
            room_actor,
            &channel_id,
            &room_jid,
            &invitee,
            &bound_jid.to_bare(),
        )
        .await;
        return EffectOutcome::Frames(vec![error_reply(
            incoming,
            bound_jid,
            deps,
            GroupDmInviteError::InternalServerError,
            "Internal server error.",
        )]);
    }

    EffectOutcome::Membership(
        crate::server::routes::interpret::effects::MembershipOutcome::Granted {
            previous_affiliation: snapshot.room.get_affiliation(&invitee),
        },
    )
}

pub(crate) async fn rollback_group_dm_membership(
    grant: &GroupDmMembershipMutation,
    deps: &crate::server::routes::interpret::Deps<'_>,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&grant.grant.room) else {
        return;
    };
    let Ok(Some(actor)) = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: grant.grant.room.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return;
    };
    let Ok(snapshot) = actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return;
    };
    if snapshot.room.get_affiliation(&grant.grant.invitee) != waddle_xmpp::Affiliation::Member {
        return;
    }
    rollback_group_dm_invite_grant(
        state,
        actor,
        &channel_id,
        &grant.grant.room,
        &grant.grant.invitee,
        &grant.grant.inviter,
    )
    .await;
}

/// Undo a partially granted group-DM invite.
///
/// Ordering is load-bearing: the channel-member permission tuple MUST
/// be deleted before `ChangeAffiliation(None)` reaches the room actor,
/// because the `None`-affiliation handler re-hydrates the durable
/// recipient mirror from the permission tuples — a still-existing
/// tuple would resurrect the rolled-back invitee as a durable inbox
/// recipient (content leak to a user who was never told they are in
/// the room). Every other removal path (`run_group_dm_leave`, the
/// admin kick/affiliation paths) orders tuple-delete first for the
/// same reason.
async fn rollback_group_dm_invite_grant(
    state: &WebSocketState,
    room_actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    channel_id: &str,
    room_jid: &jid::BareJid,
    invitee: &jid::BareJid,
    inviter: &jid::BareJid,
) {
    crate::admin::channels::rollback_group_dm_member_tuple(
        &state.deps.app_state,
        channel_id,
        invitee,
    )
    .await;
    let _ = delete_group_dm_archive_boundary(state, room_jid, invitee).await;
    // #1264: a rolled-back invite is not declinable — drop exactly the
    // ledger row this flow recorded (harmless no-op on paths that
    // failed before recording it); another inviter's outstanding
    // invitation is left alone.
    let _ = crate::server::routes::websocket::muc_invites::claim_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: room_jid.clone(),
            invitee: invitee.clone(),
            inviter: inviter.clone(),
        },
    )
    .await;
    let _ =
        crate::admin::channels::retract_group_dm_bookmark(&state.deps.app_state, invitee, room_jid)
            .await;
    let _ = room_actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: waddle_xmpp::Affiliation::None,
        })
        .await;
}

async fn record_group_dm_archive_boundary(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    member_jid: &jid::BareJid,
    visible_after: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(), String> {
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    actor
        .ask(crate::db::actor::DbExecute {
            sql: "INSERT INTO group_dm_archive_boundaries (room_jid, member_jid, visible_after, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(room_jid, member_jid) DO UPDATE SET visible_after = excluded.visible_after, updated_at = excluded.updated_at".to_string(),
            params: vec![
                room_jid.to_string().into(),
                member_jid.to_string().into(),
                visible_after.map(|timestamp| timestamp.to_rfc3339()).into(),
                chrono::Utc::now().to_rfc3339().into(),
            ],
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress_shadow::IngressEffectCapture;
    use kameo::actor::Spawn;
    use std::sync::Arc;
    use waddle_xmpp::muc::room_actor::{GetRoomSnapshot, HydrateDurableRecipients, RoomActor};
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::{MucRoom, RoomConfig};
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

    fn group_dm_invite_message(
        room_jid: &jid::BareJid,
        sender: &jid::FullJid,
        invitee: &str,
    ) -> xmpp_parsers::message::Message {
        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Normal;
        message.from = Some(jid::Jid::from(sender.clone()));
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(minidom::rxml::xml_ncname!("to").to_owned(), invitee)
                        .build(),
                )
                .build(),
        );
        message
    }

    async fn create_group_dm_room(
        state: &WebSocketState,
        room_jid: &jid::BareJid,
        channel_id: &str,
    ) -> kameo::actor::ActorRef<RoomActor> {
        crate::server::xmpp_channels::upsert_xmpp_channel(
            state.deps.app_state.db_pool.global_actor().clone(),
            &crate::server::xmpp_channels::XmppChannelUpsert {
                id: channel_id.to_string(),
                name: "Invite Test".to_string(),
                description: None,
                channel_type: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
                position: 0,
                is_default: false,
                pin_permission: Default::default(),
                members_only: true,
                public_room: false,
            },
        )
        .await
        .expect("seed group-DM channel row");
        state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
                channel_id: channel_id.to_string(),
                config: RoomConfig {
                    group_dm: true,
                    persistent: true,
                    members_only: true,
                    public_room: false,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("create room")
    }

    async fn durable_recipients(
        room_actor: &kameo::actor::ActorRef<RoomActor>,
    ) -> Vec<jid::BareJid> {
        room_actor
            .ask(GetRoomSnapshot {
                sender_jid: "observer@example.com/res".parse().expect("observer jid"),
            })
            .await
            .expect("room snapshot")
            .durable_recipient_bare_jids
    }

    /// Rolled-back invitee must not survive in the durable-recipient
    /// mirror: `rollback_group_dm_invite_grant` has to delete the
    /// channel-member permission tuple BEFORE asking the room actor to
    /// change the affiliation to `None`, because the `None`-affiliation
    /// handler re-hydrates the mirror from the permission tuples. In
    /// the reverse order the re-hydration reads the still-existing
    /// tuple and resurrects the invitee, who then receives inbox rows
    /// with full message bodies until the actor respawns — despite
    /// never being told they are in the room.
    #[tokio::test]
    async fn rollback_prunes_invitee_from_durable_recipient_mirror() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let channel_id = "gdm-rollback-order";
        let room_jid: jid::BareJid = format!("{channel_id}@muc.example.com")
            .parse()
            .expect("room jid");
        let invitee: jid::BareJid = "invitee@example.com".parse().expect("invitee jid");

        // Invite path already persisted the member tuple.
        crate::admin::channels::persist_group_dm_member_tuple(
            &state.deps.app_state,
            channel_id,
            &invitee,
        )
        .await
        .expect("persist group-DM member tuple");

        // Spawn the room actor hydrated from the SAME permission store
        // the rollback mutates, exactly like the registry does in
        // production.
        let room_actor = RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                room_jid.clone(),
                "waddle-1".to_string(),
                channel_id.to_string(),
                RoomConfig::default(),
            ),
            waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
                b"test-occupant-id-secret-32-bytes-long".to_vec(),
            )
            .expect("test secret meets length floor"),
        ));
        let source = std::sync::Arc::new(
            crate::server::durable_membership::PermissionDurableMembershipSource::new(
                state.deps.app_state.permission_actor.clone(),
            ),
        );
        room_actor
            .ask(HydrateDurableRecipients { source })
            .await
            .expect("hydrate durable recipients");
        assert!(
            durable_recipients(&room_actor).await.contains(&invitee),
            "precondition: the granted invitee hydrates into the mirror"
        );

        let inviter: jid::BareJid = "inviter@example.com".parse().expect("inviter jid");
        rollback_group_dm_invite_grant(
            &state,
            room_actor.clone(),
            channel_id,
            &room_jid,
            &invitee,
            &inviter,
        )
        .await;

        assert!(
            !durable_recipients(&room_actor).await.contains(&invitee),
            "a rolled-back invitee must not remain a durable inbox \
             recipient (tuple delete must precede ChangeAffiliation(None))"
        );
    }

    #[tokio::test]
    async fn non_member_group_dm_invite_records_error_reply_intent() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let capture = IngressEffectCapture::new(None);
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let room_jid: jid::BareJid = "group-dm-invite@muc.example.com".parse().expect("room jid");
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        create_group_dm_room(state.as_ref(), &room_jid, "group-dm-invite").await;

        let message = group_dm_invite_message(&room_jid, &sender, "bob@example.com");

        let frames =
            handle_group_dm_mediated_invite(&message, state.as_ref(), &sender, None, &deps)
                .await
                .expect("handler should reply");

        assert_eq!(frames.len(), 1);
        assert!(
            matches!(&frames[0], waddle_xmpp::Stanza::Message(message) if message.type_ == xmpp_parsers::message::MessageType::Error)
        );
        let expected_error = FrozenStanzaError::from_xmpp(&StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Only group-DM members may invite people.",
        ))
        .expect("server-built stanza error should freeze");
        assert!(capture
            .snapshot()
            .intents
            .contains(&IngressEffectIntent::ErrorReply {
                recipient: sender,
                error: expected_error,
            }));
    }

    #[tokio::test]
    async fn successful_group_dm_invite_records_membership_and_ledger_intents() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let capture = IngressEffectCapture::new(None);
        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let room_jid: jid::BareJid = "group-dm-success@muc.example.com"
            .parse()
            .expect("room jid");
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
        let room_actor = create_group_dm_room(state.as_ref(), &room_jid, "group-dm-success").await;
        crate::server::routes::websocket::tests::seed_local_account(state.as_ref(), "bob").await;
        room_actor
            .ask(ChangeAffiliation {
                jid: sender.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("grant inviter membership");
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");

        let response = handle_group_dm_mediated_invite(
            &group_dm_invite_message(&room_jid, &sender, "bob@example.com"),
            state.as_ref(),
            &sender,
            Some(&session),
            &deps,
        )
        .await
        .expect("handler should consume invite");

        assert!(
            response.is_empty(),
            "successful invite should not emit an error frame"
        );
        let snapshot = capture.snapshot();
        let granted = snapshot
            .intents
            .iter()
            .find_map(|intent| match intent {
                IngressEffectIntent::GroupDmMembershipGrant { grant } => Some(grant.clone()),
                _ => None,
            })
            .expect("successful invite must capture a membership grant");
        assert_eq!(granted.room, room_jid);
        assert_eq!(granted.invitee, invitee);
        assert_eq!(granted.inviter, sender.to_bare());
        assert!(
            snapshot
                .intents
                .contains(&IngressEffectIntent::GroupDmInviteLedger {
                    grant: granted.clone(),
                }),
            "the invite ledger must record the same committed grant (including its history visibility)"
        );
    }

    struct ConcurrentMemberThenFailure {
        fail_ledger: bool,
    }

    impl crate::server::routes::interpret::effects::EffectSink for ConcurrentMemberThenFailure {
        fn execute<'a>(
            &'a self,
            effect: PlannedEffect,
            deps: &'a crate::server::routes::interpret::Deps<'_>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EffectOutcome> + Send + 'a>>
        {
            Box::pin(async move {
                use crate::server::routes::interpret::effects::{
                    early::RoomMembershipMutation, ImmediateSink,
                };
                if let Effect::External(ExternalEffect::RoomMembershipMutation(
                    RoomMembershipMutation::GroupDm(grant),
                )) = &effect.effect
                {
                    grant
                        .actor
                        .ask(ChangeAffiliation {
                            jid: grant.grant.invitee.clone(),
                            affiliation: waddle_xmpp::Affiliation::Member,
                        })
                        .await
                        .expect("independent membership grant");
                }
                match effect.effect {
                    Effect::External(ExternalEffect::InviteLedger(
                        super::super::muc_invite::InviteLedgerMutation::Record { failure, .. },
                    )) if self.fail_ledger => {
                        if let Some(failure) = failure {
                            crate::server::routes::interpret::effects::invite::compensate(
                                *failure, deps,
                            )
                            .await;
                        }
                        EffectOutcome::InviteLedger(Err(
                            super::super::muc_invite::InviteLedgerError::Storage,
                        ))
                    }
                    Effect::External(
                        ExternalEffect::RouteToPeer(route)
                        | ExternalEffect::QueueOfflineDelivery(route),
                    ) if !self.fail_ledger => {
                        if let Some(failure) = route.failure {
                            crate::server::routes::interpret::effects::invite::compensate(
                                *failure, deps,
                            )
                            .await;
                        }
                        EffectOutcome::MucUserDelivery(Err(
                            super::super::muc_invite::MucUserDeliveryError::QuotaExceeded,
                        ))
                    }
                    _ => ImmediateSink.execute(effect, deps).await,
                }
            })
        }
        fn is_planning(&self) -> bool {
            false
        }
        fn record(&self, _effect: PlannedEffect) {}
        fn set_room_execution(
            &self,
            _execution: crate::server::routes::interpret::effects::RoomExecutionPath,
        ) {
        }
    }

    #[tokio::test]
    async fn concurrent_group_dm_membership_survives_ledger_and_delivery_failures() {
        for fail_ledger in [true, false] {
            let state =
                crate::server::routes::websocket::tests::create_test_websocket_state().await;
            let room: jid::BareJid = "group-dm-concurrent@muc.example.com".parse().expect("room");
            let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
            let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
            let actor = create_group_dm_room(&state, &room, "group-dm-concurrent").await;
            crate::server::routes::websocket::tests::seed_local_account(&state, "bob").await;
            actor
                .ask(ChangeAffiliation {
                    jid: sender.to_bare(),
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("inviter");
            let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
            let sink = ConcurrentMemberThenFailure { fail_ledger };
            let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
                &state, None,
            );
            deps.effects = &sink;
            let frames = handle_group_dm_mediated_invite(
                &group_dm_invite_message(&room, &sender, "bob@example.com"),
                &state,
                &sender,
                Some(&session),
                &deps,
            )
            .await
            .expect("consumed");
            assert_eq!(frames.len(), 1);
            assert_eq!(
                actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot")
                    .room
                    .get_affiliation(&invitee),
                waddle_xmpp::Affiliation::Member
            );
            assert!(crate::server::routes::websocket::muc_invites::list_invites(
                state.deps.app_state.db_pool.global_actor().clone(),
                &room,
                &invitee
            )
            .await
            .expect("ledger")
            .is_empty());
        }
    }

    #[tokio::test]
    async fn executed_group_dm_grant_never_demotes_existing_admin() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let room: jid::BareJid = "group-dm-admin@muc.example.com".parse().expect("room");
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
        let actor = create_group_dm_room(&state, &room, "group-dm-admin").await;
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: waddle_xmpp::Affiliation::Admin,
            })
            .await
            .expect("admin membership");
        let deps =
            crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
        let mutation = GroupDmMembershipMutation {
            grant: GroupDmMembershipGrant {
                room: room.clone(),
                invitee: invitee.clone(),
                inviter: sender.to_bare(),
                history_visibility: GroupDmHistoryVisibility::Full,
            },
            incoming: group_dm_invite_message(&room, &sender, "bob@example.com"),
            sender,
            actor: actor.clone(),
        };
        let result = execute_group_dm_membership(mutation.clone(), &deps).await;
        assert!(matches!(
            result,
            EffectOutcome::Membership(
                crate::server::routes::interpret::effects::MembershipOutcome::Preserved
            )
        ));
        rollback_group_dm_membership(&mutation, &deps).await;
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .get_affiliation(&invitee),
            waddle_xmpp::Affiliation::Admin
        );
    }

    #[tokio::test]
    async fn group_dm_invite_plan_records_effects_without_membership_or_ledger_writes() {
        use crate::server::routes::interpret::effects::{PlanSink, PlanSuppressionPolicy};
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let room: jid::BareJid = "group-dm-plan@muc.example.com".parse().expect("room");
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
        let actor = create_group_dm_room(&state, &room, "group-dm-plan").await;
        crate::server::routes::websocket::tests::seed_local_account(&state, "bob").await;
        actor
            .ask(ChangeAffiliation {
                jid: sender.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("inviter membership");
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
        let resource: jid::FullJid = "bob@example.com/phone".parse().expect("resource");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        crate::server::routes::websocket::tests::register_test_connection(&state, &resource, tx)
            .await;
        let sink = PlanSink::new();
        let mut deps =
            crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
        deps.effects = &sink;
        let result = handle_group_dm_mediated_invite(
            &group_dm_invite_message(&room, &sender, "bob@example.com"),
            &state,
            &sender,
            Some(&session),
            &deps,
        )
        .await
        .expect("consumed");
        assert!(result.is_empty());
        assert!(rx.try_recv().is_err());
        let plan = sink.snapshot();
        let membership = plan
            .iter()
            .find(|effect| {
                matches!(
                    effect.effect,
                    Effect::External(ExternalEffect::RoomMembershipMutation(_))
                )
            })
            .expect("membership effect");
        assert_eq!(membership.suppression, PlanSuppressionPolicy::Always);
        let ledger = plan
            .iter()
            .find(|effect| {
                matches!(
                    effect.effect,
                    Effect::External(ExternalEffect::InviteLedger(_))
                )
            })
            .expect("ledger effect");
        assert!(ledger.dependencies.contains(
            &crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomMembership {
                room: room.clone(),
                member: invitee.clone()
            }
        ));
        let route = plan
            .iter()
            .find(|effect| {
                matches!(
                    effect.effect,
                    Effect::External(ExternalEffect::RouteToPeer(_))
                )
            })
            .expect("route effect");
        assert!(route.dependencies.contains(
            &crate::server::routes::interpret::effects::PlanEffectDependency::AfterInviteLedger {
                invite: crate::server::routes::websocket::muc_invites::OutstandingInvite {
                    room: room.clone(),
                    invitee: invitee.clone(),
                    inviter: sender.to_bare()
                }
            }
        ));
        assert!(
            matches!(&route.effect, Effect::External(ExternalEffect::RouteToPeer(route)) if route.failure.is_some())
        );
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .get_affiliation(&invitee),
            waddle_xmpp::Affiliation::None
        );
        assert!(crate::server::routes::websocket::muc_invites::list_invites(
            state.deps.app_state.db_pool.global_actor().clone(),
            &room,
            &invitee
        )
        .await
        .expect("ledger")
        .is_empty());
        assert!(group_dm_archive_boundary(&state, &room, &invitee)
            .await
            .expect("boundary")
            .is_none());
    }

    #[tokio::test]
    async fn blocklist_lookup_failure_sets_operational_plan_rejection() {
        struct FailingBlocking;

        #[derive(Debug, thiserror::Error)]
        #[error("synthetic blocking lookup failure")]
        struct FailingBlockingError;

        #[async_trait::async_trait]
        impl BlockingStorage for FailingBlocking {
            async fn list_blocked_jids(
                &self,
                _user_jid: &jid::BareJid,
            ) -> Result<Vec<jid::BareJid>, BlockingStorageError> {
                Err(BlockingStorageError::new(FailingBlockingError))
            }

            async fn list_blocked_jid_entries(
                &self,
                _user_jid: &jid::BareJid,
            ) -> Result<Vec<jid::Jid>, BlockingStorageError> {
                Err(BlockingStorageError::new(FailingBlockingError))
            }
        }

        let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
        let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let blocking: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
        let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_pending_and_blocking(
            sm_registry,
            pending,
            blocking,
        )
        .await;
        let capture = IngressEffectCapture::new(None);
        let sink = crate::server::routes::interpret::effects::PlanSink::new();
        let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        deps.effects = &sink;
        let room_jid: jid::BareJid = "group-dm-blocking@muc.example.com"
            .parse()
            .expect("room jid");
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let room_actor = create_group_dm_room(state.as_ref(), &room_jid, "group-dm-blocking").await;
        crate::server::routes::websocket::tests::seed_local_account(state.as_ref(), "bob").await;
        room_actor
            .ask(ChangeAffiliation {
                jid: sender.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("grant inviter membership");
        let session = crate::auth::Session::new("alice@example.com", "alice", "alice");

        let response = handle_group_dm_mediated_invite(
            &group_dm_invite_message(&room_jid, &sender, "bob@example.com"),
            state.as_ref(),
            &sender,
            Some(&session),
            &deps,
        )
        .await
        .expect("handler should consume invite");

        assert!(
            response.is_empty(),
            "blocklist outage should fail closed without leaking an auth outcome"
        );
        assert!(capture.snapshot().markers.is_empty());
        assert_eq!(
            deps.effects.rejection(),
            Some(PlanRejection::PolicyDenied(
                PolicyDeniedReason::OperationalFenceLoss
            ))
        );
        assert!(sink.snapshot().is_empty());
    }
}

async fn group_dm_archive_boundary(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    member_jid: &jid::BareJid,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let row = actor
        .ask(crate::db::actor::DbQueryOne {
            sql: "SELECT visible_after FROM group_dm_archive_boundaries WHERE room_jid = ? AND member_jid = ?"
                .to_string(),
            params: vec![room_jid.to_string().into(), member_jid.to_string().into()],
        })
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = row else {
        return Ok(None);
    };
    let visible_after = crate::db::row_value(&row, 0)
        .map_err(|error| error.to_string())?
        .as_optional_string()
        .map_err(|error| error.to_string())?;
    match visible_after {
        Some(value) => chrono::DateTime::parse_from_rfc3339(&value)
            .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

async fn delete_group_dm_archive_boundary(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    member_jid: &jid::BareJid,
) -> Result<(), String> {
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    actor
        .ask(crate::db::actor::DbExecute {
            sql: "DELETE FROM group_dm_archive_boundaries WHERE room_jid = ? AND member_jid = ?"
                .to_string(),
            params: vec![room_jid.to_string().into(), member_jid.to_string().into()],
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn build_server_mediated_invite_payload(
    inviter: &jid::BareJid,
    invitee: &jid::BareJid,
    inbound_invite: &minidom::Element,
    access: waddle_xmpp::xep::xep_waddle_group_dm::GroupDmHistoryAccess,
) -> minidom::Element {
    let mut invite = minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            inviter.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            invitee.to_string(),
        );
    if let Some(reason) =
        inbound_invite.get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
    {
        invite = invite.append(reason.clone());
    }
    invite = invite.append(waddle_xmpp::xep::xep_waddle_group_dm::build_history_access(
        access,
    ));
    minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(invite.build())
        .build()
}

#[derive(Clone, Copy)]
enum GroupDmInviteError {
    NotAuthorized,
    Forbidden,
    Conflict,
    ItemNotFound,
    InternalServerError,
    ServiceUnavailable,
}

impl GroupDmInviteError {
    fn stanza_error(self, text: &str) -> StanzaError {
        let (error_type, condition) = match self {
            Self::NotAuthorized => (ErrorType::Auth, DefinedCondition::NotAuthorized),
            Self::Forbidden => (ErrorType::Auth, DefinedCondition::Forbidden),
            Self::Conflict => (ErrorType::Cancel, DefinedCondition::Conflict),
            Self::ItemNotFound => (ErrorType::Cancel, DefinedCondition::ItemNotFound),
            Self::InternalServerError => (ErrorType::Wait, DefinedCondition::InternalServerError),
            Self::ServiceUnavailable => (ErrorType::Cancel, DefinedCondition::ServiceUnavailable),
        };
        StanzaError::new(error_type, condition, "en", text)
    }
}

fn error_reply(
    incoming: &xmpp_parsers::message::Message,
    bound_jid: &jid::FullJid,
    deps: &crate::server::routes::interpret::Deps<'_>,
    kind: GroupDmInviteError,
    text: &str,
) -> waddle_xmpp::Stanza {
    let stanza_error = kind.stanza_error(text);
    deps.effects
        .set_rejection(super::classify_rejection(&stanza_error));
    let frozen_error = FrozenStanzaError::from_xmpp(&stanza_error)
        .expect("server-built stanza error should freeze");
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    deps.capture_intent(IngressEffectIntent::ErrorReply {
        recipient: bound_jid.clone(),
        error: frozen_error,
    });
    waddle_xmpp::Stanza::Message(message_error_reply(&stamped, stanza_error))
}

fn xmpp_error_reply(
    incoming: &xmpp_parsers::message::Message,
    bound_jid: &jid::FullJid,
    deps: &crate::server::routes::interpret::Deps<'_>,
    error: waddle_xmpp::XmppError,
) -> waddle_xmpp::Stanza {
    let stanza_error = match error {
        waddle_xmpp::XmppError::Stanza {
            condition,
            error_type,
            text,
        } => stanza_error_from_waddle_parts(error_type, condition, text),
        other => {
            warn!(
                error = %other,
                "group-DM invite validation failed with non-stanza error"
            );
            stanza_error_from_waddle_parts(
                waddle_xmpp::StanzaErrorType::Wait,
                waddle_xmpp::StanzaErrorCondition::InternalServerError,
                Some("Internal server error.".to_string()),
            )
        }
    };
    deps.effects
        .set_rejection(super::classify_rejection(&stanza_error));
    let frozen_error = FrozenStanzaError::from_xmpp(&stanza_error)
        .expect("server-built stanza error should freeze");
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    deps.capture_intent(IngressEffectIntent::ErrorReply {
        recipient: bound_jid.clone(),
        error: frozen_error,
    });
    waddle_xmpp::Stanza::Message(message_error_reply(&stamped, stanza_error))
}

fn stanza_error_from_waddle_parts(
    error_type: waddle_xmpp::StanzaErrorType,
    condition: waddle_xmpp::StanzaErrorCondition,
    text: Option<String>,
) -> StanzaError {
    match text {
        Some(text) => StanzaError::new(error_type.to_xmpp(), condition.to_xmpp(), "en", text),
        None => StanzaError {
            type_: error_type.to_xmpp(),
            by: None,
            defined_condition: condition.to_xmpp(),
            texts: BTreeMap::new(),
            other: None,
        },
    }
}
