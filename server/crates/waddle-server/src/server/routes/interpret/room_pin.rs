//! Interpreter arms for pin events (#414).
//!
//! Resolves an `OutboundEvent::ApplyPinChange` request by:
//!
//! 1. For pins: looking up the target message in the room's MAM
//!    archive to populate the preview (author bare-JID, body text
//!    truncated to 280 chars, original message timestamp). The chain
//!    handler is synchronous and cannot do this lookup — the
//!    interpreter is the async boundary that builds the resolved
//!    [`PinnedEntry`].
//! 2. Forwarding the resolved [`PinStateChange`] to the room actor's
//!    `ApplyPin` message.
//! 3. Building the `<pin-event/>` system message with the resolved
//!    preview and broadcasting it via
//!    [`super::room_system_message::broadcast_room_system_message_event`].
//!
//! The XEP-0424 retraction cascade emits `ApplyPinChange { Unpin {
//! reason: "retracted" } }` through this same path so the unpin
//! attribution and broadcast logic stays consistent.

use super::*;
use jid::BareJid;
use waddle_xmpp::muc::pin::{
    PinChangeRequest, PinPreview, PinStateChange, PinnedEntry, MAX_PREVIEW_LEN,
};
use waddle_xmpp::muc::room_actor::{GetPinList, GetRoomSnapshot, RollbackPinIfRevision};
use waddle_xmpp::protocol::room::pin::{
    build_pinned_system_message, build_unpinned_system_message,
};
use waddle_xmpp::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN;
use waddle_xmpp_core::xep0359::StanzaId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PinChangeOutcome {
    Applied,
    Skipped,
    NotOwner,
    OwnershipUncertain,
    PersistFailed,
}

pub(super) async fn apply_pin_change_event(
    deps: &Deps<'_>,
    room: BareJid,
    request: PinChangeRequest,
    recursion_depth: u8,
) -> PinChangeOutcome {
    if request.target_stanza_id().id.is_empty()
        || request.target_stanza_id().id.len() > MAX_TARGET_STANZA_ID_LEN
    {
        warn!(
            room = %room,
            target = %request.target_stanza_id().id,
            "ApplyPinChange: target stanza-id failed length validation; dropping"
        );
        return PinChangeOutcome::Skipped;
    }
    match request {
        PinChangeRequest::Pin { .. } => apply_pin(deps, room, request, recursion_depth).await,
        PinChangeRequest::Unpin { .. } => apply_unpin(deps, room, request, recursion_depth).await,
    }
}

async fn apply_pin(
    deps: &Deps<'_>,
    room: BareJid,
    request: PinChangeRequest,
    recursion_depth: u8,
) -> PinChangeOutcome {
    let PinChangeRequest::Pin {
        target_stanza_id,
        pinner_jid,
        pinner_nick,
        pinned_at,
    } = request
    else {
        return PinChangeOutcome::Skipped;
    };
    let room_actor = match exact_room_actor_for_effect(deps, &room).await {
        Ok(actor) => actor,
        Err(error) => return map_room_effect_authority_error(error),
    };

    // Pull the room snapshot once: we need its occupant list to map
    // the archived `room/nick` from-JID back to the author's real
    // bare JID for the preview.
    let synthetic_sender = match room.clone().with_resource_str("__pin_resolver__") {
        Ok(s) => s,
        Err(error) => {
            warn!(
                room = %room,
                ?error,
                "ApplyPinChange::Pin: failed to build resolver sender; dropping"
            );
            return PinChangeOutcome::PersistFailed;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: synthetic_sender,
        })
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "ApplyPinChange::Pin: GetRoomSnapshot failed; dropping"
            );
            return PinChangeOutcome::OwnershipUncertain;
        }
    };

    // Resolve the preview from MAM. If the target row is missing
    // (e.g., archive purged or wire id mismatched), fall back to a
    // placeholder preview keyed on the pinner — better than dropping
    // the pin silently. Most real pin requests target a recent
    // message that's still archived.
    let preview = resolve_preview_from_mam(deps, &room, &target_stanza_id, &snapshot.occupants)
        .await
        .unwrap_or_else(|| {
            warn!(
                room = %room,
                target = %target_stanza_id.id,
                "ApplyPinChange::Pin: target not found in MAM; storing placeholder preview"
            );
            PinPreview::new(pinner_jid.clone(), None, "", pinned_at)
        });

    let entry = PinnedEntry {
        target_stanza_id: target_stanza_id.clone(),
        pinner_jid: pinner_jid.clone(),
        pinned_at,
        preview: preview.clone(),
    };
    let previous = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries
            .into_iter()
            .find(|entry| entry.target_stanza_id.id == target_stanza_id.id),
        Err(error) => {
            warn!(room = %room, ?error, "ApplyPinChange::Pin: failed to snapshot pin state");
            return PinChangeOutcome::OwnershipUncertain;
        }
    };

    let (room_actor, applied_revision) =
        match apply_pin_state_change_exact(deps, &room, PinStateChange::Pin(entry)).await {
            Ok(applied) => applied,
            Err(outcome) => return outcome,
        };

    let system_message = build_pinned_system_message(
        &room,
        &pinner_jid,
        &pinner_nick,
        &target_stanza_id,
        Some(&preview),
        None,
    );
    let broadcast = super::room_system_message::broadcast_room_system_message_event(
        deps,
        room.clone(),
        Box::new(system_message),
        recursion_depth,
    )
    .await;
    if broadcast.is_none() {
        rollback_pin_change(
            &room_actor,
            &room,
            &target_stanza_id,
            previous,
            applied_revision,
        )
        .await;
        return PinChangeOutcome::OwnershipUncertain;
    }
    PinChangeOutcome::Applied
}

