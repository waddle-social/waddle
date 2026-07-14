use super::*;

mod access;
mod xml;

pub use access::{
    get_managed_channel_for_room, parse_room_jid_context, resolve_muc_room_archive_access,
    RoomArchiveAccess,
};

use access::{resolve_managed_channel_affiliation, server_permission_allowed};
use waddle_xmpp::muc::room_actor::GetSnapshot;
use waddle_xmpp::muc::RoomRegistry;
use xml::{
    build_muc_conflict_presence_xml, build_muc_join_presence_stanza, build_muc_self_unavailable_xml,
};
pub(super) use xml::{build_muc_join_presence_xml, build_muc_presence_error_xml, MucJoinPresence};

#[cfg(any(test, feature = "clustering"))]
pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    handle_muc_join_with_ordered_relay(
        state,
        MucJoinRequest {
            domain,
            room_jid,
            sender_jid,
            nick,
            presence_show,
            authenticated_session,
            ordered_relay_origin: None,
        },
    )
    .await
}

pub struct MucJoinRequest<'a> {
    pub domain: &'a str,
    pub room_jid: &'a BareJid,
    pub sender_jid: &'a FullJid,
    pub nick: &'a str,
    pub presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    pub authenticated_session: &'a Option<Session>,
    pub ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
}

struct MucJoinWork<'a> {
    domain: String,
    room_jid: &'a BareJid,
    sender_jid: &'a FullJid,
    nick: String,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    authenticated_session: &'a Option<Session>,
    ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
}

pub async fn handle_muc_join_with_ordered_relay(
    state: &WebSocketState,
    request: MucJoinRequest<'_>,
) -> Vec<String> {
    info!(room = %request.room_jid, nick = %request.nick, sender = %request.sender_jid, "MUC join request");

    handle_muc_join_unlocked(
        state,
        MucJoinWork {
            domain: request.domain.to_string(),
            room_jid: request.room_jid,
            sender_jid: request.sender_jid,
            nick: request.nick.to_string(),
            presence_show: request.presence_show,
            authenticated_session: request.authenticated_session,
            ordered_relay_origin: request.ordered_relay_origin,
        },
    )
    .await
}

/// Best-effort resolver-affiliation sync into an EXISTING live room
/// actor when join admission rejects before any actor message (review
/// F3). Never creates an actor — a rejection must not spawn rooms —
/// and never blocks the rejection on failure: the sync is a staleness
/// repair, the authoritative admission decision was already made by
/// the resolver. The actor-side handler is provenance-aware
/// (`update_affiliation_from_resolver`), so explicit grants survive.
/// The sync shares the `admission_revision` the rejection decision was
/// computed against; the actor refuses it if any admission/affiliation
/// change (e.g. the re-granted user's successful join) landed in
/// between, so a delayed sync can never clear a live occupant's fresh
/// affiliation.
async fn sync_resolver_affiliation_on_rejection(
    existing_room_actor: Option<&kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>>,
    room_jid: &BareJid,
    jid: BareJid,
    affiliation: Affiliation,
    expected_admission_revision: u64,
) {
    let Some(actor) = existing_room_actor else {
        return;
    };
    match actor
        .ask(waddle_xmpp::muc::room_actor::SyncResolverAffiliation {
            jid,
            affiliation,
            expected_admission_revision,
        })
        .await
    {
        Ok(waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::Applied) => {}
        Ok(outcome) => {
            // Best-effort staleness repair only: a refused sync means the
            // room's admission state already moved on (or the actor is
            // sealed) — the newer state wins.
            debug!(
                room = %room_jid,
                outcome = ?outcome,
                "Skipped resolver affiliation sync on rejected join"
            );
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                error = ?error,
                "Failed to sync resolver affiliation into live room actor on rejected join"
            );
        }
    }
}

