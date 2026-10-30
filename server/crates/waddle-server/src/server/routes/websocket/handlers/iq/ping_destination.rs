use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::protocol::handlers::jingle::calls_mixer_jid;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::{
    stanza_to_xml, ConnectionPhase, IqConnState, IqHandlerContext, Session, Stanza, WebSocketState,
};
use crate::server::routes::websocket::XmppServiceDomains;

/// Check the remote ping target before entering the shared IQ dispatcher.
pub(super) async fn handle_sans_io_iq_with_ping_preflight(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    authenticated_session: Option<&Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Option<Vec<String>> {
    if ctx.payload_ns == waddle_xmpp::xep::xep0199::NS_PING
        && state
            .deps
            .protocol
            .dispatcher
            .has_iq_handler(ctx.payload_ns)
    {
        if let Some(full_jid) = phase.bound_jid() {
            if let Some(reply) =
                remote_ping_error(ctx.iq, ctx.domain, &state.deps.service_domains, full_jid)
            {
                return Some(vec![stanza_to_xml(&Stanza::Iq(Box::new(reply)))]);
            }
        }
    }
    super::sans_io::handle_sans_io_iq(ctx, state, authenticated_session, phase, conn_state).await
}

/// Reject an unsupported remote ping before the local namespace handler can
/// answer it. Local full-JID forwarding and MUC self-ping run before this gate.
pub(super) fn remote_ping_error(
    iq: &Iq,
    domain: &str,
    services: &XmppServiceDomains,
    sender: &FullJid,
) -> Option<Iq> {
    let payload = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        Iq::Result { .. } | Iq::Error { .. } => return None,
    };
    let target = iq.to()?;
    let target_domain = target.domain().as_str();
    let mixer = calls_mixer_jid(domain);
    if [
        domain,
        services.muc.as_str(),
        services.spaces.as_str(),
        services.upload.as_str(),
        services.community.as_str(),
        services.extensions.as_str(),
        services.push.as_str(),
        mixer.domain().as_str(),
    ]
    .contains(&target_domain)
    {
        return None;
    }

    let mut error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAllowed,
        "en",
        "Remote ping routing is not allowed by this server.",
    );
    // RFC 6120 §8.3.1: swap the request addresses and identify the local
    // error generator with `by`. An error from the intended target is not a
    // remote pong. Always address it to the bound origin, never client `from`.
    error.by = Some(Jid::from(BareJid::from_parts(None, sender.domain())));
    Some(Iq::Error {
        from: Some(target.clone()),
        to: Some(Jid::from(sender.clone())),
        id: iq.id().to_owned(),
        payload: Some(payload.clone()),
        error,
    })
}
