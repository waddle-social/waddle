use super::*;
use crate::ingress_shadow::{
    IngressShadowRoomFence, ShadowAuthorizationDeniedReason, ShadowDecisionMarker,
    ShadowSemanticRejectedReason,
};
use waddle_xmpp::ingress::{EntityGeneration, IngressEffectIntent};

/// Bind the subject mutation emitted from one frozen room snapshot to
/// that actor incarnation's immutable ownership proof. This is mandatory
/// post-processing before interpretation: the pure protocol chain cannot carry
/// actor state in `RoomContext`, so it emits an unbound event. Bypassing this
/// boundary leaves a durable actor without its exact snapshot fence (or with a
/// different one), which it rejects before applying the mutation.
pub(super) fn bind_room_claim_fence(
    events: &mut [OutboundEvent],
    claim_fence: Option<&waddle_xmpp::muc::RoomClaimFenceContext>,
) {
    for event in events {
        if let OutboundEvent::PersistRoomSubject {
            claim_fence: event_fence @ None,
            ..
        } = event
        {
            *event_fence = claim_fence.cloned();
        }
    }
}

/// Move the one subject-persistence effect emitted by the room chain directly
/// after its archive effect while preserving every other event's order.
///
/// The handler chain itself remains subject-before-archive so an unauthorized
/// XEP-0045 subject change still halts before archival. Reordering only the
/// admitted event batch lets `ArchiveGroupchat` arm ownership-loss,
/// tombstone-hit, or deduplication suppression before the room mutation, while
/// `PersistRoomSubject` still completes before the reflector's
/// `RouteToConnection` effects as required by the ordering contract on that
/// event. The chain emits at most one of each effect for a message.
pub(super) fn order_subject_persistence_after_archive(
    mut events: Vec<OutboundEvent>,
) -> Vec<OutboundEvent> {
    let Some(subject_index) = events
        .iter()
        .position(|event| matches!(event, OutboundEvent::PersistRoomSubject { .. }))
    else {
        return events;
    };
    let Some(archive_index) = events
        .iter()
        .position(|event| matches!(event, OutboundEvent::ArchiveGroupchat { .. }))
    else {
        return events;
    };
    let insertion_index = if subject_index < archive_index {
        archive_index
    } else {
        archive_index + 1
    };
    let subject = events.remove(subject_index);
    events.insert(insertion_index, subject);
    events
}

