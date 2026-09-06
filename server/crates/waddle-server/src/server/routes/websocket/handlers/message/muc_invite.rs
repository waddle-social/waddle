//! XEP-0045 §7.8 mediated invitations for regular MUC rooms (#1248).
//!
//! Group DMs keep their dedicated flow (membership grant, archive
//! boundary, bookmark push — [`super::group_dm_invite`]); this module
//! implements the standards flow for every other room:
//!
//! 1. The inviter (a current occupant) sends
//!    `<message to='room'><x xmlns='muc#user'><invite to='invitee'/></x></message>`.
//! 2. The room stamps `from` on the `<invite/>` with the inviter's
//!    bare JID and relays it to the invitee **from the room's bare
//!    JID** (§7.8.2).
//! 3. Members-only rooms restrict invitations to admins/owners
//!    (§7.8.2 note → `<forbidden/>`) and auto-add the invitee to the
//!    member list so the invitation is actually usable.
//! 4. A nonexistent invitee yields `<item-not-found/>` (§7.8.2).
//! 5. Offline invitees get a durable pending-delivery row instead of a
//!    silent drop.
//!
//! Every relayed invite records an outstanding-invite ledger row
//! (#1264) so a later `<decline/>` can be verified and routed to the
//! actual inviter.

use tracing::warn;
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, MucInviteLedgerAction, MucInviteLedgerMutation,
        MucInviteMembershipGrant,
    },
    muc::room_actor::{AffiliationMutationError, ChangeAffiliation, GetSnapshot},
    muc::room_registry_actor::{GetOrCreateRoom, GetRoom},
    pending_delivery::{PendingPayload, PendingRow, PendingRowId},
    protocol::handlers::errors::message_error_reply,
    Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::auth::Session;
use crate::ingress_shadow::IngressShadowRoomFence;
use crate::server::routes::websocket::muc_invites::{
    claim_invite, record_invite_at, OutstandingInvite, RecordOutcome,
};
use crate::server::routes::websocket::WebSocketState;

use crate::server::routes::interpret::{
    effects::{Effect, EffectOutcome, ExternalEffect, MembershipOutcome, PlannedEffect},
    Deps,
};

#[derive(Clone, Debug)]
pub enum InviteLedgerMutation {
    Record {
        invite: OutstandingInvite,
        recorded_at: chrono::DateTime<chrono::Utc>,
        failure:
            Option<Box<crate::server::routes::interpret::effects::invite::InviteDeliveryFailure>>,
    },
    Claim {
        invite: OutstandingInvite,
    },
}
#[derive(Clone, Debug)]
pub enum InviteLedgerOutcome {
    Recorded(RecordOutcome),
    Claimed(bool),
}
#[derive(Debug, thiserror::Error)]
pub enum InviteLedgerError {
    #[error("invitation ledger storage failed")]
    Storage,
}
pub(crate) async fn execute_invite_ledger(
    mutation: InviteLedgerMutation,
    deps: &Deps<'_>,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let result = match mutation {
        InviteLedgerMutation::Record {
            invite,
            recorded_at,
            failure,
        } => {
            let result = record_invite_at(actor, &invite, recorded_at).await;
            if result.is_err() {
                if let Some(failure) = failure {
                    crate::server::routes::interpret::effects::invite::compensate(*failure, deps)
                        .await;
                }
            }
            result.map(InviteLedgerOutcome::Recorded)
        }
        InviteLedgerMutation::Claim { invite } => claim_invite(actor, &invite)
            .await
            .map(InviteLedgerOutcome::Claimed),
    }
    .map_err(|error| {
        warn!(%error, "Invitation ledger mutation failed");
        InviteLedgerError::Storage
    });
    EffectOutcome::InviteLedger(result)
}
#[derive(Clone, Debug)]
pub struct MucMembershipMutation {
    pub room: jid::BareJid,
    pub invitee: jid::BareJid,
    pub actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    pub previous_affiliation: waddle_xmpp::Affiliation,
}