async fn apply_unpin(
    deps: &Deps<'_>,
    room: BareJid,
    request: PinChangeRequest,
    recursion_depth: u8,
) -> PinChangeOutcome {
    let PinChangeRequest::Unpin {
        target_stanza_id,
        pinner_jid,
        pinner_nick,
        reason,
    } = request
    else {
        return PinChangeOutcome::Skipped;
    };
    let room_actor = match exact_room_actor_for_effect(deps, &room).await {
        Ok(actor) => actor,
        Err(error) => return map_room_effect_authority_error(error),
    };

    let previous = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries
            .into_iter()
            .find(|entry| entry.target_stanza_id.id == target_stanza_id.id),
        Err(error) => {
            warn!(room = %room, ?error, "ApplyPinChange::Unpin: failed to snapshot pin state");
            return PinChangeOutcome::OwnershipUncertain;
        }
    };

    let (room_actor, applied_revision) = match apply_pin_state_change_exact(
        deps,
        &room,
        PinStateChange::Unpin {
            target_stanza_id: target_stanza_id.clone(),
        },
    )
    .await
    {
        Ok(applied) => applied,
        Err(outcome) => return outcome,
    };

    let system_message = build_unpinned_system_message(
        &room,
        &pinner_jid,
        &pinner_nick,
        &target_stanza_id,
        reason.as_deref(),
    );
    let broadcast = super::room_system_message::broadcast_room_system_message_event(
        deps,
        room.clone(),
        Box::new(system_message),
        recursion_depth,
    )
    .await;
    if broadcast.is_none() {
        rollback_pin_change(
            &room_actor,
            &room,
            &target_stanza_id,
            previous,
            applied_revision,
        )
        .await;
        return PinChangeOutcome::OwnershipUncertain;
    }
    PinChangeOutcome::Applied
}

fn map_apply_pin_error(
    error: kameo::error::SendError<ApplyPin, waddle_xmpp::muc::room_actor::RoomMutationError>,
) -> PinChangeOutcome {
    match error {
        kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::NotOwner,
        ) => PinChangeOutcome::NotOwner,
        kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::OwnershipUncertain,
        ) => PinChangeOutcome::OwnershipUncertain,
        kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::PersistFailed(_),
        ) => PinChangeOutcome::PersistFailed,
        _ => PinChangeOutcome::OwnershipUncertain,
    }
}

/// Final, write-adjacent gate for a resolved pin change. Preview lookup and
/// prior-state reads may await; this helper deliberately resolves the retained
/// actor incarnation again immediately before the mutation ask.
pub(super) async fn apply_pin_state_change_exact(
    deps: &Deps<'_>,
    room: &BareJid,
    change: PinStateChange,
) -> Result<(kameo::actor::ActorRef<RoomActor>, u64), PinChangeOutcome> {
    let room_actor = exact_room_actor_for_effect(deps, room)
        .await
        .map_err(map_room_effect_authority_error)?;
    let revision = room_actor.ask(ApplyPin { change }).await.map_err(|error| {
        warn!(
            room = %room,
            error = ?error,
            "ApplyPinChange: ApplyPin ask failed; state not broadcast"
        );
        map_apply_pin_error(error)
    })?;
    Ok((room_actor, revision))
}

fn map_room_effect_authority_error(error: RoomEffectAuthorityError) -> PinChangeOutcome {
    match error {
        RoomEffectAuthorityError::NotOwner => PinChangeOutcome::NotOwner,
        RoomEffectAuthorityError::OwnershipUncertain => PinChangeOutcome::OwnershipUncertain,
    }
}