pub(super) async fn dispatch_to_room(
    deps: &Deps<'_>,
    room_jid: jid::BareJid,
    incoming: Message,
    recursion_depth: u8,
) -> InterpretOutcome {
    let mut outcome = InterpretOutcome::default();
    let Some(state) = deps.web_socket_state else {
        warn!(
            variant = "DispatchToRoom",
            room = %room_jid,
            "DispatchToRoom: no web_socket_state in Deps; dropping. \
             Production must populate web_socket_state."
        );
        return outcome;
    };
    let Some(room_registry) = deps.room_registry else {
        warn!(
            variant = "DispatchToRoom",
            room = %room_jid,
            "DispatchToRoom: no room_registry in Deps; dropping"
        );
        return outcome;
    };
    let Some(sender_full) = incoming
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        warn!(
            room = %room_jid,
            "DispatchToRoom: message.from is missing or not a full JID; dropping"
        );
        return outcome;
    };

    #[cfg(feature = "clustering")]
    if let Some(origin) = deps.ordered_relay_origin.as_ref() {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            use crate::clustering::route_bridge::{
                MucProxyRouteDecision, OrderedRelayMucProxyOutcome,
            };
            let stanza = Stanza::Message(incoming.clone());
            match bridge
                .try_proxy_muc_remote_decision(
                    &room_jid,
                    &stanza,
                    crate::clustering::ordered_relay::OrderedRelayMucProxyKind::GroupchatMessage,
                    origin,
                )
                .await
            {
                MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(
                    replies,
                )) => {
                    for reply in replies {
                        match reply.to_element_string() {
                            Ok(xml) => outcome.frames.push(xml),
                            Err(error) => {
                                warn!(
                                    room = %room_jid,
                                    %error,
                                    "DispatchToRoom: failed to serialize remote MUC reply"
                                );
                            }
                        }
                    }
                    return outcome;
                }
                // Attempted-but-failed AND retryable routing states
                // (claim lookup/lease trouble, origin claim held
                // elsewhere) bounce a wait-class retry error. Falling
                // through to the local registry here would misreport a
                // healthy REMOTE room as `<item-not-found/>` (review P2
                // on PR #1277).
                MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Unavailable)
                | MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Dropped)
                | MucProxyRouteDecision::RoomClaimUnavailable
                | MucProxyRouteDecision::OriginUnavailable => {
                    clear_provisional_shadow_room_fence(deps);
                    push_sender_error_reply(
                        deps,
                        &mut outcome,
                        &incoming,
                        &room_jid,
                        &sender_full,
                        resource_constraint_error(
                            "This room is temporarily unreachable; please retry.",
                        ),
                    );
                    return outcome;
                }
                MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::MaybeCommitted)
                | MucProxyRouteDecision::Attempted(
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
                ) => {
                    return outcome;
                }
                // The local registry is authoritative: the room claim is
                // owned here, or no claim row exists anywhere (a truly
                // nonexistent/dormant room — the local path bounces
                // `<item-not-found/>` correctly).
                MucProxyRouteDecision::LocalRoom | MucProxyRouteDecision::RoomUnclaimed => {}
            }
        }
    }

    // 1. Prepare the prototype the room gate sees. Enrichment is delayed
    //    until after occupancy / managed-room validation so unauthorized
    //    senders receive the XEP-0045 room error before any Waddle-specific
    //    extension payload checks or extension runtime calls.
    let mut prototype = incoming.clone();
    if prototype.id.is_none() {
        prototype.id = Some(xmpp_parsers::message::Id(uuid::Uuid::new_v4().to_string()));
    }
    prototype.type_ = XmppMessageType::Groupchat;
    // Strip any client-claimed `<stanza-id by='room'/>` so the chain's
    // canonicalize handler stamps the canonical value. Mirrors the
    // legacy `remove_stanza_ids_by` call.
    remove_stanza_ids_by(&mut prototype, &jid::Jid::from(room_jid.clone()));
    // 2. Look up the room actor + snapshot in one round-trip each.
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        // XEP-0045 §7.4 (#1263): a groupchat message to a room that does
        // not exist SHOULD be answered with `<item-not-found/>` — never
        // silently dropped. (A dormant/reaped room also has no live
        // occupancy, so the sender could not be an occupant of it.)
        Ok(None) => {
            clear_provisional_shadow_room_fence(deps);
            debug!(
                room = %room_jid,
                "DispatchToRoom: room not registered; bouncing item-not-found"
            );
            push_sender_error_reply(
                deps,
                &mut outcome,
                &incoming,
                &room_jid,
                &sender_full,
                item_not_found_error("Requested room not found."),
            );
            deps.capture_marker(ShadowDecisionMarker::SemanticRejected {
                reason: ShadowSemanticRejectedReason::MalformedPayload,
            });
            return outcome;
        }
        // Transient lookup failure (#1263): surface an error to the
        // sender instead of silently losing the message.
        Err(error) => {
            clear_provisional_shadow_room_fence(deps);
            warn!(
                room = %room_jid,
                error = ?error,
                "DispatchToRoom: room registry lookup failed; bouncing internal-server-error"
            );
            push_sender_error_reply(
                deps,
                &mut outcome,
                &incoming,
                &room_jid,
                &sender_full,
                room_lookup_internal_error(),
            );
            return outcome;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender_full.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot,
        // Snapshot failure (#1263): same rule — the sender must learn
        // their message did not reach the room.
        Err(error) => {
            clear_provisional_shadow_room_fence(deps);
            warn!(
                room = %room_jid,
                error = ?error,
                "DispatchToRoom: GetRoomSnapshot failed; bouncing internal-server-error"
            );
            push_sender_error_reply(
                deps,
                &mut outcome,
                &incoming,
                &room_jid,
                &sender_full,
                room_lookup_internal_error(),
            );
            return outcome;
        }
    };
    if let (Some(capture), Some(claim_fence)) = (
        deps.ingress_effect_capture.as_ref(),
        snapshot.claim_fence.as_ref(),
    ) {
        capture.record_room_fence(IngressShadowRoomFence::from_context(&room_jid, claim_fence));
    }

    // ADR-0017 Phase 3 Slice 7: the two-part demotion protocol's
    // guaranteed backstop — a fenced `SELECT ... FOR SHARE` against this
    // room's Postgres claim, run before any local fan-out (this is the
    // actual production fan-out call site; the legacy
    // `RoomActor::BuildGroupchatBroadcast` message this check was
    // originally sketched against is superseded by this
    // `GetRoomSnapshot`-based sans-I/O chain and carries no live
    // production caller). `None` (clustering disabled, non-Postgres, or a
    // build without the `clustering` feature) skips this entirely —
    // single-node behavior, unchanged. A transient backend error fails
    // open (logged, not blocking); only a definitive non-serving result
    // demotes. That includes a missing exact row and local identity rotation.
    #[cfg(feature = "clustering")]
    if let Some(store) = &state.deps.app_state.clustering_claims.muc_durable_store {
        match store.check_fenced_fanout(&room_jid).await {
            Ok(true) => {}
            Ok(false) => {
                warn!(
                    room = %room_jid,
                    "DispatchToRoom: retained room fence is non-serving; evicting the local \
                     room actor and bouncing the sender"
                );
                match room_registry
                    .ask(DemoteRoomIfExactActor {
                        room_jid: room_jid.clone(),
                        actor_ref: room_actor,
                    })
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => warn!(
                        room = %room_jid,
                        "DispatchToRoom: exact actor demotion found a different room incarnation"
                    ),
                    Err(error) => warn!(
                        room = %room_jid,
                        %error,
                        "DispatchToRoom: failed to ask registry to evict non-serving local actor"
                    ),
                }
                push_sender_error_reply(
                    deps,
                    &mut outcome,
                    &incoming,
                    &room_jid,
                    &sender_full,
                    resource_constraint_error(
                        "This room is temporarily unavailable; please retry.",
                    ),
                );
                deps.capture_marker(ShadowDecisionMarker::AuthorizationDenied {
                    reason: ShadowAuthorizationDeniedReason::Forbidden,
                });
                return outcome;
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "DispatchToRoom: fenced ownership check failed (transient backend \
                     error); failing open, not demoting"
                );
            }
        }
    }

    // 3. Managed-room owner override (announcements room admits
    //    server owners only). Pre-derived synchronously here so the
    //    chain's `OccupancyValidationHandler` can read
    //    `managed_room_forbidden` without an async permission call.
    let managed_room_forbidden =
        if parse_managed_room_jid(&room_jid).as_deref() == Some("announcements") {
            !session_is_server_owner(state, deps.authenticated_principal).await
        } else {
            false
        };

    // 4. Run the chain's occupancy / managed-room gate FIRST, BEFORE
    //    rich-target validation (Copilot review on PR #279). Otherwise
    //    a non-occupant or managed-room-forbidden sender would receive
    //    rich-target errors (potentially leaking archive-derived info
    //    like `<item-not-found/>`) instead of the required XEP-0045
    //    §7.4 `<not-acceptable/>` / managed-room `<forbidden/>` reply.
    //    The gate handler (`OccupancyValidationHandler`) is sync and
    //    pure — calling it directly here is equivalent to running a
    //    one-handler dispatcher, with no extra allocation.
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
    let id_gen = UuidV4Generator;
    // Capture a single dispatch timestamp here so every
    // `ProjectGroupchatInbox` event the chain emits carries the same
    // value (Copilot review on PR #279). Avoids per-projection
    // `Utc::now()` drift across a second-boundary.
    let dispatch_timestamp = chrono::Utc::now().timestamp();
    normalize_thread_create_source(&mut prototype);
    let gate_ctx = RoomContext {
        room: &room_jid,
        sender_full: &sender_full,
        occupants: &occupants,
        durable_recipient_bare_jids: &durable_recipient_bare_jids,
        managed_room_forbidden,
        room_moderated: snapshot.config.moderated,
        room_occupants_may_change_subject: snapshot.config.occupants_may_change_subject,
        room_members_only: snapshot.config.members_only,
        pin_permission: snapshot.config.pin_permission,
        id_gen: &id_gen,
        occupant_id_secret: &state.deps.occupant_id_secret,
        sender_nickname_generation: snapshot.sender_nickname_generation.unwrap_or(0),
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp,
    };
    let mut gate_working = prototype.clone();
    remove_framework_envelopes(&mut gate_working);
    use waddle_xmpp::protocol::room::RoomHandler;
    let gate_outcome =
        waddle_xmpp::protocol::room::occupancy_validation::OccupancyValidationHandler
            .handle(&mut gate_working, &gate_ctx);
    if let waddle_xmpp::protocol::room::RoomHandlerOutcome::Halt(gate_events) = gate_outcome {
        deps.capture_marker(ShadowDecisionMarker::AuthorizationDenied {
            reason: ShadowAuthorizationDeniedReason::Forbidden,
        });
        // Fold the nested outcome's full state — frames, close
        // signal, and async-callback feedback — back into the outer
        // outcome (Copilot review on PR #279). Dropping `close` /
        // `feedback` would silently lose stream-close requests or
        // pending callback completions if a future gate handler ever
        // emits them.
        let nested = Box::pin(interpret_with_depth(gate_events, deps, recursion_depth)).await;
        outcome.frames.extend(nested.frames);
        outcome.close = outcome.close || nested.close;
        outcome.feedback.extend(nested.feedback);
        // The gate cannot emit ArchiveGroupchat. Any future batch-local retry
        // marker is deliberately not folded into the enclosing dispatch.
        return outcome;
    }

    // Notification activity ingest (slice 2b): a XEP-0085 chat-state on
    // an inbound MUC stanza represents the sender being currently
    // active in the room. Record `(sender_bare, room)` only AFTER the
    // occupancy / managed-room gate has admitted the sender — otherwise
    // a non-occupant or managed-room-forbidden sender whose stanza is
    // rejected could still bump their activity projection and appear
    // "recently active" for the XEP-0513 `<active/>` filter (Codex
    // review on PR #731).
    super::notification_activity_ingest::record_chat_state_activity(
        deps,
        &sender_full.to_bare(),
        &room_jid,
        &incoming,
    )
    .await;

    if message_has_framework_envelope(&prototype) {
        let mut sanitized = incoming.clone();
        remove_framework_envelopes(&mut sanitized);
        push_sender_error_reply(
            deps,
            &mut outcome,
            &sanitized,
            &room_jid,
            &sender_full,
            bad_request_error("Client-authored Waddle extension envelopes are not allowed."),
        );
        deps.capture_marker(ShadowDecisionMarker::SemanticRejected {
            reason: ShadowSemanticRejectedReason::ClientAuthoredFrameworkEnvelope,
        });
        return outcome;
    }

    // 5. Enrich the message before the post-gate chain sees it. The legacy
    //    bridge enriched on the prototype before
    //    `BuildGroupchatBroadcast`, so reflected copies carry the
    //    enrichment payloads. Fail-open: extension errors leave the
    //    message unchanged.
    let waddle_id = waddle_id_for_room_jid(&room_jid);
    let sender_room_nick_jid = snapshot
        .sender_nick
        .as_deref()
        .and_then(|nick| room_jid.clone().with_resource_str(nick).ok().map(Jid::from));
    if let Some(sender_room_nick_jid) = sender_room_nick_jid.as_ref() {
        prototype.from = Some(sender_room_nick_jid.clone());
    }
    let _extension_outcome = state
        .deps
        .protocol
        .extension_manager
        .process_message_enrichments_for_waddle_with_requester(
            &mut prototype,
            waddle_id,
            Some(sender_full.to_bare()),
        )
        .await;
    // 6. Rich-target validation against the room archive. Runs only
    //    after the gate has admitted the sender, so non-occupants /
    //    managed-room-forbidden senders never see archive-derived
    //    error conditions. Archive rows store `from` in the XEP-0045
    //    §7.2.13 `room/nick` form (the chain stamps it AFTER
    //    validation), so derive that view here for the same-sender
    //    comparison rather than relying on `prototype.from` (alice's
    //    real full JID).
    if let Err(stanza_error) = validate_groupchat_rich_targets(
        deps,
        &room_jid,
        &prototype,
        sender_room_nick_jid.as_ref(),
        &room_actor,
        snapshot.sender_nickname_generation,
    )
    .await
    {
        let marker = match stanza_error.defined_condition {
            DefinedCondition::Forbidden | DefinedCondition::NotAcceptable => {
                ShadowDecisionMarker::AuthorizationDenied {
                    reason: ShadowAuthorizationDeniedReason::Forbidden,
                }
            }
            _ => ShadowDecisionMarker::SemanticRejected {
                reason: ShadowSemanticRejectedReason::MalformedPayload,
            },
        };
        deps.capture_marker(marker);
        push_sender_error_reply(
            deps,
            &mut outcome,
            &incoming,
            &room_jid,
            &sender_full,
            stanza_error,
        );
        return outcome;
    }

    // 7. Build context + run the rest of the chain (canonicalize,
    //    archive, inbox, reflect). Reuse the `gate_ctx` config — same
    //    snapshot, same managed-room flag, same id-gen.
    let ctx = RoomContext {
        room: &room_jid,
        sender_full: &sender_full,
        occupants: &occupants,
        durable_recipient_bare_jids: &durable_recipient_bare_jids,
        managed_room_forbidden,
        // XEP-0045 §7.5 (Copilot review on PR #279): the chain's
        // `OccupancyValidationHandler` enforces visitor-may-not-speak
        // against this flag + the sender's snapshot role, replacing
        // the legacy `RoomActor::BuildGroupchatBroadcast` check that
        // previously emitted `RoomActorError::VisitorMayNotSpeak`.
        room_moderated: snapshot.config.moderated,
        room_occupants_may_change_subject: snapshot.config.occupants_may_change_subject,
        room_members_only: snapshot.config.members_only,
        pin_permission: snapshot.config.pin_permission,
        id_gen: &id_gen,
        occupant_id_secret: &state.deps.occupant_id_secret,
        // Carry the sender's nickname-generation through the chain
        // so `MucArchiveHandler` can stamp it directly on
        // `OutboundEvent::ArchiveGroupchat`. Avoids a second
        // `RoomActor::GetRoomSnapshot` round-trip per groupchat
        // archive write (Copilot review on PR #279).
        sender_nickname_generation: snapshot.sender_nickname_generation.unwrap_or(0),
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp,
    };
    let mut working = prototype;
    let fanout_started = std::time::Instant::now();
    let fanout_span = info_span!(
        "xmpp.muc.fanout",
        room = %room_jid,
        // `working` (not `incoming`): id-less client messages get a
        // server-generated UUID stamped on the prototype above, so the
        // fanout trace never collapses them onto an empty id.
        message_id = working.id.as_ref().map_or("", |id| id.0.as_str()),
        recipients = tracing::field::Empty,
    );
    // Run only the post-gate pipeline (canonicalize → archive → inbox
    // → reflector). The occupancy gate already ran above as an
    // explicit stand-alone call (Copilot review on PR #279); using
    // the full `default_room_dispatcher()` here would re-run it.
    let dispatch_outcome =
        fanout_span.in_scope(|| default_room_pipeline_dispatcher().dispatch(&mut working, &ctx));
    let observer_message = working.clone();
    let mut dispatch_events = dispatch_outcome.events;
    bind_room_claim_fence(&mut dispatch_events, snapshot.claim_fence.as_ref());
    let dispatch_events = order_subject_persistence_after_archive(dispatch_events);
    let recipients = dispatch_events
        .iter()
        .filter(|event| matches!(event, OutboundEvent::RouteToConnection { .. }))
        .count();
    fanout_span.record("recipients", recipients);
    // This is the live MUC decision boundary: the actor snapshot has admitted
    // the sender and the room pipeline has frozen the fanout before any
    // recursive delivery can mutate routing state. Capture the primary route
    // intent here so an accepted room message cannot shadow-commit only its
    // secondary archive/inbox effects.
    let room_generation = snapshot
        .claim_fence
        .as_ref()
        .and_then(|fence| u64::try_from(fence.epoch.0).ok())
        .map(EntityGeneration::from_storage)
        .unwrap_or(EntityGeneration::INITIAL);
    deps.capture_intent(IngressEffectIntent::RouteMucGroupchat {
        room: room_jid.clone(),
        occupants: occupants
            .iter()
            .map(|occupant| occupant.full_jid.clone())
            .collect(),
        reflection: sender_full.clone(),
        room_generation,
    });

    // 6. Recursively interpret the chain's emitted events. Pass the
    //    depth through unchanged: `recursion_depth` is the headless
    //    offline-recipient pass guard, and the room handler chain
    //    legitimately emits one `RouteToConnection` per occupant —
    //    including offline ones, which the `RouteToConnection` arm
    //    promotes to a headless recipient pass (depth bumped there).
    //    Bumping here would break that path for every offline
    //    occupant.
    let nested = Box::pin(
        interpret_with_depth(dispatch_events, deps, recursion_depth).instrument(fanout_span),
    )
    .await;
    waddle_xmpp::histogram_record!(
        "xmpp.muc.fanout.latency",
        "ms",
        "MUC fanout latency: accepted groupchat broadcast until all per-recipient sends are enqueued.",
        fanout_started.elapsed().as_secs_f64() * 1000.0,
    );
    let retry_suppression = nested.retry_suppression;
    outcome.frames.extend(nested.frames);
    if nested.close {
        outcome.close = true;
    }
    outcome.feedback.extend(nested.feedback);

    // The marker controls only this nested room batch. Consume it here rather
    // than folding it into the returned outcome, where it could leak into an
    // unrelated sibling event in the enclosing interpreter batch.
    if retry_suppression.is_none() {
        let mut observer_message = observer_message;
        let observer_outcome = state
            .deps
            .protocol
            .extension_manager
            .process_message_observers_for_waddle_with_requester(
                &mut observer_message,
                waddle_id_for_room_jid(&room_jid),
                Some(sender_full.to_bare()),
            )
            .await;
        for effect in observer_outcome.effects {
            if let ExtensionEffect::HostWarning(message) = effect {
                warn!(warning = %message.as_str(), "extension message observer emitted host warning");
                push_sender_error_reply(
                    deps,
                    &mut outcome,
                    &incoming,
                    &room_jid,
                    &sender_full,
                    service_unavailable_error(message.as_str()),
                );
            }
        }
    }

    outcome
}

