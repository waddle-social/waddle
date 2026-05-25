use super::extension_forms::CommandBoundary;
use super::*;

pub(super) async fn handle_command_iq(
    request_iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    domain: &str,
    extensions_domain: &str,
    push_domain: &str,
    authenticated_session: &Option<Session>,
    bound_jid: Option<&FullJid>,
) -> Vec<String> {
    let sender_jid: Jid = match bound_jid.cloned().map(Jid::from) {
        Some(jid) => jid,
        None => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::not_authorized(Some("Authenticated session required".to_string())),
            )];
        }
    };

    let command = match parse_command_from_iq(request_iq) {
        Ok(command) => command,
        Err(err) => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::bad_request(Some(format!("Invalid command request: {err}"))),
            )];
        }
    };

    let node = command.node.clone();
    let target = request_iq
        .to()
        .map(|jid| jid.to_bare().to_string())
        .unwrap_or_else(|| domain.to_string());
    let allowed_target = match CommandBoundary::classify(&node) {
        CommandBoundary::Server => domain,
        CommandBoundary::Extensions => extensions_domain,
        CommandBoundary::PushService => push_domain,
    };
    if target.as_str() != allowed_target {
        return vec![build_xmpp_error_response(
            request_iq,
            XmppError::service_unavailable(Some(
                "Command is not available on this service".to_string(),
            )),
        )];
    }
    let session_id = command.session_id.clone();
    let ctx = CommandContext {
        from: sender_jid,
        authenticated_user_id: authenticated_session
            .as_ref()
            .map(|session| session.user_id.clone()),
        iq: request_iq.clone(),
        command,
    };

    let result = state.deps.protocol.command_registry.dispatch(ctx).await;
    let response_command = match result {
        CommandResult::Executing {
            form,
            session_id,
            notes,
            actions,
        } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Executing);
            command.session_id = Some(session_id);
            command.form = Some(form);
            command.notes = notes;
            command.actions = actions;
            command
        }
        CommandResult::Completed { form, notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Completed);
            command.session_id = session_id;
            command.form = form;
            command.notes = notes;
            command
        }
        CommandResult::Canceled { notes } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Canceled);
            command.session_id = session_id;
            command.notes = notes;
            command
        }
        CommandResult::Error(err) => return vec![build_xmpp_error_response(request_iq, err)],
    };

    vec![iq_to_xml(build_command_result(
        request_iq,
        &response_command,
    ))]
}