async fn rollback_pin_change(
    room_actor: &kameo::actor::ActorRef<RoomActor>,
    room: &BareJid,
    target: &StanzaId,
    previous: Option<PinnedEntry>,
    expected_revision: u64,
) {
    let change = previous.map_or_else(
        || PinStateChange::Unpin {
            target_stanza_id: target.clone(),
        },
        PinStateChange::Pin,
    );
    match room_actor
        .ask(RollbackPinIfRevision {
            expected_revision,
            change,
        })
        .await
    {
        Ok(true) => {}
        Ok(false) => debug!(
            room = %room,
            target = %target.id,
            expected_revision,
            "ApplyPinChange: skipped rollback because a later pin mutation won"
        ),
        Err(error) => {
            warn!(
                room = %room,
                target = %target.id,
                ?error,
                "ApplyPinChange: system broadcast failed and guarded rollback could not apply"
            );
        }
    }
}

/// Look up the target message in the room's MAM archive and build a
/// `PinPreview` from it. Returns `None` if the message isn't found or
/// MAM storage isn't wired in `Deps`. The body is truncated to
/// `MAX_PREVIEW_LEN` UTF-8 chars by `PinPreview::new`.
///
/// Author resolution: groupchat archive rows store `from` as
/// `room@conf/nick` (XEP-0045 §7.2.13 canonicalized form), so the
/// resource part is the nickname and `to_bare()` collapses to the
/// room JID. We extract the nick from the resource and consult the
/// supplied occupant snapshot to recover the *current* real bare JID
/// for that nick; if no current occupant matches (the author has
/// left the room since posting), the preview falls back to the room
/// JID as `author_jid` with the captured `author_nick` carrying the
/// identity. This trades a small amount of fidelity for a stable
/// non-async lookup — pinning is interactive, so racing with leaves
/// is rare.
async fn resolve_preview_from_mam(
    deps: &Deps<'_>,
    room: &BareJid,
    target_stanza_id: &StanzaId,
    occupants: &[waddle_xmpp::muc::room_actor::RoomChainOccupant],
) -> Option<PinPreview> {
    let mam_storage = deps.mam_storage?;
    let row = match mam_storage
        .get_message_by_stanza_id(room, &target_stanza_id.id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            warn!(
                room = %room,
                target = %target_stanza_id.id,
                error = ?error,
                "ApplyPinChange::Pin: MAM lookup failed"
            );
            return None;
        }
    };
    let nick = row.from.resource().map(|r| r.to_string());
    let author_bare = nick
        .as_deref()
        .and_then(|nick| {
            occupants
                .iter()
                .find(|o| o.nick == nick)
                .map(|o| o.full_jid.to_bare())
        })
        .unwrap_or_else(|| room.clone());
    let body = row.body.clone().unwrap_or_default();
    let truncated = if body.chars().count() > MAX_PREVIEW_LEN {
        body.chars().take(MAX_PREVIEW_LEN).collect()
    } else {
        body
    };
    Some(PinPreview::new(
        author_bare,
        nick,
        &truncated,
        row.timestamp,
    ))
}