/// Wait-class internal error for a transient room-registry / snapshot
/// failure (#1263) — context-appropriate human text (the shared
/// `internal_server_error_for_lookup` helper's text talks about archive
/// lookups; review P3 on PR #1277).
fn room_lookup_internal_error() -> xmpp_parsers::stanza_error::StanzaError {
    xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Wait,
        xmpp_parsers::stanza_error::DefinedCondition::InternalServerError,
        "en",
        "Room lookup failed; please retry.",
    )
}

fn clear_provisional_shadow_room_fence(deps: &Deps<'_>) {
    if let Some(capture) = deps.ingress_effect_capture.as_ref() {
        capture.clear_room_fence();
    }
}

/// Serialize a XEP-0045 message error reply from the room to the sender
/// and push it onto the outcome's wire frames (#1263: every pre-dispatch
/// failure must reach the sender instead of silently dropping the
/// message).
pub(crate) fn push_sender_error_reply(
    deps: &Deps<'_>,
    outcome: &mut InterpretOutcome,
    incoming: &Message,
    room_jid: &jid::BareJid,
    sender_full: &jid::FullJid,
    error: xmpp_parsers::stanza_error::StanzaError,
) {
    let condition = waddle_xmpp::StanzaErrorCondition::from_xmpp(&error.defined_condition);
    let reply = build_message_error_reply(incoming, room_jid, sender_full, error);
    match Stanza::Message(reply).to_element_string() {
        Ok(xml) => {
            deps.capture_intent(IngressEffectIntent::ErrorReply {
                recipient: sender_full.clone(),
                condition,
            });
            outcome.frames.push(xml);
        }
        Err(serialize_error) => {
            warn!(
                room = %room_jid,
                error = %serialize_error,
                "DispatchToRoom: failed to serialize sender error reply"
            );
        }
    }
}

