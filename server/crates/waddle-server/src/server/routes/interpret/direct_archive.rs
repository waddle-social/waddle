use super::*;
use waddle_xmpp::ingress::{IngressEffectIntent, RetractionTombstoneMutation};
use waddle_xmpp::mam::StoreOutcome;
use waddle_xmpp_core::xep0359::{extract_stanza_id_by, StanzaId};

const DM_CALL_PENDING_TTL_SECS: i64 = 30 * 60;
const DM_CALL_ACTIVE_TTL_SECS: i64 = 12 * 60 * 60;
const DM_CALL_STATE_MAX_KEYS: usize = 4096;

pub(super) async fn archive_direct(
    deps: &Deps<'_>,
    archive_jid: BareJid,
    from: jid::Jid,
    to: jid::Jid,
    message: Box<Message>,
) -> Option<ArchiveIdRewrite> {
    let mut message = *message;
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive_jid = %archive_jid,
            from = %from,
            to = %to,
            "ArchiveDirect: no mam_storage in Deps; skipping (test fixture?)"
        );
        return None;
    };
    let canonical_from_bare = from.to_bare();
    let canonical_to_bare = to.to_bare();
    // The archive-fidelity endpoint tuple is the MAM storage shape. Existing
    // DM-adjacent projections (call threads, activity, previews) intentionally
    // keep their archive-owner/peer bare tuple. The typed tuple identifies the
    // archive pass; the serialized stanza may already carry a room-authored
    // occupant `from`, which must not turn a sender-owned MUC-PM row into a
    // synthetic recipient/self projection.
    let sender_archive = canonical_from_bare == archive_jid;
    let (projection_from, projection_to) = if sender_archive {
        (archive_jid.clone(), canonical_to_bare)
    } else {
        (canonical_from_bare, archive_jid.clone())
    };
    if let Some(active) = prepare_dm_call_thread_archive_message(
        deps,
        &archive_jid,
        &projection_from,
        &projection_to,
        &message,
    ) {
        add_dm_call_thread_archive_payloads(&mut message, &active);
    }
    // Per XEP-0313 §5.1.3, the eligibility check is
    // upstream (ArchiveHandler) — the interpreter just
    // persists. The handler also already canonicalized the
    // XEP-0359 `<stanza-id by=archive_jid/>` stamp on the
    // typed message, so the projection serializer captures
    // it for replay.
    let archived = build_direct_archived_message(
        &jid::Jid::from(archive_jid.clone()),
        from.clone(),
        to.clone(),
        &message,
    );
    let requested_archive_id = archived.id.clone();
    match mam_storage.store_message(&archive_jid, &archived).await {
        Ok(outcome) => {
            let archive_id = match outcome {
                StoreOutcome::Stored(id) | StoreOutcome::Deduplicated(id) => id,
                StoreOutcome::TombstoneHit(id) => {
                    warn!(
                        archive_jid = %archive_jid,
                        archive_id = %id,
                        "ArchiveDirect: unexpected groupchat tombstone outcome; treating as deduplicated"
                    );
                    id
                }
            };
            debug!(
                archive_jid = %archive_jid,
                archive_id,
                "ArchiveDirect: persisted"
            );
            // Capture at the successful storage boundary. A retry-deduped
            // write can resolve to an existing authoritative archive id,
            // which is the only id that may be bound into the shadow intent.
            deps.capture_intent(IngressEffectIntent::ArchiveAuthoritative {
                archive: archive_jid.clone(),
                stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                    archive_id.clone(),
                    jid::Jid::from(archive_jid.clone()),
                ),
                by: archive_jid.clone(),
            });
            // Notification activity ingest (slice 2b): the sender's
            // own archive commit is the strongest "currently active"
            // signal in a DM. `ArchiveDirect` runs twice per DM —
            // once for the sender's archive (XEP-0313 §5.1.3) and
            // once for the recipient's. Only the sender path bumps
            // `(sender, peer)` activity, gated by `archive_jid ==
            // from`: persisting the message into the recipient's
            // archive does NOT indicate the recipient is active.
            if archive_jid == projection_from {
                super::notification_activity_ingest::record_outbound_message_activity(
                    deps,
                    &projection_from,
                    &projection_to,
                    &message,
                )
                .await;
            }
            let rewrite = ArchiveIdRewrite::from_store_result(
                jid::Jid::from(archive_jid.clone()),
                requested_archive_id,
                archive_id.clone(),
            );
            maybe_project_dm_call_thread(
                deps,
                &archive_jid,
                &projection_from,
                &projection_to,
                &archive_id,
                &message,
            )
            .await;
            update_direct_link_preview_refs(
                deps,
                &archive_jid,
                &projection_from,
                &archive_id,
                &message,
            )
            .await;
            apply_direct_retraction_tombstone(deps, &archive_jid, &message).await;
            rewrite
        }
        Err(error) => {
            // Archive errors must not block dispatch — the
            // message is already on the wire to other
            // resources via routing/carbons. Log and drop.
            warn!(
                archive_jid = %archive_jid,
                from = %from,
                to = %to,
                %error,
                "ArchiveDirect: store_message failed; dropping archive write"
            );
            apply_direct_retraction_tombstone(deps, &archive_jid, &message).await;
            None
        }
    }
}