pub(super) async fn recover_actor_after_ambiguous_invite_grant(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    stale_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
) -> Option<kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>> {
    // Snapshot the stale actor NOW (it is sealed by the ambiguous commit):
    // a caller's pre-grant copy would roll back joins/leaves that projected
    // through the actor since.
    let snapshot = match tokio::time::timeout(
        crate::server::routes::websocket::LEAVE_ASK_TIMEOUT,
        stale_actor.ask(waddle_xmpp::muc::room_actor::GetSnapshot),
    )
    .await
    {
        Ok(Ok(snapshot)) => snapshot,
        // The sealed stale actor is already gone (reaped by a concurrent
        // join's recreation) or unresponsive: there is nothing to
        // transplant. Follow the registry's CURRENT successor instead of
        // abandoning recovery — the durable membership already committed,
        // and giving up here would strand the invite's remaining effects
        // behind an unretryable "already a member" conflict (#1647, codex
        // round 27).
        _ => {
            let current = state
                .deps
                .protocol
                .room_registry
                .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                    room_jid: room_jid.clone(),
                })
                .await
                .ok()
                .flatten()?;
            if current.id() == stale_actor.id() {
                return None;
            }
            return Some(current);
        }
    };
    let snapshot = &snapshot;
    // Demote the exact stale actor and publish the successor in ONE registry
    // turn (no observable "room absent" gap). A post-publication
    // `RestoreLiveRoster` would erase joins/leaves that already projected on a
    // live successor; if the stale actor was already replaced, the live
    // successor is authoritative and gets no transplant.
    let handoff = state
        .deps
        .protocol
        .room_registry
        .ask(
            waddle_xmpp::muc::room_registry_actor::GetOrCreateRoomWithLiveRoster {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                    snapshot.room.waddle_id.clone(),
                ),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(
                    snapshot.room.channel_id.clone(),
                ),
                config: snapshot.room.config.clone(),
                live_room_restore: snapshot.room.clone(),
                occupancy_revision: snapshot.occupancy_revision,
                departures: snapshot.departures.clone(),
                demote_first: Some(stale_actor.clone()),
            },
        )
        .await;
    let stale_not_current = matches!(
        handoff,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_registry_actor::RoomRegistryError::StaleActorNotCurrent(_),
        ))
    );
    let recovered = if !stale_not_current {
        handoff.ok().map(|acquisition| acquisition.actor_ref)?
    } else {
        // Demotion refused for one of two reasons, both of which make the
        // stale snapshot non-authoritative: a successor is already live (its
        // roster is the truth), or the registry already retired the entry
        // after a definitive ownership loss (the durable restore rehydrates
        // config/subject/affiliations and occupants re-join, exactly as for
        // any retired actor). Neither case transplants.
        state
            .deps
            .protocol
            .room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: snapshot.room.waddle_id.clone(),
                channel_id: snapshot.room.channel_id.clone(),
                config: snapshot.room.config.clone(),
            })
            .await
            .ok()
            .map(|acquisition| acquisition.actor_ref)?
    };
    Some(recovered)
}

