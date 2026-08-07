use super::extension_forms::CommandBoundary;
use super::*;
use crate::server::routes::websocket::ResolvedPrincipal;

pub(super) struct CommandTargets<'a> {
    pub(super) domain: &'a str,
    pub(super) muc_domain: &'a str,
    pub(super) extensions_domain: &'a str,
    pub(super) push_domain: &'a str,
}

pub(super) async fn handle_command_iq(
    request_iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    targets: CommandTargets<'_>,
    principal: Option<ResolvedPrincipal<'_>>,
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
        // XEP-0050 §4.4 malformed-action: the responder does not
        // understand the specified `action` attribute value. Map this
        // case to the command-namespaced `<malformed-action/>` child
        // rather than a bare `<bad-request/>` so generic XEP-0050
        // clients can distinguish it from other malformed requests.
        Err(CommandError::InvalidAction(_)) => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::ad_hoc_command(
                    AdHocCommandCondition::MalformedAction,
                    Some("unrecognised command action".to_string()),
                ),
            )];
        }
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
        .unwrap_or_else(|| targets.domain.to_string());
    let boundary = CommandBoundary::classify(&node);
    let target_allowed = match boundary {
        CommandBoundary::Server => target.as_str() == targets.domain,
        CommandBoundary::Extensions => target.as_str() == targets.extensions_domain,
        CommandBoundary::PushService => target.as_str() == targets.push_domain,
        CommandBoundary::MucRoom => {
            match exact_bare_muc_room_target(request_iq, targets.muc_domain) {
                Some(room_jid) => room_command_available(state, &room_jid, bound_jid).await,
                None => false,
            }
        }
    };
    if !target_allowed {
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
        authenticated_user_id: principal.map(|principal| principal.user_jid.clone()),
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
        CommandResult::Completed {
            session_id: response_session_id,
            form,
            notes,
        } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Completed);
            command.session_id = response_session_id.or(session_id);
            command.form = form;
            command.notes = notes;
            command
        }
        CommandResult::Canceled {
            session_id: response_session_id,
            notes,
        } => {
            let mut command = Command::new(node.clone());
            command.status = Some(CommandStatus::Canceled);
            command.session_id = response_session_id.or(session_id);
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

fn exact_bare_muc_room_target(
    request_iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
) -> Option<BareJid> {
    request_iq.to().and_then(|jid| {
        let bare = jid.to_bare();
        (jid.resource().is_none() && bare.domain().as_str() == muc_domain).then_some(bare)
    })
}

async fn room_command_available(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&FullJid>,
) -> bool {
    let Some(requester_bare) = requester.map(|jid| jid.to_bare()) else {
        return false;
    };
    if let Some(room_actor) = get_room_actor(state, room_jid).await {
        let Ok(snapshot) = room_actor.ask(GetSnapshot).await else {
            return false;
        };
        if !snapshot.room.config.group_dm {
            return false;
        }
        if snapshot.room.get_affiliation(&requester_bare) >= Affiliation::Member {
            return true;
        }
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
            return false;
        };
        return requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id)
            .await;
    }

    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return false;
    };
    let Ok(Some(channel)) = get_managed_channel_for_room(state, room_jid).await else {
        return false;
    };
    if channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
        return false;
    }
    if state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(requester_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, &channel_id),
        })
        .await
        .ok()
        .is_some_and(|response| response.allowed)
    {
        return true;
    }
    requester_has_durable_group_dm_membership(state, &requester_bare, &channel_id).await
}

async fn requester_has_durable_group_dm_membership(
    state: &WebSocketState,
    requester_bare: &BareJid,
    channel_id: &str,
) -> bool {
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbQueryOne {
            sql: r#"
                SELECT 1 FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_id = ?
                  AND subject_relation IS NULL
                LIMIT 1
            "#
            .to_string(),
            params: vec![channel_id.into(), requester_bare.to_string().into()],
        })
        .await
        .is_ok_and(|row| row.is_some())
}