pub(super) fn normalize_thread_create_source(message: &mut Message) -> Option<String> {
    let Some(ForumAction::CreateThread(_)) = extract_forum_action(message) else {
        return None;
    };
    let thread_id = message
        .thread
        .as_ref()
        .map(|thread| thread.id.clone())
        .or_else(|| message.id.as_ref().map(|id| id.0.clone()))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if message.id.is_none() {
        message.id = Some(xmpp_parsers::message::Id(thread_id.clone()));
    }
    if message.thread.is_none() {
        set_thread_id(message, &thread_id);
    }
    Some(thread_id)
}

/// Resolve the managed-room owner override against the deployment
/// permission actor. Mirrors the legacy
/// `session_is_server_owner` helper that lived on the legacy MUC
/// bridge — kept here so the room handler chain can stay synchronous
/// and the async permission-actor call lands in the interpreter.
async fn session_is_server_owner(
    state: &WebSocketState,
    principal: Option<crate::server::routes::websocket::ResolvedPrincipal<'_>>,
) -> bool {
    let Some(principal) = principal else {
        return false;
    };
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            subject: Subject::user(&principal.user_jid),
            permission: Permission::Owner,
        })
        .await
        .is_ok_and(|response| response.allowed)
}

#[cfg(test)]
mod room_claim_fence_tests {
    use super::*;
    use chrono::Utc;
    use waddle_xmpp::muc::{RoomClaimFenceContext, RoomSubjectTexts};
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

