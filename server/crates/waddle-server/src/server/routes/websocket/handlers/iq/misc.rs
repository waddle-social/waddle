use super::*;

pub(super) async fn handle_misc_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Vec<String> {
    let iq = ctx.iq;
    let payload_ns = ctx.payload_ns;
    let domain = ctx.domain;
    let muc_domain = ctx.muc_domain;
    let push_domain = ctx.push_domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // jabber:iq:roster is served by handle_roster_iq above because it needs
    // durable roster storage and roster-push fanout.

    if waddle_xmpp::xep::xep0054::is_vcard_get(iq) || waddle_xmpp::xep::xep0054::is_vcard_set(iq) {
        return handle_vcard_iq(iq, state, phase.bound_jid(), response_from, response_to).await;
    }

    if is_last_activity_query(iq) {
        return handle_last_activity_iq(
            iq,
            domain,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
        )
        .await;
    }

    if waddle_xmpp::xep::xep0049::is_private_storage_query(iq) {
        return handle_private_storage_iq(iq, state, phase.bound_jid(), response_from, response_to)
            .await;
    }

    if waddle_xmpp::xep::xep0191::is_blocking_query(iq) {
        return handle_blocking_iq(
            iq,
            state,
            phase.bound_jid(),
            response_from,
            response_to,
            conn_state.state_machine.as_deref_mut(),
        )
        .await;
    }

    if is_push_enable(iq) || is_push_disable(iq) {
        return handle_push_iq(
            iq,
            state,
            phase.bound_jid(),
            push_domain,
            response_from,
            response_to,
        )
        .await;
    }

    if is_search_request(iq) {
        return handle_channel_search_iq(iq, muc_domain, state, response_from, response_to).await;
    }

    if payload_ns == "jabber:iq:search" {
        return handle_user_search_iq(iq, domain, state, response_from, response_to).await;
    }
    Vec::new()
}