fn prepare_dm_call_thread_archive_message(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    from: &BareJid,
    to: &BareJid,
    message: &Message,
) -> Option<crate::server::routes::websocket::ActiveCallThread> {
    let state = deps.web_socket_state?;
    if !waddle_xmpp::xep::HintCarrier::has_store(message) {
        return None;
    }
    prune_dm_call_thread_state(state, chrono::Utc::now());

    let sid = jmi_sid(message, "proceed")?;
    let key = crate::server::routes::websocket::DmCallThreadKey::new(from.clone(), to.clone(), sid);
    if state
        .deps
        .protocol
        .dm_call_thread_projections
        .contains(&(archive_jid.clone(), key.clone()))
    {
        return None;
    }
    if let Some(active) = state
        .deps
        .protocol
        .dm_call_threads
        .get(&key)
        .map(|active| active.clone())
    {
        if active.initiator == *from {
            return None;
        }
        return Some(active);
    }

    let offer = state
        .deps
        .protocol
        .pending_dm_call_offers
        .get(&key)
        .map(|offer| offer.clone())?;
    if offer.initiator == *from {
        return None;
    }
    let thread_id = key.sid.0.clone();
    waddle_xmpp_core::mam::ThreadId::new(thread_id.clone())?;
    let active = crate::server::routes::websocket::ActiveCallThread {
        anchor_origin_id: String::new(),
        initiator: offer.initiator,
        media: offer.media,
        started: chrono::Utc::now(),
        thread_id,
    };
    state
        .deps
        .protocol
        .dm_call_threads
        .insert(key, active.clone());
    Some(active)
}

fn add_dm_call_thread_archive_payloads(
    message: &mut Message,
    active: &crate::server::routes::websocket::ActiveCallThread,
) {
    let Some(thread_id) = waddle_xmpp_core::mam::ThreadId::new(active.thread_id.clone()) else {
        return;
    };
    if !message.payloads.iter().any(|payload| {
        payload.name() == "thread" && payload.ns() == waddle_xmpp_core::xep0201::CLIENT_STANZA_NS
    }) {
        let thread = waddle_xmpp_core::xep0201::ThreadInfo::root(thread_id);
        message
            .payloads
            .push(waddle_xmpp_core::xep0201::build_thread_element(
                &thread,
                waddle_xmpp_core::xep0201::CLIENT_STANZA_NS,
            ));
    }
    if !message.payloads.iter().any(|payload| {
        payload.name() == "call-thread" && payload.ns() == waddle_xmpp::xep::NS_WADDLE_CALL_THREAD
    }) {
        message
            .payloads
            .push(waddle_xmpp::xep::build_call_thread_anchor(
                &waddle_xmpp::xep::CallThreadAnchor {
                    kind: waddle_xmpp::xep::CallThreadKind::Dm,
                    sid: xmpp_parsers::jingle::SessionId(active.thread_id.clone()),
                    media: active.media,
                    initiator: active.initiator.clone(),
                    started: active.started,
                },
            ));
    }
}