pub(crate) async fn route_room_presence_to_occupant(
    state: &WebSocketState,
    room_jid: &BareJid,
    recipient: &FullJid,
    stanza: Stanza,
) {
    if try_deliver_registered_remote_resource(state, recipient, &stanza).await {
        return;
    }
    // #1263: `DroppedFull` was previously treated as delivered, so a
    // client whose channel was momentarily full silently missed a room
    // presence and kept a stale occupant roster forever. The frame is
    // provably never enqueued on `DroppedFull`, so retry ONCE
    // immediately — but never sleep: this helper sits inside the
    // sequential join/leave broadcast loops whose non-blocking contract
    // is load-bearing (a zombied consumer must not stall the join path,
    // or "Timed out waiting for self-presence" cascades return; SM
    // review on PR #1277). A persistently full channel surfaces the
    // loss (metric + warn) instead of reporting success — the
    // recipient's roster is stale until its next rejoin/resync, and a
    // genuinely wedged consumer is torn down by the send-stall
    // backstop, whose disconnect cleanup re-syncs occupancy.
    let mut retried = false;
    loop {
        match state
            .deps
            .protocol
            .connection_registry
            .try_send_to(recipient, stanza.clone())
        {
            waddle_xmpp::registry::BroadcastOutcome::Delivered => return,
            waddle_xmpp::registry::BroadcastOutcome::DroppedFull => {
                if !retried {
                    retried = true;
                    continue;
                }
                waddle_xmpp::prometheus::increment_delivery_retry_exhausted_drop();
                warn!(
                    room = %room_jid,
                    recipient = %recipient,
                    "MUC presence fan-out: recipient channel full; dropped — \
                     occupant roster stale until resync"
                );
                return;
            }
            waddle_xmpp::registry::BroadcastOutcome::NotConnected
            | waddle_xmpp::registry::BroadcastOutcome::DroppedClosed => break,
        }
    }
    #[cfg(not(feature = "clustering"))]
    let _ = room_jid;
    #[cfg(feature = "clustering")]
    let deps = {
        let deps =
            crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
        let entity = waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room_jid.to_string(),
        );
        deps.with_ordered_relay_origin(Some(
            crate::server::routes::interpret::OrderedRelayRouteOrigin {
                kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
                    entity.clone(),
                ),
                sender_entity: entity,
                inbound_sequence: 0,
                handoff: None,
            },
        ))
    };
    #[cfg(feature = "clustering")]
    let replies = crate::server::routes::interpret::route_to_connection(
        &deps,
        jid::Jid::from(recipient.clone()),
        Box::new(stanza),
        0,
    )
    .await;
    #[cfg(feature = "clustering")]
    if !replies.is_empty() {
        warn!(
            room = %room_jid,
            recipient = %recipient,
            reply_count = replies.len(),
            "MUC presence fan-out produced unexpected route fallback replies"
        );
    }
}

async fn try_deliver_registered_remote_resource(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> bool {
    #[cfg(feature = "clustering")]
    {
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return false;
        };
        bridge
            .try_deliver_registered_remote_resource(
                target,
                stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
            )
            .await
            .is_some()
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza);
        false
    }
}

#[cfg(feature = "clustering")]
enum RemoteMucJoinDecision {
    Delivered(Vec<Stanza>),
    MaybeCommitted,
}

#[cfg(feature = "clustering")]
enum RemoteMucLeaveDecision {
    Delivered(Vec<Stanza>),
    MaybeCommitted,
    RetryableNoEffect,
    LocalFallback,
}

#[cfg(feature = "clustering")]
fn remote_muc_join_decision(
    outcome: Option<crate::clustering::route_bridge::OrderedRelayMucProxyOutcome>,
) -> Option<RemoteMucJoinDecision> {
    match outcome {
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(replies)) => {
            Some(RemoteMucJoinDecision::Delivered(replies))
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted)
        | Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted) => {
            Some(RemoteMucJoinDecision::MaybeCommitted)
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable)
        | Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Dropped)
        | None => None,
    }
}

#[cfg(feature = "clustering")]
fn remote_muc_leave_decision(
    outcome: Option<crate::clustering::route_bridge::OrderedRelayMucProxyOutcome>,
) -> RemoteMucLeaveDecision {
    match outcome {
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(replies)) => {
            RemoteMucLeaveDecision::Delivered(replies)
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted)
        | Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted) => {
            RemoteMucLeaveDecision::MaybeCommitted
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable)
        | Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Dropped) => {
            RemoteMucLeaveDecision::RetryableNoEffect
        }
        None => RemoteMucLeaveDecision::LocalFallback,
    }
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;

    #[test]
    fn remote_muc_join_decision_suppresses_errors_for_uncertain_commit() {
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                    Stanza::Presence(xmpp_parsers::presence::Presence::new(
                        xmpp_parsers::presence::Type::None,
                    )),
                ]),
            )),
            Some(RemoteMucJoinDecision::Delivered(_))
        ));
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted,
            )),
            Some(RemoteMucJoinDecision::MaybeCommitted)
        ));
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
            )),
            Some(RemoteMucJoinDecision::MaybeCommitted)
        ));
        assert!(remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable,
        ))
        .is_none());
        assert!(remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Dropped,
        ))
        .is_none());
        assert!(remote_muc_join_decision(None).is_none());
    }

    #[test]
    fn remote_muc_join_decision_keeps_delivered_replies() {
        let decision = remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                Stanza::Presence(xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::None,
                )),
            ]),
        ));
        let Some(RemoteMucJoinDecision::Delivered(replies)) = decision else {
            panic!("expected delivered replies");
        };
        assert_eq!(replies.len(), 1);
    }

    #[test]
    fn remote_muc_leave_decision_preserves_membership_for_uncertain_commit() {
        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted,
            )),
            RemoteMucLeaveDecision::MaybeCommitted
        ));
        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
            )),
            RemoteMucLeaveDecision::MaybeCommitted
        ));
    }

    #[test]
    fn remote_muc_leave_decision_clears_only_on_delivered() {
        let decision = remote_muc_leave_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                Stanza::Presence(xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::None,
                )),
            ]),
        ));
        let RemoteMucLeaveDecision::Delivered(replies) = decision else {
            panic!("expected delivered replies");
        };
        assert_eq!(replies.len(), 1);

        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable,
            )),
            RemoteMucLeaveDecision::RetryableNoEffect
        ));
        assert!(matches!(
            remote_muc_leave_decision(None),
            RemoteMucLeaveDecision::LocalFallback
        ));
    }
}

