//! Implementation behind the parent module facade.

use super::apply_pin_change_event;
use super::archive_direct;
use super::archive_groupchat_event;

use super::dispatch_to_room;
use super::effects;

use super::groupchat_archive;
use super::mark_inbox_read_from_displayed;
use super::message_plan;

use super::preview_plan;
use super::project_direct_inbox;
use super::queue_offline_delivery;

use super::room_pin;
use super::room_system_message;
use super::route_to_connection;
use super::send_carbons;
use super::ArchiveGroupchatEventOutcome;

use super::InboundEvent;
use super::IngressEffectIntent;

use super::OutboundEvent;

use super::RichMessageId;
use super::Stanza;
use super::StanzaError;
use super::SubjectState;

use super::{apply_groupchat_retraction_tombstone_outcome, GroupchatRetractionTombstoneOutcome};
use super::{debug, error, info, warn};
use super::{extract_stanza_id_by, replace_stanza_id, Xep0359StanzaId};

use super::{lookup_archived_message, ToElementString};
use super::{
    persist_room_subject_event, PersistRoomSubjectEventOutcome, PersistRoomSubjectRequest,
};
use super::{project_groupchat_inbox_event, ProjectGroupchatInboxEvent};
use super::{BareJid, Jid};

use super::{Deps, InterpretOutcome, TimerCommand};
use super::{Message, XmppMessageType};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NotificationRecoverySweepOutcome {
    pub(crate) completed: usize,
    pub(crate) had_failure: bool,
}

pub(crate) async fn broadcast_room_system_message(
    deps: &Deps<'_>,
    room: BareJid,
    message: Box<Message>,
) -> Option<String> {
    room_system_message::broadcast_room_system_message_event(deps, room, message, 0).await
}

/// Execute the side effects described by `events`.
///
/// The function is `async` because future migration steps add variants
/// that genuinely require `.await` (registry lookups, actor calls, MAM
/// storage). The currently-supported variants are all synchronous, so this
/// function will return immediately for the ping/session flow.
pub async fn interpret(events: Vec<OutboundEvent>, deps: &Deps<'_>) -> InterpretOutcome {
    interpret_with_depth(events, deps, 0).await
}

/// Hard cap on the [`run_headless_recipient_pass`] recursion. The
/// outer dispatch (depth = 0) runs the sender pass; an offline-bare-JID
/// `RouteToConnection` re-enters at depth = 1 to run the recipient
/// pass; any further `RouteToConnection` from inside that pass drops
/// to prevent runaway recursion. See PR15 design notes.
pub(in crate::server::routes::interpret) const MAX_RECIPIENT_PASS_DEPTH: u8 = 1;

#[derive(Clone, Debug)]
pub(crate) struct ArchiveIdRewrite {
    pub(in crate::server::routes::interpret) by: Jid,
    pub(in crate::server::routes::interpret) from_id: String,
    pub(in crate::server::routes::interpret) to_id: String,
}

impl ArchiveIdRewrite {
    pub(in crate::server::routes) fn from_store_result(
        by: Jid,
        requested_id: String,
        stored_id: String,
    ) -> Option<Self> {
        if requested_id.is_empty() || requested_id == stored_id {
            return None;
        }
        Some(Self {
            by,
            from_id: requested_id,
            to_id: stored_id,
        })
    }

    pub(in crate::server::routes::interpret) fn rewritten_id_for(
        &self,
        by: &Jid,
        id: &str,
    ) -> Option<&str> {
        (&self.by == by && self.from_id == id).then_some(self.to_id.as_str())
    }
}

