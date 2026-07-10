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
    muc::room_actor::{ChangeAffiliation, GetSnapshot},
    muc::room_registry_actor::GetRoom,
    parser::stanza_to_string,
    pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId},
    protocol::handlers::errors::message_error_reply,
    Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::auth::Session;
use crate::server::routes::websocket::muc_invites::{record_invite, OutstandingInvite};
use crate::server::routes::websocket::WebSocketState;

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
        if room_actor
            .ask(ChangeAffiliation {
                jid: invitee.clone(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .is_err()
        {
            return Some(vec![error_frame(
                incoming,
                bound_jid,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
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
                    "Failed to persist members-only invite grant; rolling back"
                );
                rollback_membership_grant(&room_actor, &invitee, previous_invitee_affiliation)
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
    }

    // #1264: record the outstanding invite BEFORE relaying so a
    // decline arriving immediately after delivery always verifies.
    if let Err(error) = record_invite(
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
            "Failed to record outstanding mediated invite"
        );
        if granted_membership {
            if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) {
                let _ = super::super::iq::persist_managed_channel_affiliation(
                    state,
                    &channel_id,
                    &invitee,
                    previous_invitee_affiliation,
                )
                .await;
            }
            rollback_membership_grant(&room_actor, &invitee, previous_invitee_affiliation).await;
        }
        return Some(vec![error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
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

    deliver_muc_user_message(state, &invitee, invite).await;

    Some(vec![])
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

/// Deliver a room-authored `<message/>` to every connected resource of
/// `recipient`, falling back to a durable pending-delivery row when
/// the user is offline — mediated invites and declines must not be
/// dropped just because the recipient is away (#1248/#1264).
pub(super) async fn deliver_muc_user_message(
    state: &WebSocketState,
    recipient: &jid::BareJid,
    message: Message,
) {
    let resources = waddle_xmpp::registry::get_resources_for_user(
        &state.deps.protocol.user_registry,
        recipient,
    )
    .await;
    if resources.is_empty() {
        if let Err(error) = queue_offline_muc_user_message(state, recipient, &message).await {
            warn!(
                recipient = %recipient,
                error = %error,
                "Failed to queue offline MUC invite/decline message"
            );
        }
        return;
    }
    for resource in resources {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, Stanza::Message(message.clone()))
            .await;
    }
}

/// Queue a room-authored invite/decline for an offline recipient in
/// the pending-delivery store (same transient shape the group-DM
/// invite path uses — flushed verbatim at the recipient's next
/// session).
pub(super) async fn queue_offline_muc_user_message(
    state: &WebSocketState,
    recipient: &jid::BareJid,
    message: &Message,
) -> Result<(), String> {
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
        Ok(InsertOutcome::QuotaExceeded) => Err("pending_delivery quota exceeded".to_string()),
        Err(error) => Err(error.to_string()),
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