    fn fence(owner: &str) -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, "room@muc.example.com"),
            NodeIdentity::new(owner, "epoch-a"),
            ClaimEpoch(7),
        )
    }

    fn persist_subject_event(claim_fence: Option<RoomClaimFenceContext>) -> OutboundEvent {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room bare jid");
        OutboundEvent::PersistRoomSubject {
            room: room.clone(),
            claim_fence,
            texts: RoomSubjectTexts::from_iter([(String::new(), "subject".to_string())]),
            setter: "alice@example.com".parse().expect("setter bare jid"),
            sender: "alice@example.com/web".parse().expect("sender full jid"),
            message: Box::new(Message::new(Some(jid::Jid::from(room)))),
            setter_nick: "alice".to_string(),
            set_at: Utc::now(),
        }
    }

    fn archive_event() -> OutboundEvent {
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room bare jid");
        OutboundEvent::ArchiveGroupchat {
            room: room.clone(),
            sender: "alice@example.com/web".parse().expect("sender full jid"),
            message: Box::new(Message::new(Some(jid::Jid::from(room)))),
            sender_nickname_generation: 7,
            sender_item: None,
        }
    }

    #[test]
    fn bind_room_claim_fence_binds_missing_fence_without_overwriting_or_touching_other_events() {
        let snapshot_fence = fence("snapshot-owner");
        let other_actor_fence = fence("other-owner");
        let mut events = vec![
            persist_subject_event(None),
            OutboundEvent::CloseTransport,
            persist_subject_event(Some(other_actor_fence.clone())),
        ];

        bind_room_claim_fence(&mut events, Some(&snapshot_fence));

        let OutboundEvent::PersistRoomSubject { claim_fence, .. } = &events[0] else {
            panic!("first event must be PersistRoomSubject");
        };
        assert_eq!(claim_fence.as_ref(), Some(&snapshot_fence));
        assert!(matches!(events[1], OutboundEvent::CloseTransport));
        let OutboundEvent::PersistRoomSubject { claim_fence, .. } = &events[2] else {
            panic!("third event must be PersistRoomSubject");
        };
        assert_eq!(claim_fence.as_ref(), Some(&other_actor_fence));
    }

    #[test]
    fn subject_persistence_moves_immediately_after_archive_and_other_events_stay_stable() {
        let events = vec![
            OutboundEvent::SendKeepaliveProbe,
            persist_subject_event(None),
            OutboundEvent::CloseTransport,
            archive_event(),
            OutboundEvent::SendKeepaliveProbe,
        ];

        let reordered = order_subject_persistence_after_archive(events);

        assert!(matches!(reordered[0], OutboundEvent::SendKeepaliveProbe));
        assert!(matches!(reordered[1], OutboundEvent::CloseTransport));
        assert!(matches!(
            reordered[2],
            OutboundEvent::ArchiveGroupchat { .. }
        ));
        assert!(matches!(
            reordered[3],
            OutboundEvent::PersistRoomSubject { .. }
        ));
        assert!(matches!(reordered[4], OutboundEvent::SendKeepaliveProbe));
    }

    #[test]
    fn subject_persistence_order_is_unchanged_without_archive() {
        let events = vec![persist_subject_event(None), OutboundEvent::CloseTransport];

        let reordered = order_subject_persistence_after_archive(events);

        assert!(matches!(
            reordered[0],
            OutboundEvent::PersistRoomSubject { .. }
        ));
        assert!(matches!(reordered[1], OutboundEvent::CloseTransport));
    }

    #[test]
    fn archive_order_is_unchanged_without_subject_persistence() {
        let events = vec![OutboundEvent::CloseTransport, archive_event()];

        let reordered = order_subject_persistence_after_archive(events);

        assert!(matches!(reordered[0], OutboundEvent::CloseTransport));
        assert!(matches!(
            reordered[1],
            OutboundEvent::ArchiveGroupchat { .. }
        ));
    }
}
