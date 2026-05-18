use super::*;

pub(super) fn build_xmpp_error_response(
    request_iq: &xmpp_parsers::iq::Iq,
    err: XmppError,
) -> String {
    match err {
        XmppError::Stanza {
            condition,
            error_type,
            text,
        } => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            condition,
            error_type,
            text.as_deref(),
        ),
        other => waddle_xmpp::generate_iq_error(
            &request_iq.id,
            request_iq
                .from
                .as_ref()
                .map(|jid| jid.to_string())
                .as_deref(),
            request_iq.to.as_ref().map(|jid| jid.to_string()).as_deref(),
            StanzaErrorCondition::InternalServerError,
            StanzaErrorType::Wait,
            Some(&other.to_string()),
        ),
    }
}

pub(super) async fn global_database(
    state: &WebSocketState,
) -> Result<Database, RosterStorageError> {
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
        .map_err(|error| RosterStorageError::ConnectionFailed(error.to_string()))
}

pub(super) async fn permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    object: Object,
    permission: Permission,
) -> Result<bool, String> {
    let Some(session) = session else {
        return Ok(false);
    };
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&session.user_id),
            permission,
            object,
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    Ok(response.allowed)
}

pub(super) async fn server_permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    permission: Permission,
) -> Result<bool, String> {
    permission_allowed(
        state,
        session,
        Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        permission,
    )
    .await
}

pub(crate) async fn managed_channel_permission_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    channel_id: &str,
    permission: Permission,
) -> Result<bool, String> {
    let policy = server_policy_for_managed_channel(channel_id, &permission);
    if policy == ManagedChannelServerPolicy::DeploymentOwnerOnly {
        return server_permission_allowed(state, session, Permission::Owner).await;
    }

    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Channel, channel_id),
        permission.clone(),
    )
    .await?
    {
        return Ok(true);
    }

    if policy == ManagedChannelServerPolicy::DeploymentMembership {
        // Keep these as explicit relation/permission checks. The local permission
        // schema makes `member` inherit owner/admin, but the SpiceDB schema uses
        // server relations directly for compatibility.
        for server_permission in DEPLOYMENT_MEMBERSHIP_PERMISSIONS {
            if server_permission_allowed(state, session, server_permission).await? {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    Ok(false)
}

/// Derive the requester's server-level [`SpaceAffiliation`] from the
/// same dynamic community-owner signal that gates every admin command.
///
/// After #696 the admin ACL is "PubSub-Owner row on the Spaces JID, or
/// bootstrap localpart from `WADDLE_SERVER_OWNER_LOCALPARTS`" — see
/// [`crate::admin::is_community_owner`]. This helper drives the
/// `waddle#server_affiliation` field emitted by server-targeted
/// `disco#info` (see [`super::disco_info::server_info`]) so the chat
/// client's `canManageCommunity` UI affordance flips on the same edge
/// as the ACL. Previously the disco field was driven off the Zanzibar
/// permission graph, which could disagree with the PubSub affiliation
/// table for users promoted dynamically.
///
/// Returns:
/// - `Some(Owner)` if [`is_community_owner`] returns `true` for the
///   session's bare JID,
/// - `None` otherwise (including unauthenticated callers and sessions
///   whose JID cannot be derived — the disco field is then omitted
///   rather than asserting a tier that wasn't earned).
///
/// The intermediate Zanzibar-derived tiers (`Publisher`, `Member`)
/// previously surfaced here had no consumer on the chat side; the
/// Zanzibar graph itself remains the authority for per-channel and
/// per-space authorization via [`permission_allowed`] and
/// [`managed_channel_permission_allowed`].
///
/// [`is_community_owner`]: crate::admin::is_community_owner
pub(super) async fn server_affiliation_for_requester(
    state: &WebSocketState,
    session: Option<&Session>,
) -> Option<SpaceAffiliation> {
    let session = session?;
    let bare_jid = session_bare_jid(state, session)?;
    if crate::admin::is_community_owner(&state.deps.app_state, &bare_jid).await {
        Some(SpaceAffiliation::Owner)
    } else {
        None
    }
}

/// Build the session's bare JID from `xmpp_localpart` + the deployment's
/// configured `xmpp_domain`. Returns `None` if the localpart is missing
/// or the combination fails to parse as a [`BareJid`] — both cases are
/// treated as "no derivable identity" rather than panicking, because
/// this helper feeds disco#info responses on a hot path.
fn session_bare_jid(state: &WebSocketState, session: &Session) -> Option<BareJid> {
    let raw = format!(
        "{}@{}",
        session.xmpp_localpart, state.deps.auth_state.xmpp_domain
    );
    raw.parse().ok()
}

pub(super) async fn space_affiliation_for_requester(
    state: &WebSocketState,
    session: Option<&Session>,
    node: &str,
) -> Option<SpaceAffiliation> {
    if server_permission_allowed(state, session, Permission::Owner)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Owner,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Admin,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Publisher);
    }
    if permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Member,
    )
    .await
    .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Member);
    }
    None
}