async fn maybe_project_dm_call_thread(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    from: &BareJid,
    to: &BareJid,
    archive_id: &str,
    message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    if !waddle_xmpp::xep::HintCarrier::has_store(message) {
        return;
    }
    prune_dm_call_thread_state(state, chrono::Utc::now());

    if let Some((sid, media)) = jmi_propose(message) {
        state.deps.protocol.pending_dm_call_offers.insert(
            crate::server::routes::websocket::DmCallThreadKey::new(from.clone(), to.clone(), sid),
            crate::server::routes::websocket::PendingDmCallOffer {
                media,
                initiator: from.clone(),
                started: chrono::Utc::now(),
            },
        );
        return;
    }

    if let Some(sid) = jmi_sid(message, "finish") {
        let key =
            crate::server::routes::websocket::DmCallThreadKey::new(from.clone(), to.clone(), sid);
        state.deps.protocol.pending_dm_call_offers.remove(&key);
        let Some((key, active)) = state.deps.protocol.dm_call_threads.remove(&key) else {
            return;
        };
        state
            .deps
            .protocol
            .dm_call_thread_projections
            .remove(&(key.low_peer.clone(), key.clone()));
        state
            .deps
            .protocol
            .dm_call_thread_projections
            .remove(&(key.high_peer.clone(), key.clone()));
        let ended = chrono::Utc::now();
        let duration = ended.signed_duration_since(active.started);
        let duration =
            waddle_xmpp::xep::CallThreadDuration::parse(&format_call_thread_duration(duration))
                .expect("formatted call-thread duration is valid");
        super::direct_call_thread::mark_direct_call_thread_ended(
            deps,
            key.low_peer,
            key.high_peer,
            active.thread_id,
            ended,
            duration,
        )
        .await;
        return;
    }

    let Some(sid) = jmi_sid(message, "proceed") else {
        return;
    };
    let key = crate::server::routes::websocket::DmCallThreadKey::new(from.clone(), to.clone(), sid);
    let Some(active) = state
        .deps
        .protocol
        .dm_call_threads
        .get(&key)
        .map(|active| active.clone())
    else {
        return;
    };
    if active.initiator == *from {
        return;
    }
    if let Some(mut active) = state.deps.protocol.dm_call_threads.get_mut(&key) {
        if active.anchor_origin_id.is_empty() {
            active.anchor_origin_id = archive_id.to_owned();
        }
    }

    let projection_key = (archive_jid.clone(), key.clone());
    if !state
        .deps
        .protocol
        .dm_call_thread_projections
        .insert(projection_key)
    {
        return;
    };
    if state
        .deps
        .protocol
        .dm_call_thread_projections
        .contains(&(key.low_peer.clone(), key.clone()))
        && state
            .deps
            .protocol
            .dm_call_thread_projections
            .contains(&(key.high_peer.clone(), key.clone()))
    {
        state.deps.protocol.pending_dm_call_offers.remove(&key);
    }

    let last_updated = crate::time::now_ms();
    let peer = if archive_jid == from { to } else { from };
    super::direct_call_thread::project_direct_call_thread_anchor(
        deps,
        archive_jid.clone(),
        peer.clone(),
        active.thread_id,
        archive_id.to_owned(),
        active.media,
        last_updated,
    )
    .await;
}

fn prune_dm_call_thread_state(
    state: &crate::server::routes::websocket::WebSocketState,
    now: chrono::DateTime<chrono::Utc>,
) {
    let expired_pending = state
        .deps
        .protocol
        .pending_dm_call_offers
        .iter()
        .filter(|entry| {
            now.signed_duration_since(entry.value().started)
                .num_seconds()
                > DM_CALL_PENDING_TTL_SECS
        })
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for key in expired_pending {
        state.deps.protocol.pending_dm_call_offers.remove(&key);
    }

    let expired_active = state
        .deps
        .protocol
        .dm_call_threads
        .iter()
        .filter(|entry| {
            now.signed_duration_since(entry.value().started)
                .num_seconds()
                > DM_CALL_ACTIVE_TTL_SECS
        })
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for key in expired_active {
        remove_dm_call_thread_state_for_key(state, &key);
    }

    prune_oldest_pending_dm_call_offers(state);
    prune_oldest_active_dm_call_threads(state);
    prune_orphan_dm_call_thread_projections(state);
}