pub(in crate::server::routes::interpret) fn apply_archive_id_rewrites(
    event: &mut OutboundEvent,
    rewrites: &[ArchiveIdRewrite],
) {
    if rewrites.is_empty() {
        return;
    }
    match event {
        OutboundEvent::SendStanza(stanza) | OutboundEvent::RouteToConnection { stanza, .. } => {
            rewrite_stanza_archive_ids(stanza, rewrites);
        }
        OutboundEvent::DispatchToRoom { message, .. }
        | OutboundEvent::ArchiveGroupchat { message, .. }
        | OutboundEvent::ArchiveDirect { message, .. }
        | OutboundEvent::ProjectGroupchatInbox { message, .. }
        | OutboundEvent::SendCarbons { message, .. }
        | OutboundEvent::RequestEnrichment { message, .. }
        | OutboundEvent::ApplyGroupchatRetractionTombstone {
            retraction_message: message,
            ..
        } => {
            rewrite_message_archive_ids(message, rewrites);
        }
        OutboundEvent::ProjectInbox {
            message,
            archive_ref,
            ..
        } => {
            rewrite_message_archive_ids(message, rewrites);
            rewrite_stanza_id(archive_ref, rewrites);
        }
        OutboundEvent::QueueOfflineDelivery {
            payload,
            original_message,
            ..
        } => {
            match payload {
                waddle_xmpp::pending_delivery::PendingPayload::Archived(stanza_id) => {
                    rewrite_stanza_id(stanza_id, rewrites);
                }
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message) => {
                    rewrite_message_archive_ids(message, rewrites);
                }
            }
            rewrite_message_archive_ids(original_message, rewrites);
        }
        _ => {}
    }
}

pub(in crate::server::routes::interpret) enum BatchSuppression {
    None,
    All,
    /// Tombstone-hit swallow: everything is suppressed EXCEPT the
    /// idempotent, terminal-guarded retraction-tombstone application.
    /// A retry whose retraction-request row was itself tombstoned may
    /// still be healing a crash between the request's archive commit
    /// and the target tombstone apply — the CAS guard makes letting it
    /// through safe in every other case (Greptile review on PR #1412).
    /// `All` (ownership loss / subject-persist bounce) must NOT share
    /// this escape: a deposed node may not touch the archive at all.
    TombstoneSwallow,
}

impl BatchSuppression {
    pub(in crate::server::routes::interpret) fn allows(&self, event: &OutboundEvent) -> bool {
        match self {
            Self::None => true,
            Self::All => false,
            Self::TombstoneSwallow => {
                matches!(
                    event,
                    OutboundEvent::ApplyGroupchatRetractionTombstone { .. }
                )
            }
        }
    }
}

pub(in crate::server::routes::interpret) fn rewrite_stanza_archive_ids(
    stanza: &mut Stanza,
    rewrites: &[ArchiveIdRewrite],
) {
    if let Stanza::Message(message) = stanza {
        rewrite_message_archive_ids(message, rewrites);
    }
}

pub(crate) fn rewrite_message_archive_ids(message: &mut Message, rewrites: &[ArchiveIdRewrite]) {
    for rewrite in rewrites {
        if extract_stanza_id_by(message, &rewrite.by).as_deref() == Some(rewrite.from_id.as_str()) {
            replace_stanza_id(
                message,
                &Xep0359StanzaId::new(rewrite.to_id.clone(), rewrite.by.clone()),
            );
        }
    }
}

pub(in crate::server::routes::interpret) fn rewrite_stanza_id(
    stanza_id: &mut Xep0359StanzaId,
    rewrites: &[ArchiveIdRewrite],
) {
    for rewrite in rewrites {
        if let Some(rewritten) = rewrite.rewritten_id_for(&stanza_id.by, stanza_id.id.as_str()) {
            stanza_id.id = rewritten.to_owned();
        }
    }
}

/// Best-effort plain-text body of a message: the default-language
/// `<body/>` if present, else any body.
///
/// Shared interpret-layer helper used by both the DM and groupchat
/// notification-candidate paths (and the groupchat archive prototype) so
/// neither emission site has to reach into a sibling module for body
/// extraction.
pub(crate) fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .cloned()
}

