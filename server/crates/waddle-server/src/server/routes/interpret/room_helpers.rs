use super::*;

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

pub(super) fn normalize_thread_create_source(message: &mut Message) -> Option<String> {
    let Some(ForumAction::CreateThread(_)) = extract_forum_action(message) else {
        return None;
    };
    let thread_id = message
        .thread
        .as_ref()
        .map(|thread| thread.0.clone())
        .or_else(|| message.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if message.id.is_none() {
        message.id = Some(thread_id.clone());
    }
    if message.thread.is_none() {
        set_thread_id(message, &thread_id);
    }
    Some(thread_id)
}

#[cfg(test)]
pub(super) fn message_thread_id(message: &Message) -> Option<String> {
    message
        .thread
        .as_ref()
        .map(|thread| thread.0.clone())
        .or_else(|| {
            extract_forum_action(message).and_then(|action| match action {
                ForumAction::Reply(reply) => Some(reply.thread_id),
                ForumAction::CreateThread(_) => message.id.clone(),
            })
        })
}

/// Resolve the managed-room owner override against the deployment
/// permission actor. Mirrors the legacy
/// `session_is_server_owner` helper that lived on the legacy MUC
/// bridge — kept here so the room handler chain can stay synchronous
/// and the async permission-actor call lands in the interpreter.
pub(super) async fn session_is_server_owner(
    state: &WebSocketState,
    session: Option<&Session>,
) -> bool {
    let Some(session) = session else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            subject: Subject::user(&session.user_id),
            permission: Permission::Owner,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

pub(super) async fn enrich_message_event(deps: &Deps<'_>, message: Message) -> Message {
    if deps.extension_manager.is_none() {
        debug!(
            "RequestEnrichment: no extension_manager in Deps; \
             feeding original message back unchanged"
        );
        return message;
    }
    debug!("RequestEnrichment: direct messages do not carry a typed Waddle scope; skipping");
    message
}
