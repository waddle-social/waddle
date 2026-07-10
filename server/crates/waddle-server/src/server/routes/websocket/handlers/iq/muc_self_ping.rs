use super::*;

pub(super) async fn handle_muc_self_ping_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let Some(target) = iq.to().and_then(|jid| jid.clone().try_into_full().ok()) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };
    let room_jid = target.to_bare();
    let nick = target.resource().to_string();
    // XEP-0410 server optimization: with the
    // `muc#self-ping-optimization` feature advertised, the service
    // answers a self-ping authoritatively — success means "joined",
    // `<not-acceptable/>` means "not joined". A room with no live actor
    // (never created, reaped, or sealed-dormant) has no in-memory
    // occupancy, so the pinging session is NOT joined and must be told
    // so. The previous `<item-not-found/>` answer was read by XEP-0410
    // clients (including our own `interpret_self_ping_response`) as
    // "still joined" — after a room was reaped the client never rejoined
    // and silently stopped receiving messages (#1254). Rejoining
    // re-hydrates a dormant room through the normal join path.
    let Some(room_actor) = get_room_actor(state, &room_jid).await else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_acceptable_iq_error("You are not joined to this room."),
        )];
    };
    match room_actor
        .ask(PingSelfCheck {
            nick,
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(()) => vec![build_iq_result_xml(
            iq.id(),
            response_from,
            response_to,
            None,
        )],
        Err(kameo::error::SendError::HandlerError(_)) => vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_acceptable_iq_error("You are not joined to this room."),
        )],
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to process MUC self-ping");
            vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )]
        }
    }
}
