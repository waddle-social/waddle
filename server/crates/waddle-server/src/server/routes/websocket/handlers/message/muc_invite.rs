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
    muc::room_actor::{AffiliationMutationError, ChangeAffiliation, GetSnapshot},
    muc::room_registry_actor::{DemoteRoomIfExactActor, GetOrCreateRoom, GetRoom},
    parser::stanza_to_string,
    pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId},
    protocol::handlers::errors::message_error_reply,
    Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::auth::Session;
use crate::server::routes::websocket::muc_invites::{
    claim_invite, record_invite, OutstandingInvite, RecordOutcome,
};
use crate::server::routes::websocket::WebSocketState;

async fn recover_actor_after_ambiguous_invite_grant(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    stale_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    snapshot: &waddle_xmpp::muc::room_actor::RoomSnapshot,
) -> Option<kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>> {
    let _ = state
        .deps
        .protocol
        .room_registry
        .ask(DemoteRoomIfExactActor {
            room_jid: room_jid.clone(),
            actor_ref: stale_actor.clone(),
        })
        .await;
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
        .map(|acquisition| acquisition.actor_ref)
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
) -> Option<Vec<String>> {
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
        .await
        .ok()
        .flatten()
    else {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor.ask(GetSnapshot).await else {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };

    // XEP-0045 §7.8: a mediated invitation is an occupant action ("a
    // room in which one is an occupant").
    if snapshot.room.find_nick_by_real_jid(bound_jid).is_none() {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
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
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Invitee is not a local user.",
        )]);
    }
    let Some(invitee_localpart) = invitee.node().map(|node| node.to_string()) else {
        return Some(vec![error_frame(
            incoming,
            bound_jid,
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
            return Some(vec![]);
        }
    };
    if invitee_blocklist.contains_jid(&jid::Jid::from(bound_jid.clone())) {
        return Some(vec![]);
    }

    // XEP-0045 §7.8.2: members-only auto-add — without a member-list
    // entry the invitation would be undeliverable in practice (the
    // invitee could never pass admission).
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
                    if let Some(recovered_actor) = recover_actor_after_ambiguous_invite_grant(
                        state,
                        &room_jid,
                        &room_actor,
                        &snapshot,
                    )
                    .await
                    {
                        if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) {
                            if let Ok(
                                waddle_xmpp::Affiliation::Owner
                                | waddle_xmpp::Affiliation::Admin
                                | waddle_xmpp::Affiliation::Member,
                            ) = recovered_actor
                                .ask(GetSnapshot)
                                .await
                                .map(|snapshot| snapshot.room.get_affiliation(&invitee))
                            {
                                if let Err(error) =
                                    super::super::iq::persist_managed_channel_affiliation(
                                        state,
                                        &channel_id,
                                        &invitee,
                                        waddle_xmpp::Affiliation::Member,
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
                return Some(vec![error_frame(
                    incoming,
                    bound_jid,
                    ErrorType::Wait,
                    DefinedCondition::InternalServerError,
                    "Internal server error.",
                )]);
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
                waddle_xmpp::Affiliation::Member,
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
                return Some(vec![error_frame(
                    incoming,
                    bound_jid,
                    ErrorType::Wait,
                    DefinedCondition::InternalServerError,
                    "Internal server error.",
                )]);
            }
        }
    }

    // #1264: record the outstanding invite BEFORE relaying so a
    // decline arriving immediately after delivery always verifies.
    // `AlreadyOutstanding` doubles as the anti-spam dedup: an
    // identical unexpired re-invite is a silent success with NO second
    // delivery — repeated invites can neither flood the invitee nor
    // exhaust their offline pending-delivery quota.
    match record_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &OutstandingInvite {
            room: room_jid.clone(),
            invitee: invitee.clone(),
            inviter: inviter_bare.clone(),
        },
    )
    .await
    {
        Ok(RecordOutcome::New) => {}
        Ok(RecordOutcome::AlreadyOutstanding) => return Some(vec![]),
        Err(error) => {
            warn!(
                room = %room_jid,
                invitee = %invitee,
                error = %error,
                "Failed to record outstanding mediated invite"
            );
            rollback_invite_grant(
                state,
                &room_actor,
                &room_jid,
                &invitee,
                granted_membership,
                previous_invitee_affiliation,
            )
            .await;
            return Some(vec![error_frame(
                incoming,
                bound_jid,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    }

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

    if let Err(error) = deliver_muc_user_message(state, &invitee, invite).await {
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
        if let Err(error) = claim_invite(
            state.deps.app_state.db_pool.global_actor().clone(),
            &OutstandingInvite {
                room: room_jid.clone(),
                invitee: invitee.clone(),
                inviter: inviter_bare.clone(),
            },
        )
        .await
        {
            warn!(
                room = %room_jid,
                invitee = %invitee,
                error = %error,
                "Failed to remove ledger row for undeliverable invite"
            );
        }
        rollback_invite_grant(
            state,
            &room_actor,
            &room_jid,
            &invitee,
            granted_membership,
            previous_invitee_affiliation,
        )
        .await;
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    }

    Some(vec![])
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
pub(super) enum MucUserDeliveryError {
    #[error("pending_delivery quota exceeded")]
    QuotaExceeded,
    #[error("pending_delivery storage failed: {0}")]
    Storage(String),
}

pub(super) async fn deliver_muc_user_message(
    state: &WebSocketState,
    recipient: &jid::BareJid,
    message: Message,
) -> Result<(), MucUserDeliveryError> {
    let resources = waddle_xmpp::registry::get_resources_for_user(
        &state.deps.protocol.user_registry,
        recipient,
    )
    .await;
    let mut delivered = false;
    for resource in &resources {
        if state
            .deps
            .protocol
            .connection_registry
            .send_to(resource, Stanza::Message(message.clone()))
            .await
            .is_sent()
        {
            delivered = true;
        }
    }
    if delivered {
        return Ok(());
    }
    // Offline — or every registered session refused the write (a
    // half-closed socket is indistinguishable from offline here):
    // fall back to the durable queue rather than reporting success
    // for a message nobody received.
    queue_offline_muc_user_message(state, recipient, &message).await
}

/// Queue a room-authored invite/decline for an offline recipient in
/// the pending-delivery store (same transient shape the group-DM
/// invite path uses — flushed verbatim at the recipient's next
/// session).
async fn queue_offline_muc_user_message(
    state: &WebSocketState,
    recipient: &jid::BareJid,
    message: &Message,
) -> Result<(), MucUserDeliveryError> {
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(row)
        .await
    {
        Ok(InsertOutcome::Inserted) => Ok(()),
        Ok(InsertOutcome::QuotaExceeded) => Err(MucUserDeliveryError::QuotaExceeded),
        Err(error) => Err(MucUserDeliveryError::Storage(error.to_string())),
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
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &'static str,
) -> String {
    let mut stamped = incoming.clone();
    stamped.from = Some(jid::Jid::from(bound_jid.clone()));
    let reply = message_error_reply(
        &stamped,
        StanzaError::new(error_type, condition, "en", text),
    );
    stanza_to_string(reply).unwrap_or_default()
}

#[cfg(test)]
mod tests {
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