fn prune_oldest_pending_dm_call_offers(state: &crate::server::routes::websocket::WebSocketState) {
    let len = state.deps.protocol.pending_dm_call_offers.len();
    if len <= DM_CALL_STATE_MAX_KEYS {
        return;
    }
    let mut offers = state
        .deps
        .protocol
        .pending_dm_call_offers
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().started))
        .collect::<Vec<_>>();
    offers.sort_by_key(|(_, started)| *started);
    for (key, _) in offers.into_iter().take(len - DM_CALL_STATE_MAX_KEYS) {
        state.deps.protocol.pending_dm_call_offers.remove(&key);
    }
}

fn prune_oldest_active_dm_call_threads(state: &crate::server::routes::websocket::WebSocketState) {
    let len = state.deps.protocol.dm_call_threads.len();
    if len <= DM_CALL_STATE_MAX_KEYS {
        return;
    }
    let mut threads = state
        .deps
        .protocol
        .dm_call_threads
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().started))
        .collect::<Vec<_>>();
    threads.sort_by_key(|(_, started)| *started);
    for (key, _) in threads.into_iter().take(len - DM_CALL_STATE_MAX_KEYS) {
        remove_dm_call_thread_state_for_key(state, &key);
    }
}

fn prune_orphan_dm_call_thread_projections(
    state: &crate::server::routes::websocket::WebSocketState,
) {
    let orphaned = state
        .deps
        .protocol
        .dm_call_thread_projections
        .iter()
        .filter(|entry| {
            let (_, key) = entry.key();
            !state.deps.protocol.dm_call_threads.contains_key(key)
        })
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for projection_key in orphaned {
        state
            .deps
            .protocol
            .dm_call_thread_projections
            .remove(&projection_key);
    }
}

fn remove_dm_call_thread_state_for_key(
    state: &crate::server::routes::websocket::WebSocketState,
    key: &crate::server::routes::websocket::DmCallThreadKey,
) {
    state.deps.protocol.pending_dm_call_offers.remove(key);
    state.deps.protocol.dm_call_threads.remove(key);
    state
        .deps
        .protocol
        .dm_call_thread_projections
        .remove(&(key.low_peer.clone(), key.clone()));
    state
        .deps
        .protocol
        .dm_call_thread_projections
        .remove(&(key.high_peer.clone(), key.clone()));
}

fn jmi_propose(
    message: &Message,
) -> Option<(
    xmpp_parsers::jingle::SessionId,
    waddle_xmpp::xep::CallThreadMedia,
)> {
    let propose = message.payloads.iter().find(|payload| {
        payload.name() == "propose" && payload.ns() == waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE
    })?;
    let sid = xmpp_parsers::jingle::SessionId(propose.attr("id")?.to_owned());
    Some((sid, jmi_media(propose)?))
}

fn jmi_sid(message: &Message, name: &str) -> Option<xmpp_parsers::jingle::SessionId> {
    message
        .payloads
        .iter()
        .find(|payload| {
            payload.name() == name && payload.ns() == waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE
        })
        .and_then(|payload| payload.attr("id"))
        .map(|id| xmpp_parsers::jingle::SessionId(id.to_owned()))
}

fn jmi_media(element: &Element) -> Option<waddle_xmpp::xep::CallThreadMedia> {
    let mut audio = false;
    let mut video = false;
    for child in element.children() {
        if child.name() != "description" || child.ns() != waddle_xmpp::xep::xep0167::NS_JINGLE_RTP {
            continue;
        }
        match child.attr("media") {
            Some("audio") => audio = true,
            Some("video") => video = true,
            _ => {}
        }
    }
    (audio || video).then_some(waddle_xmpp::xep::CallThreadMedia { audio, video })
}

