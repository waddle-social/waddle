use super::{
    debug, get_room_actor, get_xmpp_channel, warn, Affiliation, BareJid, CheckPermission, Element,
    GetSnapshot, Object, ObjectType, Permission, Subject, WebSocketState, XmppChannelRecord,
    DEPLOYMENT_SERVER_ID,
};
use crate::permissions::{
    DeleteTuple, ListRelations, ListSubjects, PermissionError, Relation, SubjectType, Tuple,
    WriteTuple,
};
use crate::server::routes::websocket::ResolvedPrincipal;

/// A managed channel's entry decision for one account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedChannelAffiliation {
    /// The affiliation the account enters with; `None` when it holds none.
    pub(crate) affiliation: Option<Affiliation>,
    /// Whether the room requires membership. A room an enrolled host-grant
    /// scope manages always does, whatever its stored configuration says.
    pub(crate) members_only: bool,
    /// Whether an enrolled host-grant scope manages the room, so that the
    /// affiliation is its canonical standing and nothing else.
    pub(crate) host_grant_managed: bool,
}

pub(crate) async fn resolve_managed_channel_affiliation(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
    channel_id: &str,
    members_only: bool,
    allow_repairs: bool,
) -> Result<ManagedChannelAffiliation, ()> {
    resolve_managed_channel_admission(
        state,
        user_jid,
        room_jid,
        channel_id,
        members_only,
        allow_repairs,
    )
    .await
    .map(|(affiliation, _)| affiliation)
}

/// [`resolve_managed_channel_affiliation`] together with the
/// revocation-fence ticket of the decision, for a room an enrolled
/// host-grant scope manages: the caller that goes on to seat the account
/// acts on the decision only while the ticket is current.
pub(crate) async fn resolve_managed_channel_admission(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
    channel_id: &str,
    members_only: bool,
    allow_repairs: bool,
) -> Result<
    (
        ManagedChannelAffiliation,
        Option<crate::permissions::host_grant::FenceTicket>,
    ),
    (),
> {
    if let Some((affiliation, ticket)) = host_grant_admission(state, user_jid, room_jid).await? {
        return Ok((
            ManagedChannelAffiliation {
                affiliation,
                members_only: true,
                host_grant_managed: true,
            },
            Some(ticket),
        ));
    }
    let affiliation = resolve_legacy_channel_affiliation(
        state,
        user_jid,
        room_jid,
        channel_id,
        members_only,
        allow_repairs,
    )
    .await?;
    Ok((
        ManagedChannelAffiliation {
            affiliation,
            members_only,
            host_grant_managed: false,
        },
        None,
    ))
}

/// The affiliation a room that an enrolled host-grant scope manages enters
/// `user_jid` with, decided from the canonical grants alone: `Outcast` for a
/// recorded or effective ban, the honoured standing of a grant that allows
/// reading the room, and `None` otherwise (the room is members-only).
/// `Ok(None)` when no scope manages the room; `Err` when that cannot be
/// decided, which fails the caller closed.
///
/// Legacy channel tuples, Space membership, deployment roles and the room
/// actor's affiliations are not consulted, and no Space-parent repair runs
/// for such a room.
pub(crate) async fn host_grant_affiliation(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
) -> Result<Option<Option<Affiliation>>, ()> {
    host_grant_admission(state, user_jid, room_jid)
        .await
        .map(|admission| admission.map(|(affiliation, _)| affiliation))
}

/// [`host_grant_affiliation`] together with the revocation-fence ticket the
/// grants were read under. The standing is `None` once the ticket is no
/// longer current, and a caller whose effect follows the decision later
/// asks the ticket again where the effect happens.
pub(crate) async fn host_grant_admission(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
) -> Result<
    Option<(
        Option<Affiliation>,
        crate::permissions::host_grant::FenceTicket,
    )>,
    (),
> {
    use crate::permissions::host_grant::{RoomAffiliation, RoomBinding, RoomGate};
    match state
        .deps
        .app_state
        .host_grants
        .room(RoomBinding::Room(room_jid))
        .await
    {
        RoomGate::Unmanaged => Ok(None),
        RoomGate::Unavailable => Err(()),
        RoomGate::Managed(room) => Ok(Some((
            match room.standing_as(
                user_jid,
                &crate::permissions::host_grant::PrincipalEvidence::NoStream,
            ) {
                RoomAffiliation::Outcast => Some(Affiliation::Outcast),
                RoomAffiliation::None => None,
                RoomAffiliation::Member => Some(Affiliation::Member),
                RoomAffiliation::Admin => Some(Affiliation::Admin),
                RoomAffiliation::Owner => Some(Affiliation::Owner),
            },
            room.fence,
        ))),
    }
}