/// XEP-0424 retraction → pin auto-unpin cascade (#414, Q8 = a).
///
/// When a groupchat message is retracted, check whether its stanza-id
/// is in the room's current pin list. If so, emit an
/// `ApplyPinChange { Unpin { reason: "retracted" } }` event so the
/// regular interpreter path mutates the actor and broadcasts the
/// unpin system message — single code path, no special-case logic.
pub(super) async fn cascade_retraction_to_pin_list(
    deps: &Deps<'_>,
    room: BareJid,
    target_message_id: String,
    recursion_depth: u8,
) -> PinChangeOutcome {
    if target_message_id.is_empty() || target_message_id.len() > MAX_TARGET_STANZA_ID_LEN {
        return PinChangeOutcome::Skipped;
    }
    let room_actor = match exact_room_actor_for_effect(deps, &room).await {
        Ok(actor) => actor,
        Err(error) => return map_room_effect_authority_error(error),
    };
    let entries = match room_actor.ask(GetPinList).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "Pin retraction cascade: GetPinList ask failed"
            );
            return PinChangeOutcome::OwnershipUncertain;
        }
    };
    let Some(entry) = entries
        .iter()
        .find(|e| e.target_stanza_id.id == target_message_id)
        .cloned()
    else {
        return PinChangeOutcome::Skipped;
    };

    apply_pin_change_event(
        deps,
        room,
        PinChangeRequest::Unpin {
            target_stanza_id: entry.target_stanza_id,
            pinner_jid: entry.pinner_jid,
            pinner_nick: String::new(),
            reason: Some("retracted".to_string()),
        },
        recursion_depth,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use waddle_xmpp::muc::room_actor::{JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoom};
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    fn pin_entry(room: &BareJid, target: &str) -> PinnedEntry {
        let pinner: BareJid = "alice@example.com".parse().expect("pinner JID");
        let pinned_at = chrono::Utc::now();
        PinnedEntry {
            target_stanza_id: StanzaId::new(target, Jid::from(room.clone())),
            pinner_jid: pinner.clone(),
            pinned_at,
            preview: PinPreview::new(pinner, Some("alice".to_string()), "preview", pinned_at),
        }
    }

    async fn replaced_room(
        name: &str,
    ) -> (
        ActorRef<RoomRegistryActor>,
        BareJid,
        ActorRef<RoomActor>,
        ActorRef<RoomActor>,
    ) {
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b'p'; 32]).expect("test secret"),
        ));
        let room: BareJid = format!("{name}@muc.example.com").parse().expect("room JID");
        let original = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "original".to_string(),
                channel_id: format!("{name}-original"),
                config: RoomConfig::default(),
            })
            .await
            .expect("create original actor");
        room_registry
            .ask(DestroyRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("remove original actor");
        let replacement = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "replacement".to_string(),
                channel_id: format!("{name}-replacement"),
                config: RoomConfig::default(),
            })
            .await
            .expect("create replacement actor");
        (room_registry, room, original, replacement)
    }

    #[tokio::test]
    async fn replacement_after_preview_cannot_receive_resolved_pin_mutation() {
        let connections = ConnectionRegistry::new();
        let (room_registry, room, original, replacement) = replaced_room("pin-final-gate").await;
        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        deps.room_actor_incarnation = Some(original);

        // `apply_pin` calls this seam only after preview enrichment and the
        // prior-state read have completed. Replacing E1 before this call
        // deterministically models that exact delayed continuation.
        let outcome = apply_pin_state_change_exact(
            &deps,
            &room,
            PinStateChange::Pin(pin_entry(&room, "stale-pin")),
        )
        .await;

        assert_eq!(outcome, Err(PinChangeOutcome::NotOwner));
        assert!(replacement
            .ask(GetPinList)
            .await
            .expect("replacement pin list")
            .is_empty());
        assert!(replacement.is_alive(), "exact E1 demotion must preserve E2");
    }

    #[tokio::test]
    async fn replacement_between_pin_apply_and_broadcast_gets_no_stale_system_message() {
        let connections = ConnectionRegistry::new();
        let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b'b'; 32]).expect("test secret"),
        ));
        let room: BareJid = "pin-broadcast@muc.example.com".parse().expect("room JID");
        let original = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "original".to_string(),
                channel_id: "pin-broadcast-original".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create original actor");
        let entry = pin_entry(&room, "applied-on-e1");
        original
            .ask(ApplyPin {
                change: PinStateChange::Pin(entry.clone()),
            })
            .await
            .expect("apply pin to E1");

        room_registry
            .ask(DestroyRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("remove E1 after pin apply");
        let replacement = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "replacement".to_string(),
                channel_id: "pin-broadcast-replacement".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create E2");
        let occupant: FullJid = "bob@example.com/web".parse().expect("occupant JID");
        replacement
            .ask(JoinWithAffiliation {
                sender_jid: occupant.clone(),
                nick: "bob".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await
            .expect("join E2 occupant");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        connections.register_with_carbons(occupant.clone(), tx, false);
        let connection_entry = connections
            .get_entry(&occupant)
            .expect("registered connection entry");
        assert!(
            crate::server::dual_registration::mirror_register(
                &user_registry,
                occupant,
                connection_entry,
            )
            .await
        );

        let mut deps = Deps::registry_with_user_registry(&connections, &user_registry);
        deps.room_registry = Some(&room_registry);
        deps.room_actor_incarnation = Some(original);
        let system_message = build_pinned_system_message(
            &room,
            &entry.pinner_jid,
            "alice",
            &entry.target_stanza_id,
            Some(&entry.preview),
            None,
        );
        let broadcast = super::super::room_system_message::broadcast_room_system_message_event(
            &deps,
            room,
            Box::new(system_message),
            0,
        )
        .await;

        assert!(broadcast.is_none());
        assert!(
            rx.try_recv().is_err(),
            "E2 occupant received stale E1 pin event"
        );
        assert!(replacement
            .ask(GetPinList)
            .await
            .expect("replacement pin list")
            .is_empty());
        assert!(replacement.is_alive(), "exact E1 demotion must preserve E2");
    }
}
