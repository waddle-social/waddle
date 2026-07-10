use waddle_xmpp::{
    muc::room_registry_actor::GetRoom, parser::stanza_to_string,
    protocol::handlers::errors::message_error_reply, Stanza,
};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use crate::server::routes::websocket::WebSocketState;

pub(super) async fn handle_muc_direct_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    if let Some(frames) = handle_muc_private_message(incoming, state, bound_jid).await {
        return Some(frames);
    }
    handle_muc_mediated_decline(incoming, state, bound_jid).await
}

async fn handle_muc_private_message(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    let target_occupant_jid = incoming.to.as_ref()?.clone().try_into_full().ok()?;
    let room_jid = target_occupant_jid.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    if incoming.type_ == MessageType::Groupchat {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "Groupchat messages must be addressed to the room bare JID.",
        )]);
    }
    if !matches!(incoming.type_, MessageType::Chat | MessageType::Normal) {
        return None;
    }
    let target_nick = target_occupant_jid.resource().to_string();

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
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested room not found.",
        )]);
    };
    let Ok(snapshot) = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
    else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Wait,
            DefinedCondition::InternalServerError,
            "Internal server error.",
        )]);
    };
    let Some(sender_nick) = snapshot.room.find_nick_by_real_jid(bound_jid) else {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "Only room occupants may send private messages.",
        )]);
    };
    if snapshot.room.get_occupant(&target_nick).is_none() {
        return Some(vec![message_error_frame(
            incoming,
            bound_jid,
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            "Requested occupant not found.",
        )]);
    }

    let from_room_jid = match room_jid.clone().with_resource_str(sender_nick) {
        Ok(jid) => jid,
        Err(_) => {
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };
    // XEP-0421 §Business Rules: "The <occupant-id/> element MUST be
    // attached to every message ... sent by a MUC" — private messages
    // included. Derive the sender's stable occupant-id the same way
    // the groupchat canonicalize handler does (#1268).
    let sender_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
        &bound_jid.to_bare(),
        &room_jid,
        &state.deps.occupant_id_secret,
    );
    for recipient in snapshot.room.get_occupant_sessions(&target_nick) {
        let mut routed = incoming.clone();
        routed.from = Some(jid::Jid::from(from_room_jid.clone()));
        routed.to = Some(jid::Jid::from(recipient.clone()));
        canonicalize_muc_private_payloads(&mut routed, &sender_occupant_id);
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&recipient, Stanza::Message(routed));
    }

    Some(Vec::new())
}

/// XEP-0045 §7.8.2 mediated decline, hardened per #1264:
///
/// - the decline is only forwarded when the outstanding-invite ledger
///   holds a row for `(room, decliner)` — without that check any
///   authenticated user could make the room deliver a "declined your
///   invitation" message to an arbitrary user;
/// - the recipient is the ledger-recorded inviter (server-
///   authoritative), not whatever `to` the client supplied;
/// - delivery is durable: an offline inviter gets a pending-delivery
///   row instead of a silent drop, and the ledger row is only consumed
///   once the decline was delivered or queued.
///
/// The room actor is deliberately not consulted: the ledger outlives
/// room-actor dormancy eviction, so a legitimate decline still reaches
/// the inviter after the room actor was evicted.
async fn handle_muc_mediated_decline(
    incoming: &Message,
    state: &WebSocketState,
    bound_jid: &jid::FullJid,
) -> Option<Vec<String>> {
    if incoming.type_ != MessageType::Normal {
        return None;
    }
    let room_jid = incoming.to.as_ref()?.to_bare();
    if room_jid.domain().as_str() != state.deps.service_domains.muc {
        return None;
    }
    let inbound_decline = mediated_decline(incoming)?;

    let decliner = bound_jid.to_bare();
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    let invite = match crate::server::routes::websocket::muc_invites::find_invite(
        db_actor.clone(),
        &room_jid,
        &decliner,
    )
    .await
    {
        Ok(Some(invite)) => invite,
        Ok(None) => {
            // #1264: no outstanding invitation — refuse instead of
            // relaying a fabricated decline.
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                ErrorType::Auth,
                DefinedCondition::Forbidden,
                "You have no outstanding invitation to this room.",
            )]);
        }
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                decliner = %decliner,
                error = %error,
                "Failed to look up outstanding invite for mediated decline"
            );
            return Some(vec![message_error_frame(
                incoming,
                bound_jid,
                ErrorType::Wait,
                DefinedCondition::InternalServerError,
                "Internal server error.",
            )]);
        }
    };

    let x = build_mediated_decline_payload(bound_jid, inbound_decline);
    let mut mediated = Message::new(Some(jid::Jid::from(invite.inviter.clone())));
    mediated.id = incoming.id.clone();
    mediated.from = Some(jid::Jid::from(room_jid.clone()));
    mediated.type_ = MessageType::Normal;
    mediated.payloads.push(x);
    super::muc_invite::deliver_muc_user_message(state, &invite.inviter, mediated).await;

    if let Err(error) = crate::server::routes::websocket::muc_invites::consume_invite(
        db_actor, &room_jid, &decliner,
    )
    .await
    {
        tracing::warn!(
            room = %room_jid,
            decliner = %decliner,
            error = %error,
            "Failed to consume outstanding invite after mediated decline"
        );
    }

    Some(Vec::new())
}

fn mediated_decline(message: &Message) -> Option<&minidom::Element> {
    message
        .payloads
        .iter()
        .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
        .and_then(|x| x.get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER))
}

fn build_mediated_decline_payload(
    decliner: &jid::FullJid,
    inbound_decline: &minidom::Element,
) -> minidom::Element {
    let mut decline = minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            decliner.to_bare().to_string(),
        );
    if let Some(reason) =
        inbound_decline.get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
    {
        decline = decline.append(reason.clone());
    }
    minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(decline.build())
        .build()
}