async fn resolve_legacy_channel_affiliation(
    state: &WebSocketState,
    user_jid: &BareJid,
    room_jid: &BareJid,
    channel_id: &str,
    members_only: bool,
    allow_repairs: bool,
) -> Result<Option<Affiliation>, ()> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(user_jid.to_string());
    if let Some(affiliation) =
        direct_channel_affiliation(state, object.clone(), subject.clone()).await?
    {
        return Ok(Some(affiliation));
    }

    if !members_only {
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
            return Ok(Some(Affiliation::None));
        }
    }

    if check_channel_permission(state, object.clone(), subject.clone(), Permission::Read).await? {
        return Ok(read_access_affiliation(members_only));
    }

    // The parent-tuple repair below issues `WriteTuple`/`DeleteTuple`
    // effects. Join admission opts in (`allow_repairs = true`) so a channel
    // missing its Space parent regains it on the way into the room. The
    // read-only MAM archive gate opts out: an archive query must not
    // mutate the permission graph. A member whose Space-inherited read
    // access hinges on a broken parent tuple is denied here until their
    // next join repairs it; explicit channel affiliations (resolved by
    // `direct_channel_affiliation` above) are unaffected.
    if allow_repairs {
        restore_space_parent_tuples_for_room(state, room_jid, channel_id).await?;
        if check_channel_permission(state, object, subject, Permission::Read).await? {
            return Ok(read_access_affiliation(members_only));
        }
    }
    Ok(None)
}

/// XEP-0313 §5.1 archive-access decision for a MUC room MAM query.
pub enum RoomArchiveAccess {
    Allowed,
    Denied,
    Error,
}

/// XEP-0313 §5.1: "A MUC archive MUST check that the user requesting the
/// archive has the right to enter it at the time of the query and only
/// allow access if so." The decision mirrors join admission: members-only
/// rooms require at least Member affiliation, open rooms admit any
/// non-outcast. An unmanaged room without a live actor has no admission
/// data, so it fails closed.
///
/// `channel` is the managed-channel row for `room_jid` (or `None` for an
/// unmanaged room), resolved once by the caller and shared with
/// `group_dm_archive_visibility` so a single MAM query does not fetch the
/// same row twice.
pub async fn resolve_muc_room_archive_access(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&BareJid>,
    channel: Option<&XmppChannelRecord>,
) -> RoomArchiveAccess {
    let Some(requester) = requester else {
        return RoomArchiveAccess::Denied;
    };

    // A room an enrolled host-grant scope manages is decided from its
    // canonical grants alone, whether or not it has a channel row or a live
    // actor.
    match host_grant_affiliation(state, requester, room_jid).await {
        Ok(None) => {}
        Ok(Some(affiliation)) => return host_grant_entry(affiliation),
        Err(()) => return RoomArchiveAccess::Error,
    }

    let snapshot = match get_room_actor(state, room_jid).await {
        Some(actor) => match actor.ask(GetSnapshot).await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                warn!(
                    room = %room_jid,
                    requester = %requester,
                    error = ?error,
                    "Failed to snapshot room for MAM access gate"
                );
                return RoomArchiveAccess::Error;
            }
        },
        None => None,
    };

    if let Some(channel) = channel {
        // Same precedence as join admission: a live actor's config wins
        // over a stale channel row.
        let members_only = snapshot
            .as_ref()
            .map(|snapshot| snapshot.room.config.members_only)
            .unwrap_or(channel.members_only);
        return resolve_managed_channel_entry_access(
            state,
            room_jid,
            requester,
            channel,
            members_only,
        )
        .await;
    }

    // Unmanaged room (instant room, no channel row). Admission data lives
    // only in the live actor's in-memory affiliation list and config; both
    // are lost when the room is evicted. If no actor is live we cannot
    // authorize the request, so we deliberately fail closed rather than
    // fail open: an instant room reconfigured members-only before eviction
    // would otherwise leak its archive to non-members (the #1093 bypass).
    // Managed channels — the norm in Waddle — always resolve above via the
    // persisted channel row and are unaffected.
    match snapshot {
        Some(snapshot) if snapshot.room.can_user_join(requester) => RoomArchiveAccess::Allowed,
        Some(_) => {
            debug!(
                room = %room_jid,
                requester = %requester,
                "MAM archive denied: requester may not enter unmanaged room"
            );
            RoomArchiveAccess::Denied
        }
        None => {
            debug!(
                room = %room_jid,
                requester = %requester,
                "MAM archive denied: no live actor for unmanaged room (fail-closed)"
            );
            RoomArchiveAccess::Denied
        }
    }
}

