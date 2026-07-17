use super::groupchat_archive::{extract_room_stanza_id, room_scoped_reply_to_attr};
use super::*;

pub(super) struct BotGroupchatDispatch<'a> {
    pub(super) room_jid: &'a BareJid,
    pub(super) occupants: &'a [OccupantSnapshot],
    pub(super) durable_recipient_bare_jids: &'a [BareJid],
    pub(super) sender_full: &'a FullJid,
    pub(super) room_actor: Option<&'a ActorRef<RoomActor>>,
    pub(super) room_moderated: bool,
    pub(super) room_occupants_may_change_subject: bool,
    pub(super) room_members_only: bool,
    pub(super) pin_permission: waddle_xmpp::muc::PinPermission,
    pub(super) dispatch_timestamp: i64,
    pub(super) recursion_depth: u8,
    pub(super) occupant_id_secret: &'a waddle_xmpp::xep::xep0421::OccupantIdSecret,
}

pub(super) async fn dispatch_bot_groupchat_response(
    deps: &Deps<'_>,
    bot_ctx: BotGroupchatDispatch<'_>,
    response: ExtensionRoomMessage,
) -> Result<ExtensionRoomDispatchResult, ExtensionBotDispatchError> {
    let mut outcome = InterpretOutcome::default();
    if response.room.as_str() != bot_ctx.room_jid.to_string() {
        warn!(
            room = %bot_ctx.room_jid,
            response_room = response.room.as_str(),
            "Extension room message room did not match dispatch room; dropping"
        );
        return Err(ExtensionBotDispatchError::RoomMismatch);
    }

    let mut working = Message::new(Some(Jid::from(bot_ctx.room_jid.clone())));
    working.id = Some(xmpp_parsers::message::Id(
        response
            .stanza_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    ));
    working.from = Some(Jid::from(bot_ctx.sender_full.clone()));
    working.type_ = XmppMessageType::Groupchat;
    working.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        response.body.as_str().to_string(),
    );

    if let Some(thread_id) = response.thread_id.as_ref() {
        set_thread_id(&mut working, thread_id.as_str());
    }
    if let Some(reply_to) = response.reply_to.as_ref() {
        let mut reply = ReplyReference::new(reply_to.id.as_str());
        if let Some(to) = reply_to
            .to
            .as_ref()
            .and_then(|to| room_scoped_reply_to_attr(to.as_str(), bot_ctx.room_jid))
        {
            reply = reply.with_to(to);
        }
        set_reply_payload(&mut working, &reply);
    }
    if let Some(markup) = build_extension_message_markup(&response.markup) {
        working.payloads.push(markup);
    }
    if let Some(extensions) = response.extensions.as_ref() {
        working.payloads.push(extensions.to_minidom());
        working
            .payloads
            .push(waddle_xmpp::xep::build_fallback_element(
                &waddle_xmpp::xep::FallbackIndication::whole_body(
                    waddle_extensions::FRAMEWORK_NAMESPACE,
                ),
            ));
    }

    if let Some(room_actor) = bot_ctx.room_actor {
        if let Err(stanza_error) = validate_groupchat_rich_targets(
            deps,
            bot_ctx.room_jid,
            &working,
            None,
            room_actor,
            Some(0),
        )
        .await
        {
            warn!(
                room = %bot_ctx.room_jid,
                error = ?stanza_error,
                "Extension room message failed rich-target validation; dropping"
            );
            return Err(ExtensionBotDispatchError::RichTargetInvalid);
        }
    }

    let id_gen = UuidV4Generator;
    let ctx = RoomContext {
        room: bot_ctx.room_jid,
        sender_full: bot_ctx.sender_full,
        occupants: bot_ctx.occupants,
        durable_recipient_bare_jids: bot_ctx.durable_recipient_bare_jids,
        managed_room_forbidden: false,
        room_moderated: bot_ctx.room_moderated,
        room_occupants_may_change_subject: bot_ctx.room_occupants_may_change_subject,
        room_members_only: bot_ctx.room_members_only,
        pin_permission: bot_ctx.pin_permission,
        id_gen: &id_gen,
        occupant_id_secret: bot_ctx.occupant_id_secret,
        sender_nickname_generation: 0,
        project_sender_inbox: false,
        // XEP-0513 §"Multi-User Chats Permissions" §304 +
        // adversarial review on PR #738: the extension-bot dispatcher
        // is the only production caller that needs to broadcast as a
        // synthetic sender. Trust is established upstream by the
        // bot-registration path (`bot_ctx` is built only after the
        // sender's bot identity has been validated). Carrying the
        // authority as an explicit typed marker — separate from
        // `project_sender_inbox` — keeps the inbox-projection flag
        // from doubling as an implicit permission bypass.
        synthetic_sender_authority: Some(
            waddle_xmpp::protocol::room::SyntheticSenderAuthority::ServerAuthored,
        ),
        dispatch_timestamp: bot_ctx.dispatch_timestamp,
    };

    let dispatch_outcome = default_room_pipeline_dispatcher().dispatch(&mut working, &ctx);
    let stanza_id = extract_room_stanza_id(&working, bot_ctx.room_jid)
        .and_then(|id| StanzaId::new(id).ok())
        .ok_or(ExtensionBotDispatchError::MissingCanonicalStanzaId)?;
    let nested = Box::pin(interpret_with_depth(
        dispatch_outcome.events,
        deps,
        bot_ctx.recursion_depth,
    ))
    .await;
    let _retry_suppression = nested.retry_suppression;
    outcome.frames.extend(nested.frames);
    outcome.close = nested.close;
    outcome.feedback.extend(nested.feedback);
    // Extension-bot dispatch returns only transport/callback effects. The
    // explicit discard above keeps a nested room retry marker local to that
    // batch so it cannot label subsequent bot work.
    Ok(ExtensionRoomDispatchResult { outcome, stanza_id })
}