/// Handle a mediated invitation to a non-group-DM room. Returns `None`
/// when the stanza is not a mediated invite for a room on the MUC
/// domain (the caller falls through to the next handler); group-DM
/// invites are consumed earlier by
/// [`super::group_dm_invite::handle_group_dm_mediated_invite`].
pub(super) async fn handle_muc_mediated_invite(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
    authenticated_session: Option<&Session>,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> Option<Vec<Stanza>> {
    let ingress_effect_capture = deps.ingress_effect_capture.as_ref();
    if incoming.type_ != MessageType::Normal {
        return None;
    }
    let room_jid = incoming.to.as_ref()?.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    let (invitee, inbound_invite) = mediated_invitee(incoming)?;

    if authenticated_session.is_none() {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Auth,
            DefinedCondition::NotAuthorized,
            "Authentication required.",
        )]);
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
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };
    if let (Some(capture), Some(claim_fence)) =
        (ingress_effect_capture, snapshot.claim_fence.as_ref())
    {
        capture.record_room_fence(IngressShadowRoomFence::from_context(&room_jid, claim_fence));
    }
    // XEP-0045 §7.8: a mediated invitation is an occupant action ("a
    // room in which one is an occupant").
    if snapshot.room.find_nick_by_real_jid(bound_jid).is_none() {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "Only room occupants may send invitations.",
        )]);
    }
    let inviter_bare = bound_jid.to_bare();
    let inviter_affiliation = snapshot.room.get_affiliation(&inviter_bare);
    // XEP-0045 §7.8.2 note: "Invitation privileges in members-only
    // rooms SHOULD be restricted to room admins; if a member without
    // privileges to edit the member list attempts to invite another
    // user, the service SHOULD return a <forbidden/> error."
    if snapshot.room.config.members_only && inviter_affiliation < waddle_xmpp::Affiliation::Admin {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "Only room admins may invite people to a members-only room.",
        )]);
    }

    // XEP-0045 §7.8.2: "If the inviter supplies a non-existent JID,
    // the room SHOULD return an <item-not-found/> error." Waddle does
    // not federate, so a non-local invitee is equally unreachable.
    if invitee.domain() != inviter_bare.domain() {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Invitee is not a local user.",
        )]);
    }
    let Some(invitee_localpart) = invitee.node().map(|node| node.to_string()) else {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Modify,
            DefinedCondition::JidMalformed,
            "Invitee must be a user JID with a localpart.",
        )]);
    };
    match crate::auth::directory::local_account_exists(
        state.deps.app_state.db_pool.global_actor(),
        &invitee_localpart,
        invitee.domain().as_str(),
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            return Some(vec![error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                "Invitee does not exist.",
            )]);
        }
        Err(error) => {
            warn!(
                invitee = %invitee,
                error = %error,
                "Failed to look up mediated-invite invitee"
            );
            return Some(vec![error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    }

    // XEP-0191: an invitee who blocks the inviter must not receive the
    // invitation, and the inviter must not learn about the block —
    // silent success, mirroring the group-DM flow.
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
                "Suppressing mediated invite because blocklist lookup failed"
            );
            deps.effects.set_rejection(crate::server::routes::interpret::effects::PlanRejection::PolicyDenied(
                crate::server::routes::interpret::effects::PolicyDeniedReason::OperationalFenceLoss,
            ));
            return Some(vec![]);
        }
    };
    if invitee_blocklist.contains_jid(&jid::Jid::from(bound_jid.clone())) {
        deps.effects.set_rejection(
            crate::server::routes::interpret::effects::PlanRejection::AuthorizationDenied(
                crate::server::routes::interpret::effects::AuthorizationDeniedReason::BlockedSender,
            ),
        );
        return Some(vec![]);
    }

    let previous_invitee_affiliation = snapshot.room.get_affiliation(&invitee);
    let mut mutation = MucMembershipMutation {
        room: room_jid.clone(),
        invitee: invitee.clone(),
        actor: room_actor.clone(),
        previous_affiliation: previous_invitee_affiliation,
    };
    let granted_membership =
        if snapshot.room.config.members_only {
            match deps
            .effects
            .execute(
                PlannedEffect::new(Effect::External(ExternalEffect::RoomMembershipMutation(
                    crate::server::routes::interpret::effects::early::RoomMembershipMutation::Muc(
                        Box::new(mutation.clone()),
                    ),
                ))),
                deps,
            )
            .await
        {
            EffectOutcome::Membership(MembershipOutcome::Granted { previous_affiliation }) => {
                mutation.previous_affiliation = previous_affiliation;
                true
            },
            EffectOutcome::Membership(MembershipOutcome::Preserved) => false,
            _ => {
                return Some(vec![error_frame(
                    incoming,
                    bound_jid,
                    deps,
                    ErrorType::Wait,
                    DefinedCondition::InternalServerError,
                    "Internal server error.",
                )])
            }
        }
        } else {
            false
        };

    // #1264: record the outstanding invite BEFORE relaying so a
    // decline arriving immediately after delivery always verifies.
    // `AlreadyOutstanding` doubles as the anti-spam dedup: an
    // identical unexpired re-invite is a silent success with NO second
    // delivery — repeated invites can neither flood the invitee nor
    // exhaust their offline pending-delivery quota.
    let ledger = OutstandingInvite {
        room: room_jid.clone(),
        invitee: invitee.clone(),
        inviter: inviter_bare.clone(),
    };
    let failure = if granted_membership {
        crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RollbackMuc {
            grant: Box::new(mutation.clone()),
            invite: ledger.clone(),
        }
    } else {
        crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RemoveLedger(
            ledger.clone(),
        )
    };
    let mut ledger_effect = PlannedEffect::new(Effect::External(ExternalEffect::InviteLedger(
        InviteLedgerMutation::Record {
            invite: ledger.clone(),
            recorded_at: chrono::Utc::now(),
            failure: granted_membership.then(|| Box::new(crate::server::routes::interpret::effects::invite::InviteDeliveryFailure::RollbackMucMembership(Box::new(mutation)))),
        },
    )));
    if snapshot.room.config.members_only {
        ledger_effect = ledger_effect.with_dependency(
            crate::server::routes::interpret::effects::PlanEffectDependency::AfterRoomMembership {
                room: room_jid.clone(),
                member: invitee.clone(),
            },
        );
    }
    let recorded_at = match deps.effects.execute(ledger_effect, deps).await {
        EffectOutcome::InviteLedger(Ok(InviteLedgerOutcome::Recorded(RecordOutcome::New {
            created_at,
        }))) => created_at,
        EffectOutcome::InviteLedger(Ok(InviteLedgerOutcome::Recorded(
            RecordOutcome::AlreadyOutstanding,
        ))) => return Some(vec![]),
        _ => {
            let error = InviteLedgerError::Storage;

            warn!(
                room = %room_jid,
                invitee = %invitee,
                error = %error,
                "Failed to record outstanding mediated invite"
            );
            return Some(vec![error_frame(
                incoming,
                bound_jid,
                deps,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };

    // XEP-0045 §7.8.2: the room adds `from` (the inviter) to the
    // `<invite/>` and sends the invitation from its own bare JID.
    let mut invite = Message::new(Some(jid::Jid::from(invitee.clone())));
    invite.id = incoming.id.clone();
    invite.from = Some(jid::Jid::from(room_jid.clone()));
    invite.type_ = MessageType::Normal;
    invite.payloads.push(build_mediated_invite_payload(
        &inviter_bare,
        &inbound_invite,
    ));

    let scoped_sink = crate::server::routes::interpret::effects::ScopedInviteSink {
        inner: deps.effects,
        invite: ledger,
        failure: Some(failure),
    };
    let mut delivery_deps = deps.clone();
    delivery_deps.effects = &scoped_sink;
    if let Err(error) = deliver_muc_user_message(state, &invitee, invite, &delivery_deps).await {
        // Neither a live socket nor the durable queue accepted the
        // invitation — undo everything (ledger row, membership grant)
        // and tell the inviter, instead of reporting a success that
        // never happened.
        warn!(
            room = %room_jid,
            invitee = %invitee,
            error = %error,
            "Mediated invite could not be delivered or queued; rolling back"
        );
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            deps,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    }

    if let Some(capture) = ingress_effect_capture {
        if granted_membership {
            capture.record_intent(IngressEffectIntent::MucInviteMembershipGrant {
                grant: MucInviteMembershipGrant {
                    room: room_jid.clone(),
                    invitee: invitee.clone(),
                    inviter: inviter_bare.clone(),
                },
            });
        }
        capture.record_intent(IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: room_jid.clone(),
                invitee: invitee.clone(),
                inviter: inviter_bare.clone(),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(recorded_at),
            },
        });
    }

    Some(vec![])
}

pub(crate) async fn execute_muc_membership(
    mutation: MucMembershipMutation,
    deps: &Deps<'_>,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let MucMembershipMutation {
        room: room_jid,
        invitee,
        actor: room_actor,
        ..
    } = mutation;
    let Ok(snapshot) = room_actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return EffectOutcome::Unavailable;
    };
    let previous_invitee_affiliation = snapshot.room.get_affiliation(&invitee);
    let granted_membership = snapshot.room.config.members_only
        && previous_invitee_affiliation < waddle_xmpp::Affiliation::Member;
    if granted_membership {
        match room_actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
        {
            Ok(()) => {}
            Err(error) => {
                if should_compensate_failed_affiliation_grant(&error) {
                    warn!(
                        room = %room_jid,
                        invitee = %invitee,
                        error = %error,
                        "members-only invite grant had recoverable error; preparing rollback"
                    );
                } else if matches!(
                    error,
                    kameo::error::SendError::HandlerError(
                        AffiliationMutationError::CommitOutcomeUnknown
                    )
                ) {
                    if let Some(recovered_actor) =
                        recover_actor_after_ambiguous_invite_grant(state, &room_jid, &room_actor)
                            .await
                    {
                        if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) {
                            if let Ok(
                                affiliation @ (waddle_xmpp::Affiliation::Owner
                                | waddle_xmpp::Affiliation::Admin
                                | waddle_xmpp::Affiliation::Member),
                            ) = recovered_actor
                                .ask(GetSnapshot)
                                .reply_timeout(std::time::Duration::from_secs(5))
                                .await
                                .map(|snapshot| snapshot.room.get_affiliation(&invitee))
                            {
                                if let Err(error) =
                                    super::super::iq::persist_managed_channel_affiliation(
                                        state,
                                        &channel_id,
                                        &invitee,
                                        affiliation,
                                    )
                                    .await
                                {
                                    warn!(
                                        room = %room_jid,
                                        invitee = %invitee,
                                        error = %error,
                                        "failed to rebuild managed invite grant after ambiguous outcome"
                                    );
                                }
                            }
                        }
                    }
                    warn!(
                        room = %room_jid,
                        invitee = %invitee,
                        error = %error,
                        "members-only invite grant has unknown durable outcome; leaving state for reconciliation"
                    );
                }
                return EffectOutcome::Unavailable;
            }
        }
    }
    // An earlier ambiguous grant may have committed the MUC affiliation but
    // lost its tuple-write reply. Reconcile the managed projection on every
    // members-only re-invite of an already admitted user as well, so a
    // healthy retry repairs that coupled state instead of skipping it.
    if snapshot.room.config.members_only
        && (granted_membership || previous_invitee_affiliation >= waddle_xmpp::Affiliation::Member)
    {
        if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) {
            if let Err(error) = super::super::iq::persist_managed_channel_affiliation(
                state,
                &channel_id,
                &invitee,
                if granted_membership {
                    waddle_xmpp::Affiliation::Member
                } else {
                    previous_invitee_affiliation
                },
            )
            .await
            {
                warn!(
                    room = %room_jid,
                    invitee = %invitee,
                    error = %error,
                    "Failed to persist members-only invite grant"
                );
                if granted_membership {
                    rollback_membership_grant(&room_actor, &invitee, previous_invitee_affiliation)
                        .await;
                }
                return EffectOutcome::Unavailable;
            }
        }
    }

    EffectOutcome::Membership(if granted_membership {
        MembershipOutcome::Granted {
            previous_affiliation: previous_invitee_affiliation,
        }
    } else {
        MembershipOutcome::Preserved
    })
}