/// Right-to-enter decision for a managed channel, given the room's
/// already-frozen `members_only` setting (a live actor's config wins over
/// the persisted channel row; the caller resolves that precedence once).
///
/// Members-only rooms require at least Member affiliation; open rooms
/// admit any non-outcast, including permission-graph `outcast` bans.
/// Read-only: it never repairs or mutates the permission graph. Shared by
/// the XEP-0313 §5.1 archive gate and the groupchat mention audience so
/// both apply the same entry rule.
pub async fn resolve_managed_channel_entry_access(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: &BareJid,
    channel: &XmppChannelRecord,
    members_only: bool,
) -> RoomArchiveAccess {
    match resolve_managed_channel_affiliation(
        state,
        requester,
        room_jid,
        &channel.id,
        members_only,
        // Read-only gate: never mutate the permission graph.
        false,
    )
    .await
    {
        Ok(ManagedChannelAffiliation {
            affiliation: Some(Affiliation::Outcast),
            ..
        }) => RoomArchiveAccess::Denied,
        Ok(ManagedChannelAffiliation {
            affiliation: Some(_),
            ..
        }) => RoomArchiveAccess::Allowed,
        Ok(ManagedChannelAffiliation {
            affiliation: None,
            members_only: true,
            ..
        }) => RoomArchiveAccess::Denied,
        Ok(ManagedChannelAffiliation {
            affiliation: None,
            members_only: false,
            ..
        }) => RoomArchiveAccess::Allowed,
        Err(()) => RoomArchiveAccess::Error,
    }
}

/// Whether `requester` may still read a room an enrolled host-grant scope
/// manages, decided again after a protected read of it: `None` for a room
/// no scope manages, whose read keeps its single check. A response is sent
/// only if this read is admitted, so a page read after a revoke that
/// committed before this decision is never sent.
pub async fn host_grant_read_still_admitted(
    state: &WebSocketState,
    room_jid: &BareJid,
    requester: Option<&BareJid>,
) -> Option<bool> {
    let requester = requester?;
    match host_grant_affiliation(state, requester, room_jid).await {
        Ok(None) => None,
        Ok(Some(affiliation)) => Some(matches!(
            host_grant_entry(affiliation),
            RoomArchiveAccess::Allowed
        )),
        Err(()) => Some(false),
    }
}

/// The right to enter a room an enrolled host-grant scope manages: a
/// standing that allows reading it, never a ban and never mere presence.
fn host_grant_entry(affiliation: Option<Affiliation>) -> RoomArchiveAccess {
    match affiliation {
        Some(Affiliation::Owner | Affiliation::Admin | Affiliation::Member) => {
            RoomArchiveAccess::Allowed
        }
        Some(Affiliation::Outcast | Affiliation::None) | None => RoomArchiveAccess::Denied,
    }
}

async fn direct_channel_affiliation(
    state: &WebSocketState,
    object: Object,
    subject: Subject,
) -> Result<Option<Affiliation>, ()> {
    let relations = state
        .deps
        .app_state
        .permission_actor
        .ask(ListRelations { subject, object })
        .await
        .map_err(|error| {
            warn!(error = ?error, "Failed to load direct managed MUC affiliation relations");
        })?;

    let affiliation = ["outcast", "owner", "admin", "member"]
        .into_iter()
        .find(|relation| relations.iter().any(|actual| actual.name == *relation))
        .and_then(|relation| match relation {
            "outcast" => Some(Affiliation::Outcast),
            "owner" => Some(Affiliation::Owner),
            "admin" => Some(Affiliation::Admin),
            "member" => Some(Affiliation::Member),
            _ => None,
        });

    Ok(affiliation)
}

