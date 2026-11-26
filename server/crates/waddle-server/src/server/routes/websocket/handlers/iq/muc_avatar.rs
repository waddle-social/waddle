//! XEP-0486 MUC Avatars: the room's vcard-temp.
//!
//! A `vcard-temp` IQ addressed to a room's bare JID reads or publishes
//! the room's avatar rather than an account's profile. The vCard is
//! stored in `vcard_storage` under the room JID, and its photos are
//! advertised as XEP-0153 hashes in the room's `muc#roominfo` form.

use super::{
    bad_request_iq_error, build_iq_error_xml_typed, forbidden_iq_error, get_room_actor,
    internal_server_error_iq_error, iq_to_xml, is_muc_room_jid, item_not_found_iq_error, warn, Arc,
    BareJid, FullJid, GetSnapshot, Stanza, VCardStore, WebSocketState,
};
use crate::permissions::host_grant::{HostGrantAction, PrincipalEvidence};
use waddle_xmpp::muc::{build_config_change_message, MucConfigStatusCode};
use waddle_xmpp::xep::xep0486::{parse_room_vcard, RoomAvatarUpdate};

/// The room a `vcard-temp` IQ is addressed to, when `to` is the bare JID
/// of a room on the MUC service.
pub(super) fn vcard_room_target(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
) -> Option<BareJid> {
    let to = iq.to()?;
    if to.resource().is_some() || to.domain().as_str() != state.deps.service_domains.muc.as_str() {
        return None;
    }
    to.node()?;
    Some(to.to_bare())
}

/// The XEP-0153 hashes of a room's published avatar, empty when it has
/// none. A storage failure is logged and reads as no avatar so that it
/// cannot break the room's disco#info.
pub(in crate::server::routes::websocket::handlers) async fn room_avatar_hashes(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Vec<String> {
    let store = VCardStore::new(Arc::new(state.deps.app_state.db_pool.global().clone()));
    match store.get(room_jid).await {
        Ok(Some(vcard)) => {
            waddle_xmpp::xep::xep0486::avatar_hashes(&vcard).unwrap_or_else(|error| {
                warn!(room = %room_jid, %error, "Stored room vCard is not a valid avatar");
                Vec::new()
            })
        }
        Ok(None) => Vec::new(),
        Err(error) => {
            warn!(room = %room_jid, %error, "Failed to load room vCard for disco#info");
            Vec::new()
        }
    }
}

/// Forget a destroyed room's avatar.
pub(in crate::server::routes::websocket::handlers) async fn delete_room_avatar(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<(), crate::vcard::VCardError> {
    VCardStore::new(Arc::new(state.deps.app_state.db_pool.global().clone()))
        .delete(room_jid)
        .await
        .map(|_| ())
}

pub(super) async fn handle_room_vcard_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender: &FullJid,
    room_jid: &BareJid,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let error = |condition| {
        vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            condition,
        )]
    };

    // A room is only as visible to the vCard as to its disco#info: the
    // same decision hides locked, private-event and host-grant rooms from
    // those who may not see them.
    let disco = xmpp_parsers::iq::Iq::Get {
        from: iq.from().cloned(),
        to: Some(room_jid.clone().into()),
        id: iq.id().to_string(),
        payload: xmpp_parsers::minidom::Element::builder(
            "query",
            waddle_xmpp::disco::DISCO_INFO_NS,
        )
        .build(),
    };
    match super::muc_disco_info_for_remote(state, &disco, Some(sender), PrincipalEvidence::NoStream)
        .await
    {
        Some(Ok(_)) => {}
        Some(Err(denial)) => return error(denial),
        None => return error(item_not_found_iq_error("Requested item not found.")),
    }

    let store = VCardStore::new(Arc::new(state.deps.app_state.db_pool.global().clone()));

    let xmpp_parsers::iq::Iq::Set { payload: vcard, .. } = iq else {
        return match store.get(room_jid).await {
            Ok(Some(vcard)) => vec![iq_to_xml(xmpp_parsers::iq::Iq::Result {
                from: iq.to().cloned(),
                to: iq.from().cloned(),
                id: iq.id().to_string(),
                payload: Some(vcard),
            })],
            Ok(None) => error(item_not_found_iq_error("The room has no avatar.")),
            Err(failure) => {
                warn!(room = %room_jid, %failure, "Failed to load room vCard");
                error(internal_server_error_iq_error("Internal server error."))
            }
        };
    };

    // xep-0486.xml:132: "If the user trying to publish an avatar isn't
    // allowed to do so, the room must return a forbidden error".
    if !is_muc_room_jid(state, room_jid).await {
        return error(item_not_found_iq_error("Requested item not found."));
    }
    match super::room_action_authorized(
        state,
        room_jid,
        sender,
        None,
        &PrincipalEvidence::NoStream,
        HostGrantAction::Manage,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error(forbidden_iq_error(
                "Only owners are allowed to set avatars.",
            ))
        }
        Err(failure) => {
            warn!(room = %room_jid, %failure, "Failed to authorize room avatar publication");
            return error(internal_server_error_iq_error("Internal server error."));
        }
    }

    let update = match parse_room_vcard(vcard) {
        Ok(update) => update,
        Err(rejection) => return error(bad_request_iq_error(&rejection.to_string())),
    };
    let previous = room_avatar_hashes(state, room_jid).await;
    let (stored, hashes) = match update {
        RoomAvatarUpdate::Unpublish => (store.delete(room_jid).await.map(|_| ()), Vec::new()),
        RoomAvatarUpdate::Publish { hashes } => (store.set(room_jid, vcard).await, hashes),
    };
    if let Err(failure) = stored {
        warn!(room = %room_jid, %failure, "Failed to store room vCard");
        return error(internal_server_error_iq_error("Internal server error."));
    }
    // A destroy that raced the publication must not leave an avatar
    // behind for the next room of that name.
    if !is_muc_room_jid(state, room_jid).await {
        if let Err(failure) = store.delete(room_jid).await {
            warn!(room = %room_jid, %failure, "Failed to drop avatar of a destroyed room");
        }
        return error(item_not_found_iq_error("Requested item not found."));
    }

    if hashes != previous {
        notify_configuration_changed(state, room_jid).await;
    }
    vec![iq_to_xml(waddle_xmpp::xep::xep0054::build_vcard_success(
        iq,
    ))]
}

/// xep-0486.xml:143: "The room should then broadcast a notification that
/// the configuration changed to all users present": status 104 to every
/// session of every occupant.
async fn notify_configuration_changed(state: &WebSocketState, room_jid: &BareJid) {
    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        return;
    };
    let snapshot = match room_actor.ask(GetSnapshot).await {
        Ok(snapshot) => snapshot,
        Err(failure) => {
            warn!(room = %room_jid, ?failure, "Failed to snapshot room for avatar notification");
            return;
        }
    };
    let codes = [MucConfigStatusCode::NonPrivacyConfigurationChange];
    for occupant in snapshot.room.occupants.values() {
        for recipient in snapshot.room.occupant_sessions(&occupant.nick) {
            let message = build_config_change_message(room_jid, &recipient, &codes);
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(&recipient, Stanza::Message(message))
                .await;
        }
    }
}