pub(super) async fn write_tuple_if_absent(
    state: &WebSocketState,
    tuple: Tuple,
) -> Result<(), String> {
    match state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple { tuple })
        .await
    {
        Ok(()) => Ok(()),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(format!("permission actor failed writing tuple: {error}")),
    }
}

pub(super) async fn spaces_node_mutation_allowed(
    state: &WebSocketState,
    session: Option<&Session>,
    node: &str,
) -> Result<bool, String> {
    if server_permission_allowed(state, session, Permission::CreateSpace).await? {
        return Ok(true);
    }
    permission_allowed(
        state,
        session,
        Object::new(ObjectType::Space, node),
        Permission::Owner,
    )
    .await
}

pub(super) async fn write_space_owner_tuple(
    state: &WebSocketState,
    node: &str,
    session: Option<&Session>,
) -> Result<(), String> {
    let Some(session) = session else {
        return Ok(());
    };
    write_tuple_if_absent(
        state,
        Tuple::new(
            Object::new(ObjectType::Space, node),
            Relation::new("owner"),
            Subject::user(&session.user_id),
        ),
    )
    .await
}

/// Seed `Affiliation::Owner` rows on a freshly-created Spaces PubSub node
/// for the creator and every configured server owner. Failures are logged
/// but non-fatal — `<create>` still succeeds. The next reconcile pass at
/// startup repairs any missed seeds.
pub(super) async fn seed_spaces_node_owners(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    node: &str,
    creator: &BareJid,
) {
    let server_owner_jids = Arc::clone(&state.deps.app_state.server_owner_jids);
    let mut owners: Vec<BareJid> = server_owner_jids.iter().cloned().collect();
    if !owners.iter().any(|jid| jid == creator) {
        owners.push(creator.clone());
    }
    if owners.is_empty() {
        return;
    }
    crate::spaces_pubsub_seed::seed_owners_on_node(
        &state.deps.protocol.pubsub_storage,
        spaces_jid,
        node,
        &owners,
    )
    .await;
}

/// Write `channel:<channel_id>#parent → space:<space_node>#` so that all members
/// of the Space gain access to the channel via the permission arrow.
/// Per XEP-0503 §4, a room bookmarked inside a Space node is considered part of
/// that Space; this tuple propagates Space membership into channel access checks.
pub(super) async fn write_channel_parent_tuple(
    state: &WebSocketState,
    channel_id: &str,
    space_node: &str,
) -> Result<(), String> {
    write_tuple_if_absent(
        state,
        Tuple::new(
            Object::new(ObjectType::Channel, channel_id),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, space_node, ""),
        ),
    )
    .await
}

pub(super) async fn muc_owner_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    _session: Option<&Session>,
) -> Result<bool, String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    if matches!(
        snapshot.room.get_affiliation(&sender_jid.to_bare()),
        Affiliation::Owner
    ) {
        return Ok(true);
    }

    Ok(false)
}

pub(super) async fn build_muc_owner_config_response(
    state: &WebSocketState,
    room_jid: &BareJid,
    id: &str,
    response_to: Option<&str>,
) -> Result<String, String> {
    let room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    let form = build_config_form(&snapshot.room);
    let query = Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
        .append(form)
        .build();
    let room_jid_string = room_jid.to_string();
    Ok(build_iq_result_xml(
        id,
        Some(room_jid_string.as_str()),
        response_to,
        Some(query),
    ))
}