fn format_call_thread_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("PT{hours}H{minutes}M{seconds}S")
    } else if minutes > 0 {
        format!("PT{minutes}M{seconds}S")
    } else {
        format!("PT{seconds}S")
    }
}

async fn update_direct_link_preview_refs(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    sender: &BareJid,
    archive_id: &str,
    message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let global_db_actor = state.deps.app_state.db_pool.global_actor();
    let correction_target_message_id =
        if let Some(correction) = waddle_xmpp::xep::extract_correction_from_message(message) {
            Some(
                resolve_direct_correction_target_message_id(
                    deps,
                    archive_jid,
                    sender,
                    &correction.replaces_id,
                )
                .await
                .unwrap_or_else(|| correction.replaces_id.clone()),
            )
        } else {
            None
        };
    let message_id = correction_target_message_id
        .as_deref()
        .or_else(|| message.id.as_ref().map(|id| id.0.as_str()));
    let Some(message_id) = message_id else { return };
    for intent in crate::server::routes::websocket::link_preview_refs::record_current_message_preview_refs_with_effects(
        global_db_actor,
        state.deps.auth_state.base_url.as_str(),
        archive_jid,
        message_id,
        archive_id,
        message,
    )
    .await
    {
        deps.capture_intent(intent);
    }
}

pub(super) async fn resolve_direct_correction_target_message_id(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    sender: &BareJid,
    replaces_id: &str,
) -> Option<String> {
    let mam_storage = deps.mam_storage?;
    let origin_id = waddle_xmpp_core::xep0359::OriginId::new(replaces_id);
    let sender = jid::Jid::from(sender.clone());
    mam_storage
        .get_message_by_sender_and_origin_id(
            archive_jid,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &sender,
            &origin_id,
        )
        .await
        .ok()?
        .and_then(|row| row.stanza_id.map(|stanza_id| stanza_id.id))
}

async fn apply_direct_retraction_tombstone(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    message: &Message,
) {
    // XEP-0424 §"prevent further distribution": when the
    // archived message is itself a retraction *request*,
    // replace the target message in this archive with a
    // tombstone. The dispatcher's
    // `RichTargetValidationHandler` already authorized
    // the request (same-author check via
    // `LookupArchivedMessage`), so the only remaining
    // step is the in-place tombstone replace. Mirrors
    // the legacy `apply_retraction_tombstones` helper
    // (which `handle_message` invoked inline) — once per
    // archive write so both sender's and recipient's
    // archives observe the tombstone independently.
    if let Some(waddle_xmpp::xep::xep0424::RetractionKind::Request(retraction)) =
        waddle_xmpp::xep::xep0424::extract_retraction_from_message(message)
    {
        if let Some(mam_storage) = deps.mam_storage {
            let target_stanza_id = apply_retraction_tombstone(
                mam_storage,
                deps.sm_session_registry,
                deps.pending_delivery_storage,
                archive_jid,
                &retraction.retracts_id,
                message,
            )
            .await;
            if let Some(target_stanza_id) = target_stanza_id {
                if let Some(retraction_stanza_id) = retraction_stanza_id(message, archive_jid) {
                    deps.capture_intent(IngressEffectIntent::RetractionTombstone {
                        mutation: RetractionTombstoneMutation {
                            archive: archive_jid.clone(),
                            target_stanza_id,
                            retraction_stanza_id,
                        },
                    });
                }
                if let Some(state) = deps.web_socket_state {
                    for intent in crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                        state.deps.app_state.db_pool.global_actor(),
                        archive_jid,
                        &retraction.retracts_id,
                    )
                    .await {
                        deps.capture_intent(intent);
                    }
                }
            }
        }
    }
}

fn retraction_stanza_id(message: &Message, archive: &BareJid) -> Option<StanzaId> {
    let by = jid::Jid::from(archive.clone());
    extract_stanza_id_by(message, &by).map(|id| StanzaId::new(id, by))
}
