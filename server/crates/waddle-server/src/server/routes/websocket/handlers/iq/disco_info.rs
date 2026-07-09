use super::*;

mod account;
mod calls_mixer;
mod community;
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
    requester: Option<&'a FullJid>,
    domain: &'a str,
    muc_domain: &'a str,
    upload_domain: &'a str,
    spaces_domain: &'a str,
    community_domain: &'a str,
    extensions_domain: &'a str,
    push_domain: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
}

enum DiscoInfoResponse<'a> {
    Iq(xmpp_parsers::iq::Iq),
    IqError {
        id: &'a str,
        from: Option<&'a str>,
        to: Option<&'a str>,
        error: xmpp_parsers::stanza_error::StanzaError,
    },
}

impl<'a> DiscoInfoResponse<'a> {
    fn iq(iq: xmpp_parsers::iq::Iq) -> Self {
        Self::Iq(iq)
    }

    fn error(
        id: &'a str,
        from: Option<&'a str>,
        to: Option<&'a str>,
        error: xmpp_parsers::stanza_error::StanzaError,
    ) -> Self {
        Self::IqError {
            id,
            from,
            to,
            error,
        }
    }

    fn into_xml(self) -> String {
        match self {
            Self::Iq(iq) => iq_to_xml(iq),
            Self::IqError {
                id,
                from,
                to,
                error,
            } => build_iq_error_xml_typed(id, from, to, error),
        }
    }
}

fn disco_info_xml(response: DiscoInfoResponse<'_>) -> Vec<String> {
    vec![response.into_xml()]
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
            debug!(outcome = "malformed", "disco#info request rejected");
            return disco_info_xml(DiscoInfoResponse::error(
                ctx.id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            ));
        }
    };

    // Every disco#info request that reaches this dispatcher emits exactly one
    // bounded handler category. IQ ids and targets are deliberately excluded:
    // both are client-controlled identifiers and authenticated-self targets
    // are user JIDs. Aggregate handler counts remain useful for diagnosing
    // dispatch stalls without exporting per-request identity into Alloy.

    let request = DiscoInfoRequest {
        request_iq: ctx.iq,
        id: ctx.id,
        node: query.node.as_deref(),
        target_to: ctx.target_to,
        requester: phase.bound_jid(),
        domain: ctx.domain,
        muc_domain: ctx.muc_domain,
        upload_domain: ctx.upload_domain,
        spaces_domain: ctx.spaces_domain,
        community_domain: ctx.community_domain,
        extensions_domain: ctx.extensions_domain,
        push_domain: ctx.push_domain,
        response_from: ctx.response_from,
        response_to: ctx.response_to,
    };

    if let Some(response) = muc::handle_muc_disco_info(&request, state).await {
        info!(handler = "muc", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = calls_mixer::handle_calls_mixer_disco_info(&request, state) {
        info!(handler = "calls_mixer", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = server_info::handle_command_disco_info(&request, state).await {
        info!(handler = "commands", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = extensions::handle_extensions_disco_info(&request, state).await {
        info!(handler = "extensions", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) =
        spaces::handle_spaces_disco_info(&request, state, authenticated_session.as_ref()).await
    {
        info!(handler = "spaces", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = community::handle_community_disco_info(&request) {
        info!(handler = "community", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = services::handle_upload_disco_info(&request) {
        info!(handler = "upload", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = services::handle_push_service_disco_info(&request, state, phase).await {
        info!(handler = "push", "disco#info answered");
        return disco_info_xml(response);
    }

    if let Some(response) = account::handle_account_disco_info(&request, state, phase).await {
        info!(handler = "account", "disco#info answered");
        return disco_info_xml(response);
    }

    info!(handler = "server_fallback", "disco#info answered");
    disco_info_xml(
        server_info::handle_server_disco_info(&request, state, authenticated_session.as_ref())
            .await,
    )
}