pub(crate) struct ExtensionRoomMessage {
    pub body: DisplayText,
    pub room: RoomJid,
    pub preferred_nick: Option<String>,
    pub bot_hat_label: Option<DisplayText>,
    pub stanza_id: Option<StanzaId>,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
    pub markup: Vec<MessageMarkupSpan>,
    pub extensions: Option<ExtensionEnvelope>,
}

pub(crate) struct ExtensionRoomDispatchResult {
    pub outcome: InterpretOutcome,
    pub stanza_id: StanzaId,
}

pub(crate) fn build_extension_message_markup(spans: &[MessageMarkupSpan]) -> Option<Element> {
    let xep_spans = spans
        .iter()
        .map(|span| waddle_xmpp::xep::Xep0394MarkupSpan {
            kind: match span.kind {
                MessageMarkupKind::Blockquote => waddle_xmpp::xep::Xep0394MarkupKind::Blockquote,
            },
            start: span.start,
            end: span.end,
        })
        .collect::<Vec<_>>();
    waddle_xmpp::xep::build_message_markup_element(&xep_spans)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExtensionBotDispatchError {
    #[error("extension bot dispatch has no WebSocket state")]
    MissingWebSocketState,
    #[error("extension bot dispatch has no room registry")]
    MissingRoomRegistry,
    #[error("extension bot dispatch room is not registered")]
    RoomNotRegistered,
    #[error("extension bot dispatch room lookup failed")]
    RoomLookupFailed,
    #[error("extension bot dispatch room snapshot failed")]
    SnapshotFailed,
    #[error("extension bot is outcast from the room")]
    BotOutcast,
    #[error("extension bot could not join the room")]
    BotJoinFailed,
    #[error("extension room message target did not match dispatch room")]
    RoomMismatch,
    #[error("extension room message failed rich-target validation")]
    RichTargetInvalid,
    #[error("extension room message did not receive a canonical room stanza id")]
    MissingCanonicalStanzaId,
}

pub(crate) async fn dispatch_extension_bot_groupchat_response(
    deps: &Deps<'_>,
    room_jid: BareJid,
    bot_full: FullJid,
    response: ExtensionRoomMessage,
) -> Result<ExtensionRoomDispatchResult, ExtensionBotDispatchError> {
    let mut outcome = InterpretOutcome::default();
    let Some(state) = deps.web_socket_state else {
        warn!(
            room = %room_jid,
            "Extension bot groupchat dispatch has no WebSocket state; dropping"
        );
        return Err(ExtensionBotDispatchError::MissingWebSocketState);
    };
    let Some(room_registry) = deps.room_registry else {
        warn!(
            room = %room_jid,
            "Extension bot groupchat dispatch has no room registry; dropping"
        );
        return Err(ExtensionBotDispatchError::MissingRoomRegistry);
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            warn!(room = %room_jid, "Extension bot groupchat room not registered; dropping");
            return Err(ExtensionBotDispatchError::RoomNotRegistered);
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat room lookup failed; dropping"
            );
            return Err(ExtensionBotDispatchError::RoomLookupFailed);
        }
    };
    match room_actor
        .ask(GetAffiliation {
            jid: bot_full.to_bare(),
        })
        .await
    {
        Ok(waddle_xmpp::Affiliation::Outcast) => {
            warn!(
                room = %room_jid,
                bot = %bot_full,
                "Extension bot is outcast from room; dropping room message"
            );
            return Err(ExtensionBotDispatchError::BotOutcast);
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot affiliation lookup failed; dropping"
            );
            return Err(ExtensionBotDispatchError::RoomLookupFailed);
        }
    }
    let initial_snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: bot_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat snapshot failed; dropping"
            );
            return Err(ExtensionBotDispatchError::SnapshotFailed);
        }
    };
    let initial_occupants: Vec<OccupantSnapshot> = initial_snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let bot_nick = available_bot_nick_with_base(
        &initial_occupants,
        response.preferred_nick.as_deref().unwrap_or("waddle"),
    );
    match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bot_full.clone(),
            nick: bot_nick.clone(),
            affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Member),
            local_domain: state.deps.auth_state.xmpp_domain.clone(),
            admission_revision: initial_snapshot.admission_revision,
        })
        .await
    {
        Ok(join) => {
            if !join.is_same_bare_multi_session_join {
                for existing in join.existing_occupants {
                    let from = match room_jid.clone().with_resource_str(&bot_nick) {
                        Ok(from) => from,
                        Err(error) => {
                            warn!(
                                room = %room_jid,
                                %error,
                                "Extension bot presence could not build room occupant JID"
                            );
                            continue;
                        }
                    };
                    let bot_bare = bot_full.to_bare();
                    let mut presence = waddle_xmpp::muc::build_occupant_presence(
                        &from,
                        &existing.jid,
                        join.new_occupant_affiliation,
                        join.new_occupant_role,
                        waddle_xmpp::muc::MucPresenceStatus::new(false, false),
                        &waddle_xmpp::xep::xep0421::OccupantIdentity {
                            bare_jid: &bot_bare,
                            real_jid: Some(&bot_full),
                            secret: &state.deps.occupant_id_secret,
                        },
                    );
                    let bot_hat = response
                        .bot_hat_label
                        .as_ref()
                        .map(|label| {
                            waddle_xmpp::xep::xep0317::Hat::new(
                                label.as_str(),
                                waddle_xmpp::xep::xep0317::well_known::BOT,
                            )
                        })
                        .unwrap_or_else(waddle_xmpp::xep::xep0317::Hat::bot);
                    waddle_xmpp::xep::xep0317::set_hats(
                        &mut presence,
                        &waddle_xmpp::xep::xep0317::HatSet::new().with_hat(bot_hat),
                    );
                    let _ = state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&existing.jid, Stanza::Presence(presence));
                }
            }
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot could not join room; dropping room message"
            );
            return Err(ExtensionBotDispatchError::BotJoinFailed);
        }
    }
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: bot_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Extension bot groupchat snapshot after join failed; dropping"
            );
            return Err(ExtensionBotDispatchError::SnapshotFailed);
        }
    };
    let occupants: Vec<OccupantSnapshot> = snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let durable_recipient_bare_jids = snapshot.durable_recipient_bare_jids.clone();
    let nested = dispatch_bot_groupchat_response(
        deps,
        BotGroupchatDispatch {
            room_jid: &room_jid,
            occupants: &occupants,
            durable_recipient_bare_jids: &durable_recipient_bare_jids,
            sender_full: &bot_full,
            room_actor: Some(&room_actor),
            room_moderated: snapshot.config.moderated,
            room_occupants_may_change_subject: snapshot.config.occupants_may_change_subject,
            room_members_only: snapshot.config.members_only,
            pin_permission: snapshot.config.pin_permission,
            dispatch_timestamp: chrono::Utc::now().timestamp(),
            recursion_depth: 0,
            occupant_id_secret: &state.deps.occupant_id_secret,
        },
        response,
    )
    .await
    .map_err(|error| {
        warn!(
            room = %room_jid,
            error = ?error,
            "Extension bot groupchat dispatch failed"
        );
        error
    })?;
    outcome.frames.extend(nested.outcome.frames);
    outcome.close = outcome.close || nested.outcome.close;
    outcome.feedback.extend(nested.outcome.feedback);
    Ok(ExtensionRoomDispatchResult {
        outcome,
        stanza_id: nested.stanza_id,
    })
}