pub(crate) async fn rollback_muc_membership(grant: &MucMembershipMutation, deps: &Deps<'_>) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let Ok(snapshot) = grant
        .actor
        .ask(GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        return;
    };
    if snapshot.room.get_affiliation(&grant.invitee) != waddle_xmpp::Affiliation::Member {
        return;
    }
    rollback_invite_grant(
        state,
        &grant.actor,
        &grant.room,
        &grant.invitee,
        grant.previous_affiliation < waddle_xmpp::Affiliation::Member,
        grant.previous_affiliation,
    )
    .await;
}

/// Compensation for a members-only auto-add after a later step in the
/// invite flow failed. Rollback failures are logged loudly — they
/// leave the invitee durably authorized without a delivered
/// invitation, which an operator must be able to see.
async fn rollback_invite_grant(
    state: &WebSocketState,
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room_jid: &jid::BareJid,
    invitee: &jid::BareJid,
    granted_membership: bool,
    previous_affiliation: waddle_xmpp::Affiliation,
) {
    if !granted_membership {
        return;
    }
    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        if let Err(error) = super::super::iq::persist_managed_channel_affiliation(
            state,
            &channel_id,
            invitee,
            previous_affiliation,
        )
        .await
        {
            warn!(
                room = %room_jid,
                invitee = %invitee,
                error = %error,
                "Failed to roll back managed-channel membership tuple after invite failure; \
                 the invitee remains durably authorized without an invitation"
            );
        }
    }
    rollback_membership_grant(room_actor, invitee, previous_affiliation).await;
}

/// Best-effort revert of a members-only auto-add after a later step in
/// the invite flow failed.
async fn rollback_membership_grant(
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    invitee: &jid::BareJid,
    previous_affiliation: waddle_xmpp::Affiliation,
) {
    let _ = room_actor
        .ask(ChangeAffiliation {
            jid: invitee.clone(),
            affiliation: previous_affiliation,
        })
        .await;
}

pub(super) fn should_compensate_failed_affiliation_grant(
    error: &kameo::error::SendError<ChangeAffiliation, AffiliationMutationError>,
) -> bool {
    !matches!(
        error,
        kameo::error::SendError::HandlerError(AffiliationMutationError::CommitOutcomeUnknown)
    )
}

