use super::*;

/// XEP-0030 on the user bots domain. An enabled bot is a leaf `client/bot`
/// entity the server answers for; it supports disco#info and nothing else
/// until it holds a MUC occupancy. The bare domain, unknown or disabled
/// bots, and any node on a bot do not exist.
pub(super) async fn handle_user_bot_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
    state: &WebSocketState,
) -> Option<DiscoInfoResponse<'a>> {
    let target = req.target_to?.parse::<BareJid>().ok()?;
    if target.domain() != &**req.bots_domain {
        return None;
    }
    let not_found = || {
        Some(DiscoInfoResponse::error(
            req.id,
            req.response_from,
            req.response_to,
            item_not_found_iq_error("Requested item not found."),
        ))
    };
    if req.node.is_some() {
        return not_found();
    }
    let Ok(bot) = crate::auth::bots::BotJid::parse(target.as_str(), req.bots_domain) else {
        return not_found();
    };
    match state.deps.auth_state.bot_store.get(&bot).await {
        Ok(Some(record)) if record.disabled.is_none() => {
            let name = record
                .display_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| bot.localpart());
            let identities = vec![Identity::client_bot(Some(name))];
            let features = vec![Feature::disco_info()];
            let response = build_disco_info_response(req.request_iq, &identities, &features, None);
            Some(DiscoInfoResponse::iq(response))
        }
        Ok(_) => not_found(),
        Err(error) => {
            warn!(target = %target, error = %error, "Failed to resolve user bot disco target");
            Some(DiscoInfoResponse::error(
                req.id,
                req.response_from,
                req.response_to,
                internal_server_error_iq_error("Internal server error."),
            ))
        }
    }
}
