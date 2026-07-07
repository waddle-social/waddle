use super::*;

pub(in crate::server::routes::websocket::handlers::presence) async fn handle_directed_presence(
    state: &WebSocketState,
    sender_jid: &FullJid,
    mut presence: xmpp_parsers::presence::Presence,
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
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&target_full, stanza)
            .await;
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