/// Deliver a room-authored `<message/>` to every connected resource of
/// `recipient`, falling back to a durable pending-delivery row when
/// the user is offline — mediated invites and declines must not be
/// dropped just because the recipient is away (#1248/#1264).
/// Typed failure of [`deliver_muc_user_message`]: nothing accepted the
/// message — neither a live socket nor the durable pending-delivery
/// queue.
#[derive(Debug, thiserror::Error)]
pub enum MucUserDeliveryError {
    #[error("pending_delivery quota exceeded")]
    QuotaExceeded,
    #[error("pending_delivery storage failed: {0}")]
    Storage(#[source] waddle_xmpp::pending_delivery::storage::PendingStorageError),
    #[error("delivery effect executor unavailable")]
    Unavailable,
}

pub(super) async fn deliver_muc_user_message(
    state: &WebSocketState,
    recipient: &jid::BareJid,
    message: Message,
    deps: &Deps<'_>,
) -> Result<(), MucUserDeliveryError> {
    use crate::server::routes::interpret::effects::invite::MucUserRoute;
    let resources = waddle_xmpp::registry::get_resources_for_user(
        &state.deps.protocol.user_registry,
        recipient,
    )
    .await;
    let fallback = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let offline = resources.is_empty();
    if deps.effects.is_planning() {
        // A listed resource can refuse its Phase-C write. Freeze the fallback
        // intent now as well, before the ingress transaction commits.
        deps.capture_intent(IngressEffectIntent::PendingDelivery {
            mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: fallback.id.clone(),
            },
        });
        super::record_route_direct_intent(
            deps.ingress_effect_capture.as_ref(),
            recipient.clone(),
            resources.clone(),
        );
    }
    let route = MucUserRoute {
        recipient: recipient.clone(),
        resources,
        message: Box::new(message),
        fallback,
        failure: None,
    };
    let effect = if offline {
        ExternalEffect::QueueOfflineDelivery(route)
    } else {
        ExternalEffect::RouteToPeer(route)
    };
    match deps
        .effects
        .execute(
            PlannedEffect::new(Effect::External(effect)).with_suppression(
                crate::server::routes::interpret::effects::PlanSuppressionPolicy::SenderOnly,
            ),
            deps,
        )
        .await
    {
        EffectOutcome::Completed => Ok(()),
        EffectOutcome::MucUserDelivery(result) => result,
        _ => Err(MucUserDeliveryError::Unavailable),
    }
}

pub(super) fn mediated_invitee(message: &Message) -> Option<(jid::BareJid, minidom::Element)> {
    let x = message
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))?;
    let invite = x.get_child("invite", waddle_xmpp::muc::presence::NS_MUC_USER)?;
    let to = invite.attr("to")?.parse::<jid::BareJid>().ok()?;
    Some((to, invite.clone()))
}

/// Build the room-relayed `<x xmlns='muc#user'><invite from='inviter'/></x>`
/// payload (§7.8.2), preserving the inviter's optional `<reason/>`.
fn build_mediated_invite_payload(
    inviter: &jid::BareJid,
    inbound_invite: &minidom::Element,
) -> minidom::Element {
    let mut invite = minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            inviter.to_string(),
        );
    if let Some(reason) =
        inbound_invite.get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
    {
        invite = invite.append(reason.clone());
    }
    minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(invite.build())
        .build()
}