#[cfg(test)]
pub(super) fn available_bot_nick(occupants: &[OccupantSnapshot]) -> String {
    available_bot_nick_with_base(occupants, "waddle")
}

pub(super) fn available_bot_nick_with_base(
    occupants: &[OccupantSnapshot],
    preferred_base: &str,
) -> String {
    let base = valid_bot_nick_base(preferred_base);
    if !occupants.iter().any(|occupant| occupant.nick == base) {
        return base;
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if jid::ResourcePart::new(candidate.as_str()).is_ok()
            && !occupants.iter().any(|occupant| occupant.nick == candidate)
        {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search always returns")
}

fn valid_bot_nick_base(preferred_base: &str) -> String {
    let trimmed = preferred_base.trim();
    if !trimmed.is_empty() && jid::ResourcePart::new(trimmed).is_ok() {
        return trimmed.to_string();
    }

    let mut sanitized = String::new();
    let mut previous_dash = false;
    for ch in trimmed.chars() {
        let replacement = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            Some(ch)
        } else if ch.is_whitespace() || matches!(ch, '/' | '\\' | ':' | '@') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = replacement else {
            continue;
        };
        if ch == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        sanitized.push(ch);
    }

    let sanitized = sanitized.trim_matches('-');
    if !sanitized.is_empty() && jid::ResourcePart::new(sanitized).is_ok() {
        sanitized.to_string()
    } else {
        "waddle".to_string()
    }
}