async fn handle_muc_join_unlocked(state: &WebSocketState, request: MucJoinWork<'_>) -> Vec<String> {
    let MucJoinWork {
        domain,
        room_jid,
        sender_jid,
        nick,
        presence_show,
        authenticated_session,
        ordered_relay_origin,
    } = request;
    #[cfg(not(feature = "clustering"))]
    let _ = &ordered_relay_origin;

    // Resolver-derived first joins bump the admission revision, so a
    // burst of concurrent first-time joiners can hit several stale
    // revisions in a row — allow a few re-snapshots before giving up
    // (each retry re-reads the current revision; convergence is
    // guaranteed once admissions quiesce).
    // 10 bounds a pathological revision-churn loop while making spurious
    // failure implausible for realistic bursts: each retry re-snapshots the
    // CURRENT revision, so a retry only fails when yet another admission
    // landed inside that single snapshot-to-ask window.
    const MAX_STALE_ADMISSION_RETRIES: u32 = 10;
    let mut stale_admission_retries = 0u32;
    // #1108: a room actor can be sealed+destroyed by the guarded
    // dormancy eviction between our registry lookup and the join ask.
    // The seal refuses the join with a typed retryable error (or the
    // ask fails outright on the stopped actor); one retry re-runs the
    // registry lookup, which respawns the room — the join must never
    // be silently dropped.
    let mut retried_dead_room = false;
    loop {
        let managed_channel = match get_managed_channel_for_room(state, room_jid).await {
            Ok(channel) => channel,
            Err(error) => {
                warn!(room = %room_jid, error = %error, "Failed to resolve managed MUC channel");
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    &nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Wait,
                        DefinedCondition::InternalServerError,
                        "en",
                        "Failed to resolve managed channel for room.",
                    ),
                )];
            }
        };
        let existing_room_actor = get_room_actor(state, room_jid).await;
        let existing_room_snapshot = if let Some(actor) = existing_room_actor.as_ref() {
            match actor.ask(GetSnapshot).await {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    if !retried_dead_room
                        && !matches!(&error, kameo::error::SendError::HandlerError(_))
                    {
                        // Room actor destroyed between lookup and
                        // snapshot (#1108) — retry via the registry.
                        retried_dead_room = true;
                        continue;
                    }
                    warn!(room = %room_jid, error = ?error, "Failed to snapshot MUC room before join");
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::InternalServerError,
                            "en",
                            "Failed to snapshot room before join.",
                        ),
                    )];
                }
            }
        } else {
            None
        };
        let admission_revision = existing_room_snapshot
            .as_ref()
            .map(|snapshot| snapshot.admission_revision)
            .unwrap_or(0);
        let managed_affiliation = if let Some(channel) = managed_channel.as_ref() {
            let Some(session) = authenticated_session else {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    &nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Auth,
                        DefinedCondition::NotAuthorized,
                        "en",
                        "Authentication required to join managed channel.",
                    ),
                )];
            };
            let admission_members_only = existing_room_snapshot
                .as_ref()
                .map(|snapshot| snapshot.room.config.members_only)
                .unwrap_or(channel.members_only);
            let Ok(session_bare) = session.user_jid.parse::<BareJid>() else {
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    &nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Wait,
                        DefinedCondition::InternalServerError,
                        "en",
                        "Failed to resolve managed-channel affiliation.",
                    ),
                )];
            };
            match resolve_managed_channel_affiliation(
                state,
                &session_bare,
                room_jid,
                &channel.id,
                admission_members_only,
                // Join admission repairs a stale Space→channel projection.
                true,
            )
            .await
            {
                Ok(Some(Affiliation::Outcast)) => {
                    // The resolver's Outcast comes from the permission
                    // graph (resolver-derived), so mirror it into a live
                    // actor the same way: a formerly-Member-now-Outcast
                    // user's stale resolver-derived Member entry would
                    // otherwise linger on the room's affiliation list
                    // until eviction. Explicit bans are untouched by the
                    // provenance-aware sync.
                    sync_resolver_affiliation_on_rejection(
                        existing_room_actor.as_ref(),
                        room_jid,
                        // Room affiliations are keyed by the joiner's
                        // bare JID (`JoinWithAffiliation` uses
                        // `sender_jid.to_bare()`), so the sync must use
                        // the same key.
                        sender_jid.to_bare(),
                        Affiliation::Outcast,
                        admission_revision,
                    )
                    .await;
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Auth,
                            DefinedCondition::Forbidden,
                            "en",
                            "Banned from managed channel.",
                        ),
                    )];
                }
                Ok(Some(affiliation)) => Some(affiliation),
                Ok(None) => {
                    if admission_members_only {
                        // The registration-required rejection returns
                        // BEFORE `JoinWithAffiliation`, so its
                        // `Resolver(None)` write never reaches a live
                        // actor — clear any stale resolver-derived
                        // affiliation from before the revocation here.
                        sync_resolver_affiliation_on_rejection(
                            existing_room_actor.as_ref(),
                            room_jid,
                            // Same key as `JoinWithAffiliation`:
                            // `sender_jid.to_bare()`.
                            sender_jid.to_bare(),
                            Affiliation::None,
                            admission_revision,
                        )
                        .await;
                        return vec![build_muc_presence_error_xml(
                            room_jid,
                            &nick,
                            sender_jid,
                            StanzaError::new(
                                ErrorType::Auth,
                                DefinedCondition::RegistrationRequired,
                                "en",
                                "Membership required to join managed channel.",
                            ),
                        )];
                    }
                    Some(Affiliation::None)
                }
                Err(()) => {
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::InternalServerError,
                            "en",
                            "Failed to resolve managed-channel affiliation.",
                        ),
                    )];
                }
            }
        } else {
            None
        };

        let (room_actor, created_instant_room) = match existing_room_actor {
            Some(actor) => (actor, false),
            None => {
                if managed_channel.is_none()
                    && !server_permission_allowed(
                        state,
                        authenticated_session.as_ref(),
                        Permission::CreateMuc,
                    )
                    .await
                    .unwrap_or(false)
                {
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Cancel,
                            DefinedCondition::NotAllowed,
                            "en",
                            "Creating new MUC rooms is not permitted for this account.",
                        ),
                    )];
                }

                let config = managed_channel
                    .as_ref()
                    .map(|channel| RoomConfig {
                        name: channel.name.clone(),
                        description: channel.description.clone(),
                        members_only: channel.members_only,
                        public_room: channel.public_room,
                        moderated: channel.channel_type == "announcement",
                        forum: channel.channel_type == "forum",
                        group_dm: channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM,
                        // #422: load persisted pin policy so the actor's
                        // snapshot matches the channel's last-saved value
                        // even after eviction.
                        pin_permission: channel.pin_permission,
                        ..Default::default()
                    })
                    .unwrap_or_else(|| RoomConfig {
                        name: room_jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "Room".to_string()),
                        members_only: false,
                        ..Default::default()
                    });

                let (waddle_id, channel_id) = managed_channel
                    .as_ref()
                    .map(|channel| {
                        let (waddle_id, _) = parse_room_jid_context(room_jid);
                        (waddle_id, channel.id.clone())
                    })
                    .unwrap_or_else(|| parse_room_jid_context(room_jid));

                let acquisition = match get_or_create_room_actor(
                    state,
                    room_jid,
                    config,
                    waddle_id,
                    channel_id,
                )
                .await
                {
                    Ok(acquisition) => acquisition,
                    // ADR-0017 Phase 3 Slice 7 FIX 6 (council-adjudicated):
                    // another node genuinely, currently owns this room's
                    // claim. Phase 4 first tries the ordered relay MUC proxy
                    // so the owning RoomActor remains the single writer.
                    Err(waddle_xmpp::muc::room_registry_actor::RoomRegistryError::ClaimHeldByAnotherNode(_)) => {
                        #[cfg(feature = "clustering")]
                        if let Some(origin) = ordered_relay_origin.as_ref() {
                            if let Some(bridge) = state
                                .deps
                                .app_state
                                .clustering_claims
                                .ordered_relay_delivery_bridge
                                .as_ref()
                            {
                                let mut presence = xmpp_parsers::presence::Presence::new(
                                    xmpp_parsers::presence::Type::None,
                                );
                                presence.from = Some(jid::Jid::from(sender_jid.clone()));
                                presence.to = room_jid
                                    .clone()
                                    .with_resource_str(&nick)
                                    .ok()
                                    .map(jid::Jid::from);
                                if let Some(show) = presence_show {
                                    presence.show = Some(show.to_xep0045());
                                }
                                let stanza = Stanza::Presence(presence);
                                let _remote_muc_membership_guard = state
                                    .deps
                                    .protocol
                                    .remote_muc_memberships
                                    .lock_membership(sender_jid, room_jid)
                                    .await;
                                match remote_muc_join_decision(
                                    bridge
                                        .try_proxy_muc_remote(
                                            room_jid,
                                            &stanza,
                                            crate::clustering::ordered_relay::OrderedRelayMucProxyKind::JoinPresence,
                                            origin,
                                        )
                                        .await,
                                ) {
                                    Some(RemoteMucJoinDecision::Delivered(replies)) => {
                                        state
                                            .deps
                                            .protocol
                                            .remote_muc_memberships
                                            .record_join(sender_jid, room_jid, &nick);
                                        return replies
                                            .into_iter()
                                            .map(|reply| stanza_to_xml(&reply))
                                            .collect();
                                    }
                                    Some(RemoteMucJoinDecision::MaybeCommitted) => {
                                        // The remote owner may already have mutated room state; a
                                        // local presence error would lie. Keep cleanup state and let
                                        // the client retry/resynchronize instead.
                                        state
                                            .deps
                                            .protocol
                                            .remote_muc_memberships
                                            .record_join(sender_jid, room_jid, &nick);
                                        return Vec::new();
                                    }
                                    None => {}
                                }
                            }
                        }
                        return vec![build_muc_presence_error_xml(
                            room_jid,
                            &nick,
                            sender_jid,
                            StanzaError::new(
                                ErrorType::Wait,
                                DefinedCondition::ResourceConstraint,
                                "en",
                                "This room's ownership is currently held by another node; \
                                 please retry.",
                            ),
                        )];
                    }
                    Err(error) => {
                        warn!(
                            room = %room_jid,
                            %error,
                            "Failed to get or create room actor for MUC join"
                        );
                        return vec![build_muc_presence_error_xml(
                            room_jid,
                            &nick,
                            sender_jid,
                            StanzaError::new(
                                ErrorType::Wait,
                                DefinedCondition::InternalServerError,
                                "en",
                                "Failed to get or create the room.",
                            ),
                        )];
                    }
                };
                // #1134: the created-bit is registry-authoritative —
                // the registry's serialized handler makes exactly one
                // racing first-join the creator. Inferring it from "no
                // actor existed when we looked" gave Owner to every
                // racer.
                let created = acquisition.creation
                    == waddle_xmpp::muc::room_registry_actor::RoomCreation::Created;
                (acquisition.actor_ref, managed_channel.is_none() && created)
            }
        };

        let affiliation_grant = if created_instant_room {
            // XEP-0045 §10.1.1: only the actual room creator gets Owner.
            JoinAffiliationGrant::CreatorOwner
        } else if let Some(affiliation) = managed_affiliation {
            JoinAffiliationGrant::Resolver(affiliation)
        } else {
            JoinAffiliationGrant::Unaffiliated
        };

        let join_outcome = match room_actor
            .ask(JoinWithAffiliation {
                sender_jid: sender_jid.clone(),
                nick: nick.clone(),
                affiliation_grant,
                local_domain: domain.clone(),
                admission_revision,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // #1108: sealed-for-destruction room actor, or an ask
                // against an already-stopped actor. Retry once through
                // the registry, which respawns the room; never drop the
                // join silently.
                let room_sealed = matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RoomSealed
                    )
                );
                let room_gone =
                    room_sealed || !matches!(&error, kameo::error::SendError::HandlerError(_));
                if room_gone {
                    if !retried_dead_room {
                        retried_dead_room = true;
                        if room_sealed {
                            // #1108 follow-up: a sealed actor can still
                            // be registered when the guarded destroy's
                            // seal ask timed out — the registry lookup
                            // would hand back the same sealed actor and
                            // the retry would fail identically. Purge it
                            // so get-or-create respawns a fresh room.
                            let _ = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                                .reap_sealed_room(room_jid.clone())
                                .await;
                        }
                        continue;
                    }
                    warn!(room = %room_jid, nick = %nick, error = ?error, "MUC join failed twice against a destroyed room actor");
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::InternalServerError,
                            "en",
                            "Room was evicted while joining; please retry.",
                        ),
                    )];
                }
                let nick_collision = matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                    )
                );
                if nick_collision {
                    warn!(
                        room = %room_jid,
                        nick = %nick,
                        sender = %sender_jid,
                        "MUC nick collision; returning conflict"
                    );
                    return vec![build_muc_conflict_presence_xml(room_jid, &nick, sender_jid)];
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::OccupantAlreadyJoinedUnderDifferentNick {
                        current_nick,
                        ..
                    },
                ) = &error
                {
                    // #1107 / XEP-0045 §7.6: nicknames are locked to
                    // identity; a session already in the room under
                    // another nick is refused with <not-acceptable/>
                    // instead of being admitted as a ghost occupancy.
                    warn!(
                        room = %room_jid,
                        nick = %nick,
                        current_nick = %current_nick,
                        sender = %sender_jid,
                        "MUC join under second nick refused (nicknames locked)"
                    );
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Cancel,
                            DefinedCondition::NotAcceptable,
                            "en",
                            "You are already in this room under a different nickname.",
                        ),
                    )];
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::StaleAdmissionRevision,
                ) = &error
                {
                    if stale_admission_retries < MAX_STALE_ADMISSION_RETRIES {
                        stale_admission_retries += 1;
                        continue;
                    }
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::InternalServerError,
                            "en",
                            "Room admission changed while joining; please retry.",
                        ),
                    )];
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::JoinForbidden { reason },
                ) = &error
                {
                    // XEP-0045 §7.2.8: bans map to <forbidden/> even in
                    // members-only rooms (#1265 item 1).
                    let (error_type, condition, message) = match reason {
                        waddle_xmpp::muc::room_actor::JoinDenialReason::MembersOnly => (
                            ErrorType::Auth,
                            DefinedCondition::RegistrationRequired,
                            "Membership required to join this room.",
                        ),
                        waddle_xmpp::muc::room_actor::JoinDenialReason::Banned => (
                            ErrorType::Auth,
                            DefinedCondition::Forbidden,
                            "You are banned from this room.",
                        ),
                    };
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(error_type, condition, "en", message),
                    )];
                }
                // ADR-0017 Phase 3 Slice 7 FIX 4/FIX 6 (council-adjudicated):
                // this incarnation's durable restore has not (yet) resolved
                // — a genuine backend failure, not a legitimate empty new
                // room. Bounce with the same conformant, recoverable
                // condition the ownership-claim bounce above uses, so the
                // client retries rather than silently never joining.
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RestorePending
                    )
                ) {
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::ResourceConstraint,
                            "en",
                            "This room's durable state has not finished loading; please retry.",
                        ),
                    )];
                }
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::OwnershipUnavailable
                    )
                ) {
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::ResourceConstraint,
                            "en",
                            "This room's ownership is being reconciled; please retry.",
                        ),
                    )];
                }
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RoomFull
                    )
                ) {
                    // XEP-0045 §7.2.9: the room has reached its maximum
                    // number of occupants — deny access with a presence
                    // error of type "wait" carrying <service-unavailable/>.
                    // Returning an empty reply here left the client
                    // stalled forever waiting for self-presence (#1111).
                    warn!(
                        room = %room_jid,
                        nick = %nick,
                        sender = %sender_jid,
                        "MUC join refused: room is full"
                    );
                    return vec![build_muc_presence_error_xml(
                        room_jid,
                        &nick,
                        sender_jid,
                        StanzaError::new(
                            ErrorType::Wait,
                            DefinedCondition::ServiceUnavailable,
                            "en",
                            "The room has reached its maximum number of occupants.",
                        ),
                    )];
                }
                // FIX 6 / #1111: no remaining error variant may silently
                // drop the join with no presence reply at all — bounce
                // typed instead of the previous bare `return vec![]`. This
                // is unreachable for the current JoinWithAffiliation error
                // surface (every RoomActorError variant it returns has a
                // typed arm above, and transport failures take the #1108
                // retry path) — kept as a typed fail-safe so a future
                // variant can never stall the client with an empty reply.
                warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
                return vec![build_muc_presence_error_xml(
                    room_jid,
                    &nick,
                    sender_jid,
                    StanzaError::new(
                        ErrorType::Wait,
                        DefinedCondition::InternalServerError,
                        "en",
                        "Failed to join the room; please retry.",
                    ),
                )];
            }
        };

        let occupant_count = join_outcome.occupant_count;
        let self_muji = join_outcome
            .existing_occupants
            .iter()
            .find(|existing| existing.nick == nick && existing.jid == *sender_jid)
            .and_then(|existing| existing.muji.as_ref());
        let self_in_call = join_outcome
            .existing_occupants
            .iter()
            .find(|existing| existing.nick == nick && existing.jid == *sender_jid)
            .map(|existing| existing.in_call)
            .unwrap_or_default();

        info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

        // Notification activity ingest (slice 2b): a successful MUC join
        // bumps `(sender_bare, room)` activity. The XEP-0513 `<active/>`
        // filter consults this projection to admit ActiveChannelMention
        // pushes for users who are present in the room. `presence_show` is
        // passed in by the caller (`handle_presence`) when the incoming
        // presence carried a typed `<show/>` token; on first join (or
        // when no `<show/>` is present) we record `None` so the column
        // stays NULL until the user actually broadcasts a state.
        crate::server::routes::interpret::record_presence_available_activity_on_state(
            state,
            &sender_jid.to_bare(),
            room_jid,
            presence_show,
        )
        .await;

        let mut responses = Vec::new();

        // Replay one base occupant presence per nick to the joiner, then
        // append extra same-nick Muji payloads for additional sessions
        // that own call state. Active call membership is nick-level, but
        // XEP-0272 preparing is resource-owned coordination state, so the
        // joiner needs the exact full JID that advertised it.
        let mut replayed_nicks = std::collections::HashSet::new();
        let replay_occupants: Vec<_> = join_outcome
            .existing_occupants
            .iter()
            .filter(|existing| existing.nick != nick)
            .collect();
        for existing in replay_occupants
            .iter()
            .copied()
            .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
        {
            // XEP-0045 §7.2 conformant occupant-list replay, plus the
            // typed `<call xmlns='urn:waddle:muc-call:0'/>` extension when
            // the room actor's snapshot still has an active advertisement
            // for that occupant. Without this enrichment the joiner only
            // sees the chip light up via the NEXT presence update from a
            // call participant, which never happens if the call is steady
            // state — the late joiner is the one we're trying to help.
            responses.push(build_muc_join_presence_xml(MucJoinPresence {
                occupant_id_secret: &state.deps.occupant_id_secret,
                room_jid,
                nick: &existing.nick,
                to_jid: sender_jid,
                affiliation: existing.affiliation,
                role: existing.role,
                real_jid: &existing.jid,
                disclose_real_jid: true,
                include_self_status: false,
                room_created: false,
                warn_nonanonymous_join: false,
                muji: existing.muji.as_ref(),
                in_call: existing.in_call,
            }));

            for extra in replay_occupants.iter().copied().filter(|candidate| {
                candidate.nick == existing.nick
                    && candidate.jid != existing.jid
                    && (candidate.muji.is_some() || !candidate.in_call.is_empty())
            }) {
                responses.push(build_muc_join_presence_xml(MucJoinPresence {
                    occupant_id_secret: &state.deps.occupant_id_secret,
                    room_jid,
                    nick: &extra.nick,
                    to_jid: sender_jid,
                    affiliation: extra.affiliation,
                    role: extra.role,
                    real_jid: &extra.jid,
                    disclose_real_jid: true,
                    include_self_status: false,
                    room_created: false,
                    warn_nonanonymous_join: false,
                    muji: extra.muji.as_ref(),
                    in_call: extra.in_call,
                }));
            }
        }

        // Broadcast the new occupant's presence to all existing occupants.
        // Non-blocking: a zombied/slow consumer must never stall the join path,
        // which is how "Timed out waiting for self-presence" cascades start.
        // Drop accounting is handled inside `try_send_to` (logs + metrics);
        // per-occupant outcome is discarded here because a missed join
        // presence self-heals via the next MUC presence/probe round-trip.
        if !join_outcome.is_same_bare_multi_session_join && !join_outcome.is_existing_session_rejoin
        {
            for existing in &join_outcome.existing_occupants {
                let presence_stanza = build_muc_join_presence_stanza(MucJoinPresence {
                    occupant_id_secret: &state.deps.occupant_id_secret,
                    room_jid,
                    nick: &nick,
                    to_jid: &existing.jid,
                    affiliation: join_outcome.new_occupant_affiliation,
                    role: join_outcome.new_occupant_role,
                    real_jid: sender_jid,
                    disclose_real_jid: true,
                    include_self_status: false,
                    room_created: false,
                    warn_nonanonymous_join: false,
                    muji: None,
                    in_call: waddle_xmpp::xep::InCallPresenceState::default(),
                });
                let stanza = Stanza::Presence(presence_stanza);
                route_room_presence_to_occupant(state, room_jid, &existing.jid, stanza).await;
            }
        }

        // Send self-presence to the joining user (with status code 110)
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            room_jid,
            nick: &nick,
            to_jid: sender_jid,
            affiliation: join_outcome.new_occupant_affiliation,
            role: join_outcome.new_occupant_role,
            real_jid: sender_jid,
            disclose_real_jid: true,
            include_self_status: true,
            room_created: created_instant_room,
            // XEP-0045 §7.2.3: status 100 rides ONLY on the joiner's
            // initial self-presence (#1265 item 4).
            warn_nonanonymous_join: true,
            muji: self_muji,
            in_call: self_in_call,
        }));

        // Same-account sibling resources share one MUC nick. If a
        // sibling already advertised Muji for this nick, reflect exact
        // per-session snapshots after the new session's own plain
        // self-presence so a refresh/new tab can show "call active on
        // another device" without misattributing `<preparing/>` to the
        // joining resource.
        for existing in join_outcome.existing_occupants.iter().filter(|existing| {
            existing.nick == nick
                && existing.jid.to_bare() == sender_jid.to_bare()
                && existing.jid != *sender_jid
                && (existing.muji.is_some() || !existing.in_call.is_empty())
        }) {
            responses.push(build_muc_join_presence_xml(MucJoinPresence {
                occupant_id_secret: &state.deps.occupant_id_secret,
                room_jid,
                nick: &nick,
                to_jid: sender_jid,
                affiliation: existing.affiliation,
                role: existing.role,
                real_jid: &existing.jid,
                disclose_real_jid: true,
                include_self_status: true,
                room_created: false,
                warn_nonanonymous_join: false,
                muji: existing.muji.as_ref(),
                in_call: existing.in_call,
            }));
        }

        // XEP-0045 §7.2.15 historical room subject. The typed builder
        // produces the conformant envelope: nick-form `from` + `<delay/>`
        // + XEP-0421 `<occupant-id/>` when a setter is known, or bare-from
        // empty `<subject/>` for a never-set room (matching the established
        // resolution of XEP-0421 §3 vs §7.2.15 on never-set rooms).
        let subject_msg = build_subject_message(
            room_jid,
            sender_jid,
            join_outcome.subject_state.as_ref(),
            &state.deps.occupant_id_secret,
        );
        responses.push(stanza_to_xml(&Stanza::Message(subject_msg)));

        return responses;
    }
}