/// Internal entry point that threads the recursion depth. The public
/// [`interpret`] starts at depth 0; the offline-recipient pass
/// re-enters at depth 1 via [`run_headless_recipient_pass`].
pub(in crate::server::routes::interpret) async fn interpret_with_depth(
    events: Vec<OutboundEvent>,
    deps: &Deps<'_>,
    recursion_depth: u8,
) -> InterpretOutcome {
    let registry = deps.connection_registry;
    let mut outcome = InterpretOutcome::default();
    let mut archive_id_rewrites: Vec<ArchiveIdRewrite> = Vec::new();
    let mut batch_suppression = BatchSuppression::None;

    for mut event in events {
        if !batch_suppression.allows(&event) {
            continue;
        }
        apply_archive_id_rewrites(&mut event, &archive_id_rewrites);
        message_plan::observe_message(deps, &event, recursion_depth);
        match event {
            OutboundEvent::SendStanza(stanza) => emit_frame(deps, &mut outcome, *stanza),
            OutboundEvent::CloseTransport => {
                outcome.close = true;
            }
            OutboundEvent::Log { level, message } => {
                // Route the log back through tracing so it ends up in the
                // application's log pipeline. We format the state-machine
                // message into the event text (via `%message`) rather than
                // as a structured field so it renders the same as the rest
                // of the codebase's logs.
                match level {
                    tracing::Level::ERROR => error!(%message, "protocol"),
                    tracing::Level::WARN => warn!(%message, "protocol"),
                    tracing::Level::INFO => info!(%message, "protocol"),
                    tracing::Level::DEBUG | tracing::Level::TRACE => {
                        debug!(%message, "protocol")
                    }
                }
            }

            // -------------------------------------------------------
            // Variants defined for future migration steps. We log only the
            // variant discriminant (and, where cheap, typed identifiers
            // like JIDs or stanza ids) — never the typed payload. Some of
            // these variants carry `Message` / `Iq` structs containing
            // user content, and their `Debug` impls would leak that
            // content into logs.
            // -------------------------------------------------------
            OutboundEvent::RouteToConnection {
                jid,
                stanza,
                call_setup,
            } => {
                outcome.route_to_connection_events += 1;
                for stanza in
                    route_to_connection(deps, jid, stanza, recursion_depth, call_setup).await
                {
                    emit_frame(deps, &mut outcome, stanza);
                }
            }
            OutboundEvent::DispatchToRoom { room, message } => {
                // #229 PR18 — MUC cutover. Replaces the legacy
                // `deliver_groupchat_via_room_actor` bridge with the
                // stateless room handler chain (Q7 option C). The
                // chain handles XEP-0045 §7.4 occupancy validation,
                // §7.5 visitor-may-not-speak, XEP-0359 stanza-id
                // stamping, XEP-0421 occupant-id stamping, XEP-0313
                // archive-eligibility, XEP-0424 retraction tombstone
                // emission, durable-recipient inbox projection, and
                // per-occupant fan-out — emitting typed
                // [`OutboundEvent`]s the interpreter then resolves
                // recursively below.
                let nested =
                    Box::pin(dispatch_to_room(deps, room, *message, recursion_depth)).await;
                let InterpretOutcome {
                    frames: nested_frames,
                    close: nested_close,
                    feedback: nested_feedback,
                    keepalive_probes: nested_probes,
                    timer_commands: nested_timer_commands,
                    // Retry suppression is local to the nested room batch;
                    // dispatch_to_room consumes it before returning so it
                    // cannot suppress unrelated siblings in this outer batch.
                    archive_id_rewrites: nested_rewrites,
                    route_to_connection_events: _,
                } = nested;
                outcome.frames.extend(nested_frames);
                if nested_close {
                    outcome.close = true;
                }
                outcome.feedback.extend(nested_feedback);
                outcome.keepalive_probes += nested_probes;
                outcome.timer_commands.extend(nested_timer_commands);
                // Carry nested rewrites forward so later events in THIS
                // batch see them too.
                archive_id_rewrites.extend(nested_rewrites);
            }
            OutboundEvent::ProjectInbox {
                owner,
                peer,
                message,
                archive_ref,
                increment_unread,
            } => {
                project_direct_inbox(deps, owner, peer, message, archive_ref, increment_unread)
                    .await;
            }
            OutboundEvent::SendCarbons {
                owner,
                message,
                kind,
                exclude,
            } => {
                send_carbons(registry, deps, owner, message, kind, exclude).await;
            }
            OutboundEvent::MarkInboxReadFromDisplayed {
                owner,
                room,
                displayed_message_id,
            } => {
                mark_inbox_read_from_displayed(deps, owner, room, displayed_message_id).await;
            }
            OutboundEvent::LookupArchivedMessage {
                id,
                archive,
                archive_kind,
                reference,
            } => {
                let result =
                    lookup_archived_message(deps, &archive, archive_kind, &reference).await;
                outcome
                    .feedback
                    .push(InboundEvent::ArchivedMessageLoaded { id, result });
            }
            OutboundEvent::RegisterConnection(jid) => {
                warn!(
                    variant = "RegisterConnection",
                    jid = %jid,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::UnregisterConnection(jid) => {
                // Deliberately a warn-only stub, NOT a registry unregister
                // (Greptile review on PR #1177). No producer emits this event,
                // and the variant carries only a `FullJid` — no ownership token
                // or SM stream id — so a plain `registry.unregister(&jid)` here
                // could evict an UNRELATED live session that holds the same full
                // JID, exactly the replacement-eviction footgun fixed on every
                // real teardown path (`unregister_if_owner` /
                // `unregister_if_sm_stream_id`). Owner-gated registry removal is
                // a server-side concern the live teardown paths already own
                // (`cleanup::cleanup_connection_shutdown`,
                // `stream_management::registration`'s resume-rollback helpers,
                // `cleanup_invalidated_detached_session`, the SM-expiry janitor);
                // the pure state machine has no ownership token to drive it
                // safely. If this is ever wired, the event MUST first carry the
                // owning session's token so the removal can be gated.
                warn!(
                    variant = "UnregisterConnection",
                    jid = %jid,
                    "OutboundEvent variant not wired in interpreter: registry \
                     unregister is owner-gated and driven by the live teardown \
                     paths, not the state machine"
                );
            }
            OutboundEvent::ArchiveGroupchat {
                room,
                sender,
                message,
                sender_nickname_generation,
                sender_item,
            } => {
                match archive_groupchat_event(
                    deps,
                    room,
                    sender,
                    message,
                    sender_nickname_generation,
                    sender_item,
                )
                .await
                {
                    ArchiveGroupchatEventOutcome::Stored(Some(rewrite)) => {
                        archive_id_rewrites.push(rewrite);
                    }
                    ArchiveGroupchatEventOutcome::Stored(None)
                    | ArchiveGroupchatEventOutcome::Skipped => {}
                    ArchiveGroupchatEventOutcome::TombstoneHit => {
                        debug!(
                            "ArchiveGroupchat: tombstone hit; silently suppressing remaining dispatch batch"
                        );
                        batch_suppression = BatchSuppression::TombstoneSwallow;
                    }
                    ArchiveGroupchatEventOutcome::OwnershipLost(bounce) => {
                        // FIX 1: not archived, not fanned out — suppress
                        // every remaining event in this batch (the
                        // reflector's fan-out for this same message) and
                        // bounce the sender with the same typed
                        // recoverable error `dispatch_to_room`'s own
                        // pre-fan-out check uses.
                        emit_frame(deps, &mut outcome, Stanza::Message(*bounce));
                        batch_suppression = BatchSuppression::All;
                    }
                }
            }
            OutboundEvent::ApplyPinChange { room, request } => {
                apply_pin_change_event(deps, room, request, recursion_depth).await;
            }
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room,
                target_message_id,
                retraction_message,
            } => {
                // This entire arm heals target cleanup even when the request
                // itself is tombstoned. Nested pin/preview/system-message
                // effects inherit that policy without affecting sibling events.
                let exempt_sink = effects::TombstoneExemptSink(deps.effects);
                let mut retraction_deps = deps.clone();
                retraction_deps.effects = &exempt_sink;
                let deps = &retraction_deps;
                let Some(mam_storage) = deps.mam_storage else {
                    debug!(
                        room = %room,
                        target = %target_message_id,
                        "ApplyGroupchatRetractionTombstone: no mam_storage in Deps; skipping"
                    );
                    continue;
                };
                let tombstone_outcome = if deps.effects.is_planning() {
                    groupchat_archive::plan_groupchat_retraction_tombstone(
                        deps,
                        &room,
                        &target_message_id,
                        &retraction_message,
                    )
                    .await
                } else {
                    apply_groupchat_retraction_tombstone_outcome(
                        mam_storage,
                        deps.sm_session_registry,
                        deps.pending_delivery_storage,
                        &room,
                        &target_message_id,
                        &retraction_message,
                    )
                    .await
                };
                if tombstone_outcome == GroupchatRetractionTombstoneOutcome::Replaced {
                    if let Some(retraction_stanza_id) =
                        groupchat_retraction_stanza_id(&retraction_message, &room)
                    {
                        deps.capture_intent(IngressEffectIntent::RetractionTombstone {
                            mutation: waddle_xmpp::ingress::RetractionTombstoneMutation {
                                archive: room.clone(),
                                target_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                                    target_message_id.clone(),
                                    jid::Jid::from(room.clone()),
                                ),
                                retraction_stanza_id,
                            },
                        });
                    }
                }
                if tombstone_outcome.tombstoned() && deps.effects.is_planning() {
                    if let Some(message_id) = RichMessageId::try_new(target_message_id.clone()) {
                        preview_plan::clear(deps, &room, &message_id).await;
                    }
                }
                if tombstone_outcome.tombstoned() && !deps.effects.is_planning() {
                    if let Some(state) = deps.web_socket_state {
                        for intent in crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                            state.deps.app_state.db_pool.global_actor(),
                            &room,
                            &target_message_id,
                        )
                        .await {
                            deps.capture_intent(intent);
                        }
                    }
                }
                // #414: cascade XEP-0424 retraction to the room's pin
                // list. If the retracted stanza-id is currently pinned,
                // remove it from the projection and broadcast a
                // synthetic unpin system message so live clients see the
                // tab update without a separate poll.
                room_pin::cascade_retraction_to_pin_list(
                    deps,
                    room,
                    target_message_id,
                    recursion_depth,
                )
                .await;
            }
            OutboundEvent::PersistRoomSubject {
                room,
                claim_fence,
                texts,
                setter,
                sender,
                message,
                setter_nick,
                set_at,
            } => {
                let subject_intent = IngressEffectIntent::RoomSubjectMutation {
                    room: room.clone(),
                    state: SubjectState {
                        texts: texts.clone(),
                        setter: setter.clone(),
                        setter_nick: setter_nick.clone(),
                        set_at,
                    },
                };
                match persist_room_subject_event(
                    deps,
                    PersistRoomSubjectRequest {
                        room,
                        claim_fence,
                        texts,
                        setter,
                        sender,
                        message,
                        setter_nick,
                        set_at,
                    },
                )
                .await
                {
                    PersistRoomSubjectEventOutcome::Committed => {
                        deps.capture_intent(subject_intent);
                    }
                    PersistRoomSubjectEventOutcome::BounceAndHalt(bounce) => {
                        let stanza = Stanza::Message(*bounce);
                        if deps.effects.is_planning() {
                            capture_serialized_error_reply(deps, &stanza);
                            deps.effects.record(effects::PlannedEffect::new(
                                effects::Effect::External(effects::ExternalEffect::Frame(
                                    Box::new(stanza),
                                )),
                            ));
                        } else {
                            match stanza.to_element_string() {
                                Ok(xml) => {
                                    capture_serialized_error_reply(deps, &stanza);
                                    outcome.frames.push(xml);
                                }
                                Err(error) => {
                                    warn!(%error, "failed to serialize subject error reply")
                                }
                            }
                        }
                        batch_suppression = BatchSuppression::All;
                    }
                }
            }
            OutboundEvent::ProjectGroupchatInbox {
                owner,
                room,
                message,
                audience,
                is_live_occupant,
                room_members_only,
                sender_can_broadcast_channel_mention,
                thread,
                dispatch_timestamp,
            } => {
                project_groupchat_inbox_event(ProjectGroupchatInboxEvent {
                    deps,
                    owner,
                    room,
                    message,
                    audience,
                    is_live_occupant,
                    room_members_only,
                    sender_can_broadcast_channel_mention,
                    thread,
                    dispatch_timestamp,
                })
                .await;
            }
            OutboundEvent::ArchiveDirect {
                archive_jid,
                from,
                to,
                message,
            } => {
                if let Some(rewrite) = archive_direct(deps, archive_jid, from, to, message).await {
                    archive_id_rewrites.push(rewrite);
                }
            }
            OutboundEvent::RequestEnrichment { id, message } => {
                let enriched = enrich_message_event(deps, *message).await;
                outcome.feedback.push(InboundEvent::EnrichmentComplete {
                    id,
                    message: Box::new(enriched),
                });
            }
            OutboundEvent::AskSfu { id, .. } => {
                warn!(
                    variant = "AskSfu",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::QueryMam { id, .. } => {
                warn!(
                    variant = "QueryMam",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::LoadScramCredentials { id, .. } => {
                warn!(
                    variant = "LoadScramCredentials",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::ValidateOAuthBearer { id, .. } => {
                warn!(
                    variant = "ValidateOAuthBearer",
                    callback_id = id.0,
                    "OutboundEvent variant not yet wired in interpreter"
                );
            }
            OutboundEvent::SendKeepaliveProbe => {
                // A liveness probe is a transport control frame, not a
                // stanza — it never serializes to XML here. The command
                // rides the outcome back to the transport adapter,
                // which maps it to its native frame (WS `Ping`).
                outcome.keepalive_probes += 1;
            }
            OutboundEvent::SetTimer { id, duration_ms } => {
                outcome
                    .timer_commands
                    .push(TimerCommand::Set { id, duration_ms });
            }
            OutboundEvent::CancelTimer(id) => {
                outcome.timer_commands.push(TimerCommand::Cancel(id));
            }
            OutboundEvent::QueueOfflineDelivery {
                recipient,
                payload,
                original_receipt_at,
                original_message,
            } => {
                queue_offline_delivery(
                    deps,
                    recipient,
                    payload,
                    original_receipt_at,
                    original_message,
                )
                .await;
            }
        }
    }

    outcome.archive_id_rewrites = archive_id_rewrites;
    outcome
}

/// Sender output remains typed throughout planning; immediate dispatch serializes
/// only when returning the transport frame, preserving error-intent timing.
pub(in crate::server::routes) fn emit_frame(
    deps: &Deps<'_>,
    outcome: &mut InterpretOutcome,
    stanza: Stanza,
) {
    if deps.effects.is_planning() {
        capture_serialized_error_reply(deps, &stanza);
        deps.effects
            .record(effects::PlannedEffect::new(effects::Effect::External(
                effects::ExternalEffect::Frame(Box::new(stanza)),
            )));
    } else {
        match stanza.to_element_string() {
            Ok(xml) => {
                capture_serialized_error_reply(deps, &stanza);
                outcome.frames.push(xml);
            }
            Err(error) => warn!(%error, "failed to serialize outbound stanza"),
        }
    }
}

pub(in crate::server::routes::interpret) fn groupchat_retraction_stanza_id(
    message: &Message,
    room: &BareJid,
) -> Option<waddle_xmpp_core::xep0359::StanzaId> {
    let by = jid::Jid::from(room.clone());
    waddle_xmpp_core::xep0359::extract_stanza_id_by(message, &by)
        .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, by))
}

/// Capture a typed message error only after its exact stanza has serialized to
/// the transport frame. Direct `SendStanza` errors include XEP-0191 sender
/// blocks as well as route fallbacks, so derive the frozen error from the
/// actual typed payload rather than reconstructing a particular error shape.
pub(crate) fn capture_serialized_error_reply(deps: &Deps<'_>, stanza: &Stanza) {
    let Stanza::Message(message) = stanza else {
        return;
    };
    if message.type_ != XmppMessageType::Error {
        return;
    }
    let Some(recipient) = message
        .to
        .as_ref()
        .and_then(|recipient| recipient.try_as_full().ok())
    else {
        return;
    };
    let Some(error_payload) = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "error")
    else {
        return;
    };
    let Ok(error) = StanzaError::try_from(error_payload.clone()) else {
        warn!("serialized message error carried an invalid stanza-error payload");
        return;
    };
    let Ok(frozen_error) = waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error) else {
        warn!("serialized message error could not be frozen for ingress");
        return;
    };
    deps.capture_intent(IngressEffectIntent::ErrorReply {
        recipient: recipient.clone(),
        error: frozen_error,
    });
}

pub(in crate::server::routes::interpret) async fn enrich_message_event(
    deps: &Deps<'_>,
    message: Message,
) -> Message {
    // Observability (#1320): time the enrichment pass and count the
    // embeds it adds as the payload-count delta before/after.
    let started = std::time::Instant::now();
    let payloads_before = message.payloads.len();
    let enriched = if deps.extension_manager.is_none() {
        debug!(
            "RequestEnrichment: no extension_manager in Deps; \
             feeding original message back unchanged"
        );
        message
    } else {
        debug!("RequestEnrichment: direct messages do not carry a typed Waddle scope; skipping");
        message
    };
    let embeds_added = enriched.payloads.len().saturating_sub(payloads_before) as u64;
    waddle_xmpp::metrics::record_extension_enrichment(
        started.elapsed().as_secs_f64() * 1000.0,
        embeds_added,
    );
    enriched
}
