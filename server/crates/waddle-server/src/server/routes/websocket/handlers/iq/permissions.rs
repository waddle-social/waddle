use super::{
    build_config_form, build_iq_error_xml_typed, build_iq_result_xml,
    server_policy_for_managed_channel, Affiliation, Arc, BareJid, CheckPermission, Element,
    FullJid, GetSnapshot, ManagedChannelServerPolicy, Object, ObjectType, Permission,
    PermissionError, Relation, Session, SpaceAffiliation, StanzaErrorCondition, StanzaErrorType,
    Subject, SubjectType, Tuple, WebSocketState, WriteTuple, XmppError,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS, DEPLOYMENT_SERVER_ID,
};
use crate::permissions::DeleteTuple;
use crate::server::routes::websocket::cleanup::get_room_actor_result;
use crate::server::routes::websocket::ResolvedPrincipal;
use std::collections::BTreeMap;

pub(super) fn build_xmpp_error_response(
    request_iq: &xmpp_parsers::iq::Iq,
    err: XmppError,
) -> String {
    let error = match err {
        XmppError::Stanza {
            condition,
            error_type,
            text,
        } => stanza_error_from_parts(error_type, condition, text, None),
        // XEP-0050 §4.4: render the command-specific condition child in
        // the commands namespace alongside the mapped general condition.
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
    build_iq_error_xml_typed(
        request_iq.id(),
        request_iq.to().map(|jid| jid.to_string()).as_deref(),
        request_iq.from().map(|jid| jid.to_string()).as_deref(),
        error,
    )
}

/// Build a typed [`StanzaError`] from Waddle's typed condition/type enums,
/// omitting the `<text/>` child entirely when no diagnostic is supplied
/// (matching the prior wire shape) and attaching an optional
/// protocol-specific extension element (e.g. a XEP-0050 §4.4 condition).
fn stanza_error_from_parts(
    error_type: StanzaErrorType,
    condition: StanzaErrorCondition,
    text: Option<String>,
    other: Option<xmpp_parsers::minidom::Element>,
) -> xmpp_parsers::stanza_error::StanzaError {
    use xmpp_parsers::stanza_error::StanzaError;
    let mut error = match text {
        Some(text) => StanzaError::new(error_type.to_xmpp(), condition.to_xmpp(), "en", text),
        None => StanzaError {
            type_: error_type.to_xmpp(),
            by: None,
            defined_condition: condition.to_xmpp(),
            texts: BTreeMap::new(),
            other: None,
        },
    };
    error.other = other;
    error
}

pub(super) async fn permission_allowed(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    object: Object,
    permission: Permission,
) -> Result<bool, String> {
    let Some(principal) = principal else {
        return Ok(false);
    };
    let response = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&principal.user_jid),
            permission,
            object,
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    Ok(response.allowed)
}

pub(super) async fn server_permission_allowed(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    permission: Permission,
) -> Result<bool, String> {
    permission_allowed(
        state,
        principal,
        Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        permission,
    )
    .await
}