/// Handle MUC room leave
pub async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");
    #[cfg(not(feature = "clustering"))]
    let _ = ordered_relay_origin;

    // Notification activity ingest (slice 2b): a XEP-0045 leave
    // (explicit `<presence type='unavailable'/>`) is still an
    // engagement signal — the user just acted on the room — so we
    // bump `(sender_bare, room)` activity and clear the persisted
    // `<show/>`. We record before the room-actor teardown so a
    // missing room actor doesn't suppress the activity write; the
    // typed signal happened on the wire regardless.
    crate::server::routes::interpret::record_presence_unavailable_activity_on_state(
        state,
        &sender_jid.to_bare(),
        room_jid,
    )
    .await;

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        let known_remote_membership = state
            .deps
            .protocol
            .remote_muc_memberships
            .contains(sender_jid, room_jid);
        #[cfg(feature = "clustering")]
        if let Some(origin) = ordered_relay_origin {
            if let Some(bridge) = state
                .deps
                .app_state
                .clustering_claims
                .ordered_relay_delivery_bridge
                .as_ref()
            {
                let mut presence = xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::Unavailable,
                );
                presence.from = Some(jid::Jid::from(sender_jid.clone()));
                presence.to = room_jid
                    .clone()
                    .with_resource_str(nick)
                    .ok()
                    .map(jid::Jid::from);
                let stanza = Stanza::Presence(presence);
                let _remote_muc_membership_guard = state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .lock_membership(sender_jid, room_jid)
                    .await;
                match remote_muc_leave_decision(
                    bridge
                        .try_proxy_muc_remote(
                            room_jid,
                            &stanza,
                            crate::clustering::ordered_relay::OrderedRelayMucProxyKind::OccupantPresence,
                            origin,
                        )
                        .await,
                ) {
                    RemoteMucLeaveDecision::Delivered(replies) => {
                        state
                            .deps
                            .protocol
                            .remote_muc_memberships
                            .record_leave(sender_jid, room_jid);
                        return replies
                            .into_iter()
                            .map(|reply| stanza_to_xml(&reply))
                            .collect();
                    }
                    RemoteMucLeaveDecision::MaybeCommitted => {
                        return Vec::new();
                    }
                    RemoteMucLeaveDecision::RetryableNoEffect => {
                        return vec![build_muc_presence_error_xml(
                            room_jid,
                            nick,
                            sender_jid,
                            StanzaError::new(
                                ErrorType::Wait,
                                DefinedCondition::ResourceConstraint,
                                "en",
                                "This room's ownership is currently unreachable; please retry.",
                            ),
                        )];
                    }
                    RemoteMucLeaveDecision::LocalFallback => {}
                }
            }
        }
        if known_remote_membership {
            return vec![build_muc_presence_error_xml(
                room_jid,
                nick,
                sender_jid,
                StanzaError::new(
                    ErrorType::Wait,
                    DefinedCondition::ResourceConstraint,
                    "en",
                    "This room's ownership is currently unreachable; please retry.",
                ),
            )];
        }

        debug!(room = %room_jid, "Room not found for leave");
        // Idempotent on the SFU side — the user could have an SFU
        // participant even when the room actor is gone (process
        // restart, eviction). Tear that down too.
        super::super::super::muc_call_sfu::unregister_participant_from_room(
            state, room_jid, sender_jid,
        );
        return vec![build_muc_self_unavailable_xml(
            state,
            room_jid,
            nick,
            sender_jid,
            Affiliation::None,
        )];
    };

    let outcome = match room_actor
        .ask(LeaveByRealJid {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            // No occupant slot to remove, but a stale SFU
            // participant could still exist — clear it.
            super::super::super::muc_call_sfu::unregister_participant_from_room(
                state, room_jid, sender_jid,
            );
            return vec![build_muc_self_unavailable_xml(
                state,
                room_jid,
                nick,
                sender_jid,
                Affiliation::None,
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(
                state,
                room_jid,
                nick,
                sender_jid,
                Affiliation::None,
            )];
        }
    };

    // SFU teardown runs after `LeaveByRealJid` so the MUC's
    // authoritative view drops the occupant first; the membership
    // gate immediately reports the user as a non-occupant and any
    // subsequent `request-join` is rejected before the SFU is
    // touched again. Closes the gap where a client leaves the MUC
    // without sending the call-specific `request-leave` — their SFU
    // participant would otherwise linger until LiveKit's timeout.
    super::super::super::muc_call_sfu::unregister_participant_from_room(
        state, room_jid, sender_jid,
    );

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`. The same helper
    // is used by `cleanup_muc_presence` for unclean disconnects, so
    // both the explicit-leave path and the tab-close path produce the
    // same wire shape.
    super::super::super::cleanup::broadcast_muc_leave_to_remaining(
        state, room_jid, sender_jid, &outcome,
    )
    .await;
    super::super::super::cleanup::broadcast_muc_muji_clear_to_remaining(
        state, room_jid, sender_jid, &outcome,
    )
    .await;

    let response = vec![build_muc_self_unavailable_xml(
        state,
        room_jid,
        &outcome.nick,
        sender_jid,
        outcome.affiliation,
    )];
    super::super::super::cleanup::maybe_evict_empty_room(state, room_jid, &outcome).await;
    response
}
