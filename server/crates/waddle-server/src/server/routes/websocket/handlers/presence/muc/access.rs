use super::*;

pub(super) async fn resolve_managed_channel_affiliation(
    state: &WebSocketState,
    session: &Session,
    channel_id: &str,
) -> Result<Option<Affiliation>, ()> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(&session.user_jid);
    if check_channel_permission(
        state,
        object.clone(),
        subject.clone(),
        Permission::Custom("outcast".into()),
    )
    .await?
    {
        return Ok(Some(Affiliation::Outcast));
    }

    for (permission, affiliation) in [
        (Permission::Owner, Affiliation::Owner),
        (Permission::Admin, Affiliation::Admin),
        (Permission::Member, Affiliation::Member),
    ] {
        if check_channel_permission(state, object.clone(), subject.clone(), permission).await? {
            return Ok(Some(affiliation));
        }
    }

    if matches!(channel_id, "chat" | "announcements" | "github-actions") {
        let server = Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID);
        if check_channel_permission(state, server.clone(), subject.clone(), Permission::Owner)
            .await?
        {
            return Ok(Some(Affiliation::Owner));
        }
        if check_channel_permission(state, server.clone(), subject.clone(), Permission::Admin)
            .await?
        {
            return Ok(Some(Affiliation::Admin));
        }
        if check_channel_permission(state, server, subject.clone(), Permission::Member).await? {
            return Ok(Some(Affiliation::Member));
        }
    }

    if check_channel_permission(state, object, subject, Permission::Read).await? {
        return Ok(Some(Affiliation::Member));
    }
    Ok(None)
}

async fn check_channel_permission(
    state: &WebSocketState,
    object: Object,
    subject: Subject,
    permission: Permission,
) -> Result<bool, ()> {
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject,
            permission,
            object,
        })
        .await
        .map_err(|error| {
            warn!(error = ?error, "Permission actor failed during managed MUC join");
        })?;
    Ok(response.allowed)
}

pub(super) async fn server_permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    permission: Permission,
) -> Result<bool, ()> {
    let Some(session) = session else {
        return Ok(false);
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&session.user_jid),
            permission,
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        })
        .await
        .map(|response| response.allowed)
        .map_err(|error| {
            warn!(error = %error, "Failed to authorize MUC creation");
        })
}

/// Derive the single-space id and channel id from a room's bare JID node.
///
/// Convention: node is the channel id.
/// Falls back to ("default", "default") if the node can't be parsed.
pub fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        return ("space".to_string(), channel_id);
    }
    ("default".to_string(), "default".to_string())
}

pub async fn get_managed_channel_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<XmppChannelRecord>, String> {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(None);
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    get_xmpp_channel(actor, &channel_id).await
}