pub(crate) async fn managed_channel_permission_allowed(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    channel_id: &str,
    permission: Permission,
) -> Result<bool, String> {
    let Some(resolved) = principal else {
        return Ok(false);
    };
    if let Some(allowed) = crate::server::managed_channel_policy::host_grant_channel_permission(
        &state.deps.app_state.host_grants,
        &resolved.user_jid,
        channel_id,
        &permission,
    )
    .await?
    {
        return Ok(allowed);
    }
    let policy = server_policy_for_managed_channel(channel_id, &permission);
    if policy == ManagedChannelServerPolicy::DeploymentOwnerOnly {
        return server_permission_allowed(state, principal, Permission::Owner).await;
    }

    if permission_allowed(
        state,
        principal,
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
            if server_permission_allowed(state, principal, server_permission).await? {
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
    principal: Option<ResolvedPrincipal<'_>>,
) -> Option<SpaceAffiliation> {
    let principal = principal?;
    let bare_jid = session_bare_jid(state, principal)?;
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
fn session_bare_jid(state: &WebSocketState, principal: ResolvedPrincipal<'_>) -> Option<BareJid> {
    let raw = format!(
        "{}@{}",
        principal.xmpp_localpart, state.deps.auth_state.xmpp_domain
    );
    raw.parse().ok()
}

pub(super) async fn space_affiliation_for_requester(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    node: &str,
) -> Option<SpaceAffiliation> {
    if server_permission_allowed(state, principal, Permission::Owner)
        .await
        .unwrap_or(false)
    {
        return Some(SpaceAffiliation::Owner);
    }
    if permission_allowed(
        state,
        principal,
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
        principal,
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
        principal,
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

async fn write_tuple_if_absent_status(
    state: &WebSocketState,
    tuple: Tuple,
) -> Result<bool, String> {
    match state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple { tuple })
        .await
    {
        Ok(()) => Ok(true),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {
            Ok(false)
        }
        Err(error) => Err(format!("permission actor failed writing tuple: {error}")),
    }
}

pub(super) async fn write_tuple_if_absent(
    state: &WebSocketState,
    tuple: Tuple,
) -> Result<(), String> {
    write_tuple_if_absent_status(state, tuple).await.map(|_| ())
}

pub(super) async fn spaces_node_mutation_allowed(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    node: &str,
) -> Result<bool, String> {
    if server_permission_allowed(state, principal, Permission::CreateSpace).await? {
        return Ok(true);
    }
    permission_allowed(
        state,
        principal,
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
            Subject::user(&session.user_jid),
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
    // A privileged seed names an account of this deployment's account
    // domain or of a trusted home of the S2S snapshot it runs (ADR-0020
    // D8). An account domain that yields no issuance policy seeds nothing.
    let Some(issuance) = crate::spaces_pubsub_seed::replica_seed_issuance(
        &state.deps.app_state,
        &state.deps.auth_state.xmpp_domain,
    ) else {
        tracing::warn!(
            kind = "issuance_policy_invalid",
            "Refused to seed Spaces node owners: the account domain yields no issuance policy"
        );
        return;
    };
    crate::spaces_pubsub_seed::seed_owners_on_node(
        &state.deps.protocol.pubsub_storage,
        spaces_jid,
        node,
        &owners,
        crate::spaces_pubsub_seed::OwnerSeedPolicy {
            issuance: &issuance,
            host_grants: &state.deps.app_state.host_grants,
        },
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
    write_channel_parent_tuple_if_absent(state, channel_id, space_node)
        .await
        .map(|_| ())
}

pub(super) async fn write_channel_parent_tuple_if_absent(
    state: &WebSocketState,
    channel_id: &str,
    space_node: &str,
) -> Result<bool, String> {
    write_tuple_if_absent_status(
        state,
        Tuple::new(
            Object::new(ObjectType::Channel, channel_id),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, space_node, ""),
        ),
    )
    .await
}

/// The Space nodes the channel's parent relationships name.
pub(super) async fn channel_parent_spaces(
    state: &WebSocketState,
    channel_id: &str,
) -> Result<Vec<String>, String> {
    state
        .deps
        .app_state
        .permission_actor
        .ask(crate::permissions::ListSubjects {
            object: Object::new(ObjectType::Channel, channel_id),
            relation: Relation::new("parent"),
        })
        .await
        .map(|parents| {
            parents
                .into_iter()
                .filter(|parent| parent.subject_type == SubjectType::Space)
                .map(|parent| parent.id)
                .collect()
        })
        .map_err(|error| format!("channel parent lookup failed: {error}"))
}

pub(super) async fn delete_channel_parent_tuple(
    state: &WebSocketState,
    channel_id: &str,
    space_node: &str,
) -> Result<bool, String> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, space_node, ""),
    );
    match state
        .deps
        .app_state
        .permission_actor
        .ask(DeleteTuple { tuple })
        .await
    {
        Ok(()) => Ok(true),
        Err(kameo::error::SendError::HandlerError(
            crate::permissions::PermissionError::TupleNotFound,
        )) => Ok(false),
        Err(error) => Err(format!("permission actor failed deleting tuple: {error}")),
    }
}

/// Whether `sender_jid` may perform `action` on `room_jid`: a room an
/// enrolled host-grant scope manages is decided by the canonical grants
/// alone (no room affiliation confers `manage` or `delete` there); any
/// other room by the sender's honoured owner affiliation.
pub(super) async fn room_action_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    session: Option<&Session>,
    evidence: &crate::permissions::host_grant::PrincipalEvidence,
    action: crate::permissions::host_grant::HostGrantAction,
) -> Result<bool, XmppError> {
    use crate::permissions::host_grant::{RoomBinding, RoomGate};
    match state
        .deps
        .app_state
        .host_grants
        .allows_as(
            RoomBinding::Room(room_jid),
            &sender_jid.to_bare(),
            evidence,
            action,
        )
        .await
    {
        RoomGate::Unmanaged => {
            muc_owner_authorized(state, room_jid, sender_jid, session, evidence).await
        }
        RoomGate::Managed(allowed) => Ok(allowed),
        RoomGate::Unavailable => Err(XmppError::internal(
            "the host-grant authority is unavailable",
        )),
    }
}

/// Whether a room no enrolled scope manages holds `sender_jid` as an
/// owner, as that standing is honoured for a request that reached the room
/// with `evidence`: as stored for a session of this deployment, and for a
/// remote principal only for a trusted home on a `pkix` stream (ADR-0020
/// D8).
async fn muc_owner_authorized(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    _session: Option<&Session>,
    evidence: &crate::permissions::host_grant::PrincipalEvidence,
) -> Result<bool, XmppError> {
    let room_actor = get_room_actor_result(state, room_jid)
        .await
        .map_err(|error| XmppError::internal(format!("room lookup failed: {error}")))?
        .ok_or_else(|| XmppError::internal("room actor not found"))?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| XmppError::internal(format!("snapshot failed: {error:?}")))?;
    let account = sender_jid.to_bare();
    let honoured =
        crate::server::routes::websocket::handlers::presence::honoured_legacy_affiliation(
            state,
            &account,
            evidence,
            Some(snapshot.room.affiliation(&account)),
        );
    Ok(honoured == Some(Affiliation::Owner))
}

pub(super) async fn build_muc_owner_config_response(
    state: &WebSocketState,
    room_jid: &BareJid,
    id: &str,
    response_to: Option<&str>,
) -> Result<String, XmppError> {
    let room_actor = get_room_actor_result(state, room_jid)
        .await
        .map_err(|error| XmppError::internal(format!("room lookup failed: {error}")))?
        .ok_or_else(|| XmppError::internal("room actor not found"))?;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| XmppError::internal(format!("snapshot failed: {error:?}")))?;
    let mut room = snapshot.room;
    if let Some(channel_type) =
        super::muc_owner_config::managed_channel_type_for_room(state, room_jid)
            .await
            .map_err(XmppError::internal)?
    {
        super::muc_owner_config::project_channel_type_to_config(&mut room.config, channel_type);
    }
    let form = build_config_form(&room);
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
