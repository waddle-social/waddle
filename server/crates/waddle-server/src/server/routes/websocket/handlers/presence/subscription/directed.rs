use super::*;

pub(in crate::server::routes::websocket::handlers::presence) async fn handle_directed_presence(
    state: &WebSocketState,
    sender_jid: &FullJid,
    mut presence: xmpp_parsers::presence::Presence,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    let Some(target) = presence.to.clone() else {
        return;
    };
    let target_bare = target.to_bare();
    if recipient_blocks_sender(state, &target_bare, &sender_jid.to_bare()).await {
        debug!(from = %sender_jid, to = %target_bare, "Dropping directed presence blocked by recipient");
        return;
    }

    presence.from = Some(Jid::from(sender_jid.clone()));
    let stanza = Stanza::Presence(presence);
    if let Ok(target_full) = target.clone().try_into_full() {
        if super::delivery::try_route_presence_to_full_remote(
            state,
            &target_full,
            &stanza,
            ordered_relay_origin,
        )
        .await
        {
            return;
        }
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&target_full, stanza)
            .await;
        return;
    }

    if super::delivery::try_route_presence_to_bare_remote(
        state,
        &target_bare,
        &stanza,
        ordered_relay_origin,
    )
    .await
    {
        return;
    }

    for resource in waddle_xmpp::registry::get_resources_for_user(
        &state.deps.protocol.user_registry,
        &target_bare,
    )
    .await
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}