fn canonicalize_muc_private_payloads(
    message: &mut Message,
    sender_occupant_id: &waddle_xmpp::xep::xep0421::OccupantId,
) {
    message.payloads.retain(|payload| {
        // XEP-0313 §Security "MUC message spoofing" + XEP-0045
        // anti-spoofing: strip every client-supplied payload in a MUC
        // *service* namespace (muc / muc#user / muc#admin / muc#owner)
        // so an occupant cannot forge affiliation/role/status/invite
        // signalling on a PM that the server then relays from
        // `room/nick`. Namespace-only, sharing the exact set with the
        // groupchat canonicalizer (#1251, #1268).
        if waddle_xmpp::muc::is_muc_service_namespace(payload.ns().as_str())
            || payload.is("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
        {
            return false;
        }
        // XEP-0359: stanza-ids are assigned by servers/rooms, never by
        // senders. Strip every client-supplied stanza-id from a MUC private
        // message so a client cannot inject a room-spoofing or otherwise
        // misleading identifier — the server is the canonical source.
        if payload.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID) {
            return false;
        }
        true
    });
    message
        .payloads
        .push(minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER).build());
    // XEP-0421: re-stamp the server-derived occupant-id after stripping
    // any client-supplied one (#1268).
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(message, sender_occupant_id);
}

fn message_error_frame(
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
    use super::*;
    use waddle_xmpp::xep::xep0421::{
        extract_occupant_id_from_message, generate_occupant_id, OccupantId, OccupantIdSecret,
        NS_OCCUPANT_ID, OCCUPANT_ID_SECRET_MIN_BYTES,
    };

    fn secret() -> OccupantIdSecret {
        OccupantIdSecret::new(vec![3u8; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("valid secret")
    }

    fn pm() -> Message {
        let mut m = Message::new(Some(
            "room@muc.example.com/bob".parse::<jid::Jid>().expect("jid"),
        ));
        m.type_ = MessageType::Chat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), "psst".to_string());
        m
    }

    /// XEP-0421 Business Rules (#1268): MUC private messages MUST carry
    /// the server-derived occupant-id — stripping the client's forgery
    /// and re-stamping the canonical value.
    #[test]
    fn xep0421_pm_carries_server_stamped_occupant_id() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // Client tries to spoof someone else's occupant-id.
        msg.payloads
            .push(waddle_xmpp::xep::xep0421::build_occupant_id_element(
                &OccupantId::new("forged-id"),
            ));

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        let stamped = extract_occupant_id_from_message(&msg).expect("occupant-id stamped on PM");
        assert_eq!(stamped, server_id);
        assert_ne!(stamped, OccupantId::new("forged-id"));
        // Exactly one occupant-id element.
        let count = msg
            .payloads
            .iter()
            .filter(|p| p.is("occupant-id", NS_OCCUPANT_ID))
            .count();
        assert_eq!(count, 1);
    }

    /// XEP-0045 §7.5: the PM keeps exactly one empty muc#user `<x/>`
    /// marker (client-supplied ones are stripped) alongside the
    /// occupant-id.
    #[test]
    fn xep0421_pm_keeps_single_empty_muc_user_marker() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // Forged muc#user x with an item claiming an affiliation.
        msg.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("item", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            "owner",
                        )
                        .build(),
                )
                .build(),
        );

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        let markers: Vec<_> = msg
            .payloads
            .iter()
            .filter(|p| p.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))
            .collect();
        assert_eq!(markers.len(), 1, "exactly one muc#user marker");
        assert_eq!(
            markers[0].children().count(),
            0,
            "the PM marker is empty — forged items must not survive"
        );
    }

    /// XEP-0313 §Security / XEP-0045 anti-spoofing (#1251): a PM must
    /// not launder client-supplied payloads in ANY MUC service
    /// namespace (muc / muc#admin / muc#owner), not just muc#user —
    /// including non-`<x>` element names.
    #[test]
    fn xep0045_pm_strips_all_muc_service_namespaces() {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let sender_bare: jid::BareJid = "alice@example.com".parse().expect("sender");
        let secret = secret();
        let server_id = generate_occupant_id(&sender_bare, &room, &secret);

        let mut msg = pm();
        // A non-`<x>` element in muc#user (status code), plus payloads
        // in muc / muc#admin / muc#owner.
        msg.payloads.push(
            minidom::Element::builder("status", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), "110")
                .build(),
        );
        msg.payloads
            .push(minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC).build());
        msg.payloads
            .push(minidom::Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN).build());
        msg.payloads
            .push(minidom::Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build());

        canonicalize_muc_private_payloads(&mut msg, &server_id);

        // Only the single server-authored empty muc#user marker remains
        // in MUC service namespaces; nothing else.
        for ns in [
            waddle_xmpp::muc::presence::NS_MUC,
            waddle_xmpp::muc::NS_MUC_ADMIN,
            waddle_xmpp::muc::NS_MUC_OWNER,
        ] {
            assert!(
                !msg.payloads.iter().any(|p| p.ns() == ns),
                "client payloads in `{ns}` must be stripped from a MUC PM"
            );
        }
        let muc_user: Vec<_> = msg
            .payloads
            .iter()
            .filter(|p| p.ns() == waddle_xmpp::muc::presence::NS_MUC_USER)
            .collect();
        assert_eq!(
            muc_user.len(),
            1,
            "only the server-authored empty muc#user marker survives"
        );
        assert_eq!(muc_user[0].name(), "x");
        assert_eq!(muc_user[0].children().count(), 0);
    }
}
