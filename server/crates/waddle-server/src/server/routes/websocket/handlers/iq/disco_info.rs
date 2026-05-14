use super::*;

mod account;
mod extensions;
mod muc;
mod server_info;
mod services;
mod spaces;

#[derive(Clone, Copy)]
struct DiscoInfoRequest<'a> {
    request_iq: &'a xmpp_parsers::iq::Iq,
    id: &'a str,
    node: Option<&'a str>,
    target_to: Option<&'a str>,
    domain: &'a str,
    muc_domain: &'a str,
    upload_domain: &'a str,
    spaces_domain: &'a str,
    extensions_domain: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
}

pub(super) async fn handle_disco_info_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    if ctx.payload_ns != "http://jabber.org/protocol/disco#info" {
        return Vec::new();
    }

    let query = match parse_disco_info_query(ctx.iq) {
        Ok(query) => query,
        Err(_) => {
            return vec![build_iq_error_xml_typed(
                ctx.id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
    };

    let request = DiscoInfoRequest {
        request_iq: ctx.iq,
        id: ctx.id,
        node: query.node.as_deref(),
        target_to: ctx.target_to,
        domain: ctx.domain,
        muc_domain: ctx.muc_domain,
        upload_domain: ctx.upload_domain,
        spaces_domain: ctx.spaces_domain,
        extensions_domain: ctx.extensions_domain,
        response_from: ctx.response_from,
        response_to: ctx.response_to,
    };

    if let Some(response) = muc::handle_muc_disco_info(&request, state).await {
        return response;
    }

    if let Some(response) = server_info::handle_command_disco_info(&request, state).await {
        return response;
    }

    if let Some(response) = extensions::handle_extensions_disco_info(&request, state).await {
        return response;
    }

    if let Some(response) =
        spaces::handle_spaces_disco_info(&request, state, authenticated_session.as_ref()).await
    {
        return response;
    }

    if let Some(response) = services::handle_upload_disco_info(&request) {
        return response;
    }

    if let Some(response) = account::handle_account_disco_info(&request, state, phase).await {
        return response;
    }

    server_info::handle_server_disco_info(&request, state, authenticated_session.as_ref()).await
}