fn error_frame(
    incoming: &Message,
    bound_jid: &jid::FullJid,
    deps: &Deps<'_>,
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &'static str,
) -> Stanza {
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    let error = StanzaError::new(error_type, condition, "en", text);
    deps.effects
        .set_rejection(super::classify_rejection(&error));
    let frozen_error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error)
        .expect("server-built stanza error should freeze");
    let reply = message_error_reply(&stamped, error);
    if let Some(capture) = deps.ingress_effect_capture.as_ref() {
        capture.record_intent(IngressEffectIntent::ErrorReply {
            recipient: bound_jid.clone(),
            error: frozen_error,
        });
    }
    Stanza::Message(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress_shadow::IngressEffectCapture;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, register_test_connection,
    };
    use kameo::actor::{ActorRef, Spawn};
    use waddle_xmpp::ingress::IngressEffectIntent;
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation, RoomActor,
    };
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::{MucRoom, RoomConfig};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    async fn create_test_room(
        state: &WebSocketState,
        room_jid: &jid::BareJid,
        waddle_id: &str,
        channel_id: &str,
    ) -> ActorRef<RoomActor> {
        state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: waddle_id.to_string(),
                channel_id: channel_id.to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
    }

    fn spawn_unregistered_room(
        room_jid: &jid::BareJid,
        waddle_id: &str,
        channel_id: &str,
    ) -> ActorRef<RoomActor> {
        RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                room_jid.clone(),
                waddle_id.to_string(),
                channel_id.to_string(),
                RoomConfig::default(),
            ),
            OccupantIdSecret::new(vec![b't'; 32]).expect("occupant-id secret"),
        ))
    }

    async fn join_member(
        room_actor: &ActorRef<RoomActor>,
        occupant_jid: &jid::FullJid,
        nick: &str,
    ) {
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: occupant_jid.clone(),
                nick: nick.to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("join occupant");
    }

    #[tokio::test]
    async fn deliver_muc_user_message_records_live_direct_route_intent() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new(None);
        let recipient: jid::BareJid = "bob@example.com".parse().expect("recipient");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("bob phone");
        let (bob_tx, _bob_rx) = tokio::sync::mpsc::channel(4);
        register_test_connection(state.as_ref(), &bob_phone, bob_tx).await;

        let mut message = Message::new(Some(jid::Jid::from(recipient.clone())));
        message.type_ = MessageType::Normal;

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        deliver_muc_user_message(state.as_ref(), &recipient, message, &deps)
            .await
            .expect("delivery succeeds");

        assert!(capture.snapshot().intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteDirect {
                    recipient: captured_recipient,
                    fanout,
                    ..
                } if captured_recipient == &recipient && fanout == &vec![bob_phone.clone()]
            )
        }));
    }

    #[tokio::test]
    async fn deliver_muc_user_message_records_offline_direct_route_intent() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new(None);
        let recipient: jid::BareJid = "offline@example.com".parse().expect("recipient");
        let mut message = Message::new(Some(jid::Jid::from(recipient.clone())));
        message.type_ = MessageType::Normal;

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        deliver_muc_user_message(state.as_ref(), &recipient, message, &deps)
            .await
            .expect("offline queue succeeds");

        assert!(capture.snapshot().intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteDirect {
                    recipient: captured_recipient,
                    fanout,
                    ..
                } if captured_recipient == &recipient && fanout.is_empty()
            )
        }));
        assert!(capture.snapshot().intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::PendingDelivery {
                    mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                        recipient: captured_recipient,
                        ..
                    }
                } if captured_recipient == &recipient
            )
        }));
    }

    #[tokio::test]
    async fn deliver_muc_user_message_excludes_rejected_live_resources_from_route_intent() {
        let state = create_test_websocket_state().await;
        let capture = IngressEffectCapture::new(None);
        let recipient: jid::BareJid = "bob@example.com".parse().expect("recipient");
        let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("bob phone");
        let bob_laptop: jid::FullJid = "bob@example.com/laptop".parse().expect("bob laptop");
        let (bob_phone_tx, _bob_phone_rx) = tokio::sync::mpsc::channel(4);
        let (bob_laptop_tx, bob_laptop_rx) = tokio::sync::mpsc::channel(4);
        register_test_connection(state.as_ref(), &bob_phone, bob_phone_tx).await;
        register_test_connection(state.as_ref(), &bob_laptop, bob_laptop_tx).await;
        drop(bob_laptop_rx);

        let mut message = Message::new(Some(jid::Jid::from(recipient.clone())));
        message.type_ = MessageType::Normal;

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        deliver_muc_user_message(state.as_ref(), &recipient, message, &deps)
            .await
            .expect("delivery succeeds");

        assert!(capture.snapshot().intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteDirect {
                    recipient: captured_recipient,
                    fanout,
                    ..
                } if captured_recipient == &recipient && fanout == &vec![bob_phone.clone()]
            )
        }));
    }

    #[tokio::test]
    async fn mediated_invite_auth_rejection_records_error_reply_intent() {
        let state = create_test_websocket_state().await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let capture = IngressEffectCapture::new(None);
        let mut message = Message::new(Some(
            "room@muc.example.com"
                .parse::<jid::Jid>()
                .expect("room jid"),
        ));
        message.type_ = MessageType::Normal;
        message.from = Some(jid::Jid::from(sender.clone()));
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("to").to_owned(),
                            "bob@example.com",
                        )
                        .build(),
                )
                .build(),
        );

        let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        let frames = handle_muc_mediated_invite(&message, state.as_ref(), &sender, None, &deps)
            .await
            .expect("handled");

        assert_eq!(frames.len(), 1);
        let expected_error = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::NotAuthorized,
            "en",
            "Authentication required.",
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

    #[derive(Default)]
    struct GrantBeforeExecutionAndFailLedger {
        concurrent_affiliation: Option<waddle_xmpp::Affiliation>,
        granted: std::sync::atomic::AtomicBool,
        ledger_failed: std::sync::atomic::AtomicBool,
    }

    impl crate::server::routes::interpret::effects::EffectSink for GrantBeforeExecutionAndFailLedger {
        fn execute<'a>(
            &'a self,
            effect: PlannedEffect,
            deps: &'a Deps<'_>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = EffectOutcome> + Send + 'a>>
        {
            Box::pin(async move {
                use crate::server::routes::interpret::effects::{
                    early::RoomMembershipMutation, ImmediateSink,
                };
                match &effect.effect {
                    Effect::External(ExternalEffect::RoomMembershipMutation(
                        RoomMembershipMutation::Muc(grant),
                    )) => {
                        grant
                            .actor
                            .ask(ChangeAffiliation {
                                jid: grant.invitee.clone(),
                                affiliation: self
                                    .concurrent_affiliation
                                    .unwrap_or(waddle_xmpp::Affiliation::Member),
                            })
                            .await
                            .expect("independent membership grant");
                        self.granted
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Effect::External(ExternalEffect::InviteLedger(
                        InviteLedgerMutation::Record { .. },
                    )) => {
                        deps.web_socket_state
                            .expect("state")
                            .deps
                            .app_state
                            .db_pool
                            .global_actor()
                            .ask(crate::db::actor::DbExecute {
                                sql: "DROP TABLE muc_pending_invites".to_owned(),
                                params: vec![],
                            })
                            .await
                            .expect("force real downstream ledger storage failure");
                        self.ledger_failed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    _ => {}
                }
                ImmediateSink.execute(effect, deps).await
            })
        }
        fn is_planning(&self) -> bool {
            false
        }
        fn record(&self, _effect: PlannedEffect) {
            panic!("immediate test sink only executes effects");
        }
        fn set_room_execution(
            &self,
            _execution: crate::server::routes::interpret::effects::RoomExecutionPath,
        ) {
        }
    }

    async fn assert_ledger_failure_preserves_concurrent_affiliation(
        concurrent_affiliation: waddle_xmpp::Affiliation,
    ) {
        use crate::server::routes::websocket::tests::create_test_session;
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let _recipient_session = create_test_session(state.as_ref(), "bob").await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
        let room: jid::BareJid = "membership-race@muc.example.com".parse().expect("room");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "w".to_owned(),
                channel_id: "c".to_owned(),
                config: RoomConfig {
                    members_only: true,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("members-only room");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: sender.clone(),
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.com".to_owned(),
                admission_revision: 0,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("owner joins");
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("initial snapshot")
                .room
                .get_affiliation(&invitee),
            waddle_xmpp::Affiliation::None
        );
        let mut message = Message::new(Some(jid::Jid::from(room)));
        message.type_ = MessageType::Normal;
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("to").to_owned(),
                            invitee.to_string(),
                        )
                        .build(),
                )
                .build(),
        );
        let sink = GrantBeforeExecutionAndFailLedger {
            concurrent_affiliation: Some(concurrent_affiliation),
            ..Default::default()
        };
        let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        );
        deps.effects = &sink;
        let frames =
            handle_muc_mediated_invite(&message, state.as_ref(), &sender, Some(&session), &deps)
                .await
                .expect("handled invitation");
        assert_eq!(frames.len(), 1, "ledger failure returns the standard error");
        assert!(sink.granted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(sink.ledger_failed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("final snapshot")
                .room
                .get_affiliation(&invitee),
            concurrent_affiliation,
            "failed invitation must restore the execution-time affiliation"
        );
    }

    #[tokio::test]
    async fn muc_invite_ledger_failure_preserves_membership_granted_after_planning_read() {
        assert_ledger_failure_preserves_concurrent_affiliation(waddle_xmpp::Affiliation::Member)
            .await;
    }

    #[tokio::test]
    async fn muc_invite_ledger_failure_restores_outcast_set_after_planning_read() {
        assert_ledger_failure_preserves_concurrent_affiliation(waddle_xmpp::Affiliation::Outcast)
            .await;
    }

    #[tokio::test]
    async fn executed_muc_grant_never_demotes_existing_admin() {
        let state = create_test_websocket_state().await;
        let room: jid::BareJid = "preserved-admin@muc.example.com".parse().expect("room");
        let invitee: jid::BareJid = "bob@example.com".parse().expect("invitee");
        let actor = create_test_room(&state, &room, "w", "c").await;
        let mutation = MucMembershipMutation {
            room,
            invitee: invitee.clone(),
            actor: actor.clone(),
            previous_affiliation: waddle_xmpp::Affiliation::None,
        };
        // The frozen plan predates an independent administrator promotion.
        actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: waddle_xmpp::Affiliation::Admin,
            })
            .await
            .expect("concurrent admin promotion");
        let deps =
            crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
        assert!(matches!(
            execute_muc_membership(mutation.clone(), &deps).await,
            EffectOutcome::Membership(MembershipOutcome::Preserved)
        ));
        rollback_muc_membership(&mutation, &deps).await;
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
    async fn plan_muc_mediated_invite_records_ledger_and_route_without_writes() {
        use crate::server::routes::interpret::effects::{Effect, ExternalEffect, PlanSink};
        use crate::server::routes::websocket::tests::create_test_session;
        let state = create_test_websocket_state().await;
        let session = create_test_session(state.as_ref(), "alice").await;
        let _invitee_session = create_test_session(state.as_ref(), "bob").await;
        let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
        let recipient: jid::BareJid = "bob@example.com".parse().expect("recipient");
        let resource: jid::FullJid = "bob@example.com/phone".parse().expect("resource");
        let room: jid::BareJid = "planned-invite@muc.example.com".parse().expect("room");
        let actor = create_test_room(state.as_ref(), &room, "w", "c").await;
        join_member(&actor, &sender, "alice").await;
        // XEP-0045 §7.8.2: only admins may invite into a members-only room.
        actor
            .ask(ChangeAffiliation {
                jid: sender.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Admin,
            })
            .await
            .expect("promote inviter");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        register_test_connection(state.as_ref(), &resource, tx).await;
        let mut message = Message::new(Some(jid::Jid::from(room.clone())));
        message.type_ = MessageType::Normal;
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("to").to_owned(),
                            recipient.to_string(),
                        )
                        .build(),
                )
                .build(),
        );
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new(None);
        let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
            state.as_ref(),
            None,
        )
        .with_ingress_effect_capture(Some(capture.clone()));
        deps.effects = &sink;
        let frames =
            handle_muc_mediated_invite(&message, state.as_ref(), &sender, Some(&session), &deps)
                .await
                .expect("handled");
        assert!(frames.is_empty(), "unexpected frames: {frames:?}");
        let effects = sink.snapshot();
        use crate::server::routes::interpret::effects::{
            PlanEffectDependency, PlanSuppressionPolicy,
        };
        assert_eq!(
            effects.len(),
            3,
            "grant, ledger, and fan-out are the entire plan"
        );
        assert!(matches!(
            effects[0].effect,
            Effect::External(ExternalEffect::RoomMembershipMutation(_))
        ));
        assert_eq!(effects[0].suppression, PlanSuppressionPolicy::Always);
        assert!(matches!(
            effects[1].effect,
            Effect::External(ExternalEffect::InviteLedger(_))
        ));
        assert!(effects[1]
            .dependencies
            .contains(&PlanEffectDependency::AfterRoomMembership {
                room: room.clone(),
                member: recipient.clone(),
            }));
        assert!(matches!(
            effects[2].effect,
            Effect::External(ExternalEffect::RouteToPeer(_))
        ));
        assert!(effects[2]
            .dependencies
            .contains(&PlanEffectDependency::AfterInviteLedger {
                invite: OutstandingInvite {
                    room: room.clone(),
                    invitee: recipient.clone(),
                    inviter: sender.to_bare()
                },
            }));
        if let Effect::External(ExternalEffect::RouteToPeer(route)) = &effects[2].effect {
            assert!(capture.snapshot().intents.iter().any(|intent| matches!(intent,
                IngressEffectIntent::PendingDelivery {
                    mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient { recipient: pending_recipient, row_id },
                } if pending_recipient == &recipient && row_id == &route.fallback.id
            )), "even a live route must commit its exact fallback identity");
            assert_eq!(route.message.from, Some(jid::Jid::from(room.clone())));
            assert_eq!(route.message.to, Some(jid::Jid::from(recipient.clone())));
            let payload = route.message.payloads[0]
                .get_child("invite", waddle_xmpp::muc::presence::NS_MUC_USER)
                .expect("XEP-0045 invitation");
            assert_eq!(payload.attr("from"), Some(sender.to_bare().as_str()));
            assert!(
                route.failure.is_some(),
                "delivery failure owns grant compensation"
            );
        }
        assert!(rx.try_recv().is_err());
        assert!(crate::server::routes::websocket::muc_invites::list_invites(
            state.deps.app_state.db_pool.global_actor().clone(),
            &room,
            &recipient
        )
        .await
        .expect("ledger read")
        .is_empty());
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .get_affiliation(&recipient),
            waddle_xmpp::Affiliation::None
        );
    }

    #[tokio::test]
    async fn ambiguous_invite_recovery_does_not_overwrite_successor_roster() {
        let state = create_test_websocket_state().await;
        let room_jid: jid::BareJid = "ambiguous-successor@muc.example.com"
            .parse()
            .expect("room jid");
        let successor =
            create_test_room(state.as_ref(), &room_jid, "successor-w", "successor-c").await;
        let stale_actor = spawn_unregistered_room(&room_jid, "stale-w", "stale-c");
        let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");

        join_member(&stale_actor, &alice, "alice").await;
        join_member(&successor, &bob, "bob").await;

        let recovered =
            recover_actor_after_ambiguous_invite_grant(state.as_ref(), &room_jid, &stale_actor)
                .await
                .expect("live successor should be returned");

        assert_eq!(recovered.id(), successor.id());
        let recovered_snapshot = recovered
            .ask(GetSnapshot)
            .await
            .expect("recovered snapshot");
        assert!(
            recovered_snapshot
                .room
                .find_occupant_by_real_jid(&bob)
                .is_some(),
            "the live successor occupant must survive recovery"
        );
        assert!(
            recovered_snapshot
                .room
                .find_occupant_by_real_jid(&alice)
                .is_none(),
            "the stale snapshot must not overwrite a published successor roster"
        );

        stale_actor.kill();
    }

    #[tokio::test]
    async fn ambiguous_invite_recovery_transplants_roster_when_stale_actor_is_demoted() {
        let state = create_test_websocket_state().await;
        let room_jid: jid::BareJid = "ambiguous-demotion@muc.example.com"
            .parse()
            .expect("room jid");
        let stale_actor = create_test_room(state.as_ref(), &room_jid, "stale-w", "stale-c").await;
        let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");

        join_member(&stale_actor, &alice, "alice").await;

        let recovered =
            recover_actor_after_ambiguous_invite_grant(state.as_ref(), &room_jid, &stale_actor)
                .await
                .expect("fresh actor should be recovered");

        assert_ne!(recovered.id(), stale_actor.id());
        let recovered_snapshot = recovered
            .ask(GetSnapshot)
            .await
            .expect("recovered snapshot");
        let restored = recovered_snapshot
            .room
            .find_occupant_by_real_jid(&alice)
            .expect("demoted stale roster should be transplanted to the replacement");
        assert_eq!(restored.nick, "alice");
    }
}

#[cfg(test)]
mod compensation_tests {
    use super::should_compensate_failed_affiliation_grant;
    use waddle_xmpp::muc::room_actor::ChangeAffiliation;

    #[test]
    fn ambiguous_affiliation_grant_errors_do_not_trigger_compensation() {
        assert!(!should_compensate_failed_affiliation_grant(
            &kameo::error::SendError::<ChangeAffiliation, _>::HandlerError(
                waddle_xmpp::muc::room_actor::AffiliationMutationError::CommitOutcomeUnknown
            )
        ));
        assert!(should_compensate_failed_affiliation_grant(
            &kameo::error::SendError::<ChangeAffiliation, _>::HandlerError(
                waddle_xmpp::muc::room_actor::AffiliationMutationError::PersistFailed
            )
        ));
    }
}