fn read_access_affiliation(members_only: bool) -> Option<Affiliation> {
    if members_only {
        None
    } else {
        Some(Affiliation::None)
    }
}

/// Give a channel that has no parent the Space its link names, when that
/// Space lists the channel's bookmark. A channel that already has a parent
/// keeps it, whichever Space the link or a bookmark names: a join is not
/// authority over the channel, and only a placement moves it.
async fn restore_space_parent_tuples_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
) -> Result<(), ()> {
    let parents = state
        .deps
        .app_state
        .permission_actor
        .ask(ListSubjects {
            object: Object::new(ObjectType::Channel, channel_id),
            relation: Relation::new("parent"),
        })
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                error = ?error,
                "Failed to read channel parents while repairing managed MUC permissions"
            );
        })?;
    if !parents.is_empty() {
        return Ok(());
    }
    let Some(node) = linked_space_node(state, room_jid, channel_id).await? else {
        return Ok(());
    };
    let item_id = room_jid.to_string();
    if !space_node_has_room_bookmark(state, room_jid, channel_id, &node, &item_id).await? {
        return Ok(());
    }
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, &node, ""),
    );
    match state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: tuple.clone(),
        })
        .await
    {
        Ok(()) => {}
        // Written by a concurrent repair or placement: not this call's to undo.
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {
            return Ok(());
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                channel_id,
                node,
                error = ?error,
                "Failed to repair managed MUC channel parent tuple"
            );
            return Err(());
        }
    }
    if !space_node_has_room_bookmark(state, room_jid, channel_id, &node, &item_id).await? {
        match state
            .deps
            .app_state
            .permission_actor
            .ask(DeleteTuple { tuple })
            .await
        {
            Ok(()) | Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    channel_id,
                    node,
                    error = ?error,
                    "Failed to clean up stale managed MUC channel parent tuple after revalidation"
                );
                return Err(());
            }
        }
    }

    Ok(())
}

async fn space_node_has_room_bookmark(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
    node: &str,
    item_id: &str,
) -> Result<bool, ()> {
    let item_filter = [item_id.to_string()];
    let items = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(
            &state.deps.app_state.spaces_jid,
            node,
            Some(1),
            &item_filter,
        )
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                node,
                error = %error,
                "Failed to load Spaces bookmark while repairing managed MUC permissions"
            );
        })?;
    let Some(item) = items.first() else {
        return Ok(false);
    };
    let Some(payload) = item
        .payload_xml
        .as_ref()
        .and_then(|xml| xml.parse::<Element>().ok())
    else {
        return Ok(false);
    };
    let Ok(bookmark) = waddle_xmpp::xep::xep0402::parse_bookmark(&item.id, &payload) else {
        return Ok(false);
    };
    Ok(bookmark.jid == *room_jid)
}

/// The Space node the channel's link names, on this deployment's Spaces
/// service.
async fn linked_space_node(
    state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
) -> Result<Option<String>, ()> {
    let link = state
        .deps
        .app_state
        .channel_space_link_store
        .get(room_jid)
        .await
        .map_err(|error| {
            warn!(
                room = %room_jid,
                channel_id,
                error = %error,
                "Failed to read channel-space projection while repairing managed MUC permissions"
            );
        })?;
    let Some(link) = link else {
        return Ok(None);
    };
    if link.space_jid.domain() != state.deps.app_state.spaces_jid.domain() {
        warn!(
            room = %room_jid,
            channel_id,
            linked_space = %link.space_jid,
            spaces = %state.deps.app_state.spaces_jid,
            "Refusing to repair managed MUC Space parent from a different Spaces service"
        );
        return Ok(None);
    }
    let node = link.space_jid.node().map(|node| node.to_string());
    if node.is_none() {
        warn!(
            room = %room_jid,
            channel_id,
            linked_space = %link.space_jid,
            "Refusing to repair managed MUC Space parent without a Space node"
        );
    }
    Ok(node)
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
    principal: Option<ResolvedPrincipal<'_>>,
    permission: Permission,
) -> Result<bool, ()> {
    let Some(principal) = principal else {
        return Ok(false);
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&principal.user_jid),
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
