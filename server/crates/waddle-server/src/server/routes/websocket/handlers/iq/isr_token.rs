use super::*;

pub(super) async fn handle_isr_token_request_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    sender_jid: Option<&FullJid>,
) -> Vec<String> {
    let (Some(session), Some(sender_jid)) = (authenticated_session.as_ref(), sender_jid) else {
        return vec![iq_to_xml(build_isr_token_error(iq, "not-authorized"))];
    };
    let token: IsrToken = state
        .deps
        .protocol
        .isr_token_store
        .create_token(session.user_jid.to_string(), sender_jid.to_bare());
    vec![iq_to_xml(build_isr_token_result(iq, &token))]
}
