use super::extension_forms::CommandBoundary;
use super::permissions::stanza_error_from_parts;
use super::{
    build_command_result, build_xmpp_error_response, get_managed_channel_for_room, get_room_actor,
    iq_to_xml, parse_command_from_iq, AdHocCommandCondition, Affiliation, BareJid, CheckPermission,
    Command, CommandContext, CommandError, CommandResult, CommandStatus, DbQueryOne, FullJid,
    GetSnapshot, Jid, Object, ObjectType, Permission, StanzaErrorCondition, StanzaErrorType,
    Subject, WebSocketState, XmppError,
};
use crate::server::routes::interpret::{deliver_direct_to_full, FullJidDeliveryOutcome};
use crate::server::routes::websocket::frame_backstop::{
    spawn_decoupled_streaming_inference, StreamingInferenceSession,
};
use crate::server::routes::websocket::ResolvedPrincipal;
use std::sync::Arc;
use waddle_xmpp::xep::RsmResponse;
use waddle_xmpp::Stanza;

/// Language of all responder-authored command text (XEP-0050 §2.4).
const RESPONSE_LANG: &str = "en";

pub(super) struct CommandTargets<'a> {
    pub(super) domain: &'a str,
    pub(super) muc_domain: &'a str,
    pub(super) extensions_domain: &'a str,
    pub(super) push_domain: &'a str,
}

fn command_result_to_stanza(
    request_iq: &xmpp_parsers::iq::Iq,
    node: &str,
    session_id: Option<String>,
    result: CommandResult,
) -> Stanza {
    let response_command = match result {
        CommandResult::Executing {
            form,
            session_id,
            notes,
            actions,
        } => {
            let mut command = Command::new(node.to_owned()).with_lang(RESPONSE_LANG);
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
            let mut command = Command::new(node.to_owned()).with_lang(RESPONSE_LANG);
            command.status = Some(CommandStatus::Completed);
            command.session_id = response_session_id.or(session_id);
            command.form = form;
            command.notes = notes;
            command
        }
        CommandResult::CompletedPage {
            session_id: response_session_id,
            form,
            next_cursor,
            notes,
        } => {
            let mut command = Command::new(node.to_owned());
            command.status = Some(CommandStatus::Completed);
            command.session_id = response_session_id.or(session_id);
            command.form = Some(form);
            command.result_set = next_cursor.map(|cursor| RsmResponse::new().with_last(cursor));
            command.notes = notes;
            command
        }
        CommandResult::Canceled {
            session_id: response_session_id,
            notes,
        } => {
            let mut command = Command::new(node.to_owned()).with_lang(RESPONSE_LANG);
            command.status = Some(CommandStatus::Canceled);
            command.session_id = response_session_id.or(session_id);
            command.notes = notes;
            command
        }
        CommandResult::Error(err) => {
            let stanza_err = match err {
                XmppError::Stanza {
                    condition,
                    error_type,
                    text,
                } => stanza_error_from_parts(error_type, condition, text, None),
                XmppError::AdHocCommand { condition, text } => {
                    let (error_type, general) = condition.stanza_error();
                    let child = xmpp_parsers::minidom::Element::builder(
                        condition.element_name(),
                        waddle_xmpp::xep::xep0050::NS_COMMANDS,
                    )
                    .build();
                    stanza_error_from_parts(error_type, general, text, Some(child))
                }
                other => stanza_error_from_parts(
                    StanzaErrorType::Wait,
                    StanzaErrorCondition::InternalServerError,
                    Some(other.to_string()),
                    None,
                ),
            };
            let mut iq = xmpp_parsers::iq::Iq::from_error(request_iq.id().to_owned(), stanza_err);
            if let Some(to) = request_iq.to() {
                iq = iq.with_from(to.clone());
            }
            if let Some(from) = request_iq.from() {
                iq = iq.with_to(from.clone());
            }
            return Stanza::Iq(Box::new(iq));
        }
    };

    Stanza::Iq(Box::new(build_command_result(
        request_iq,
        &response_command,
    )))
}

pub(super) async fn handle_command_iq(
    request_iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    targets: CommandTargets<'_>,
    principal: Option<ResolvedPrincipal<'_>>,
    bound_jid: Option<&FullJid>,
    active_inference_sessions: Option<&mut Vec<StreamingInferenceSession>>,
) -> Vec<String> {
    let sender_full_jid = match bound_jid.cloned() {
        Some(jid) => jid,
        None => {
            return vec![build_xmpp_error_response(
                request_iq,
                XmppError::not_authorized(Some("Authenticated session required".to_string())),
            )];
        }
    };
    let sender_jid: Jid = Jid::from(sender_full_jid.clone());

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
            // XEP-0050 §4.4: the responding JID cannot find the requested
            // command node. This also hides whether a room exists or the
            // requester may use it.
            XmppError::item_not_found(Some("Command node not found on this service".to_string())),
        )];
    }
    let session_id = command.session_id.clone();
    let ctx = CommandContext {
        from: sender_jid,
        authenticated_user_id: principal.map(|principal| principal.user_jid.clone()),
        iq: request_iq.clone(),
        command,
        tenant_id: state.deps.protocol.command_registry.tenant_id().cloned(),
        placement_generation: None,
    };

    if boundary == CommandBoundary::Extensions {
        let registry = Arc::clone(&state.deps.protocol.command_registry);
        let user_registry = state.deps.protocol.user_registry.clone();
        let sm_session_registry = Arc::clone(&state.deps.protocol.sm_session_registry);
        let shutdown = state.deps.shutdown.stop_token();
        let req_iq = request_iq.clone();
        let node_for_task = node.clone();
        let stream_id = format!("ext-cmd:{}:{}", req_iq.id(), node);
        let (session, _handle) = spawn_decoupled_streaming_inference(
            stream_id,
            16,
            shutdown,
            move |_chunk_tx, mut cancel_rx| async move {
                let result = tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => CommandResult::Canceled {
                        session_id: session_id.clone(),
                        notes: vec![waddle_xmpp::commands::Note::info(
                            "Extension command canceled".to_owned(),
                        )],
                    },
                    res = registry.dispatch(ctx) => res,
                };
                let reply_stanza =
                    command_result_to_stanza(&req_iq, &node_for_task, session_id, result);
                let _outcome: FullJidDeliveryOutcome = deliver_direct_to_full(
                    Some(&user_registry),
                    Some(&sm_session_registry),
                    &sender_full_jid,
                    &reply_stanza,
                    None,
                )
                .await;
            },
        );
        if let Some(sessions) = active_inference_sessions {
            sessions.retain(|existing| !existing.chunk_rx.is_closed());
            sessions.push(session);
        } else {
            tokio::spawn(async move {
                let mut s = session;
                while s.chunk_rx.recv().await.is_some() {}
            });
        }
        return Vec::new();
    }

    let result = state.deps.protocol.command_registry.dispatch(ctx).await;
    let reply = command_result_to_stanza(request_iq, &node, session_id, result);
    let Stanza::Iq(reply_iq) = reply else {
        return Vec::new();
    };
    vec![iq_to_xml(*reply_iq)]
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
        if snapshot.room.affiliation(&requester_bare) >= Affiliation::Member {
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
