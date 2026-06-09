use super::*;

pub(super) async fn archive_direct(
    deps: &Deps<'_>,
    archive_jid: BareJid,
    from: BareJid,
    to: BareJid,
    message: Box<Message>,
) -> Option<ArchiveIdRewrite> {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive_jid = %archive_jid,
            from = %from,
            to = %to,
            "ArchiveDirect: no mam_storage in Deps; skipping (test fixture?)"
        );
        return None;
    };
    // Per XEP-0313 §5.1.3, the eligibility check is
    // upstream (ArchiveHandler) — the interpreter just
    // persists. The handler also already canonicalized the
    // XEP-0359 `<stanza-id by=archive_jid/>` stamp on the
    // typed message, so the projection serializer captures
    // it for replay.
    let archived = build_direct_archived_message(
        &jid::Jid::from(archive_jid.clone()),
        jid::Jid::from(from.clone()),
        jid::Jid::from(to.clone()),
        &message,
    );
    let requested_archive_id = archived.id.clone();
    match mam_storage.store_message(&archive_jid, &archived).await {
        Ok(archive_id) => {
            debug!(
                archive_jid = %archive_jid,
                archive_id,
                "ArchiveDirect: persisted"
            );
            // Notification activity ingest (slice 2b): the sender's
            // own archive commit is the strongest "currently active"
            // signal in a DM. `ArchiveDirect` runs twice per DM —
            // once for the sender's archive (XEP-0313 §5.1.3) and
            // once for the recipient's. Only the sender path bumps
            // `(sender, peer)` activity, gated by `archive_jid ==
            // from`: persisting the message into the recipient's
            // archive does NOT indicate the recipient is active.
            if archive_jid == from {
                super::notification_activity_ingest::record_outbound_message_activity(
                    deps, &from, &to, &message,
                )
                .await;
            }
            let rewrite = ArchiveIdRewrite::from_store_result(
                jid::Jid::from(archive_jid.clone()),
                requested_archive_id,
                archive_id.clone(),
            );
            maybe_project_dm_call_thread(deps, &archive_jid, &from, &to, &archive_id, &message)
                .await;
            update_direct_link_preview_refs(deps, &archive_jid, &from, &archive_id, &message).await;
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

    if let Some((sid, media)) = jmi_propose(message) {
        state.deps.protocol.pending_dm_call_offers.insert(
            crate::server::routes::websocket::DmCallThreadKey::new(
                from.clone(),
                to.clone(),
                sid.clone(),
            ),
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
    let key = crate::server::routes::websocket::DmCallThreadKey::new(
        from.clone(),
        to.clone(),
        sid.clone(),
    );
    let active = if let Some(active) = state.deps.protocol.dm_call_threads.get(&key) {
        active.clone()
    } else {
        let Some(offer) = state
            .deps
            .protocol
            .pending_dm_call_offers
            .get(&key)
            .map(|offer| offer.clone())
        else {
            return;
        };
        if offer.initiator == *from {
            return;
        }
        let active = crate::server::routes::websocket::ActiveCallThread {
            anchor_origin_id: archive_id.to_owned(),
            media: offer.media,
            started: offer.started,
            thread_id: sid.0.clone(),
        };
        state
            .deps
            .protocol
            .dm_call_threads
            .insert(key.clone(), active.clone());
        active
    };

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
    crate::server::routes::websocket::link_preview_refs::record_current_message_preview_refs(
        global_db_actor,
        state.deps.auth_state.base_url.as_str(),
        archive_jid,
        message_id,
        archive_id,
        message,
    )
    .await;
}

pub(super) async fn resolve_direct_correction_target_message_id(
    deps: &Deps<'_>,
    archive_jid: &BareJid,
    sender: &BareJid,
    replaces_id: &str,
) -> Option<String> {
    let mam_storage = deps.mam_storage?;
    let query = waddle_xmpp::mam::MamQuery {
        with: Some(jid::Jid::from(sender.clone())),
        ..Default::default()
    };
    let result = mam_storage.query_messages(archive_jid, &query).await.ok()?;
    result.messages.into_iter().find_map(|row| {
        if super::archive_lookup::row_matches_origin_id(&row, sender, replaces_id)
            || super::archive_lookup::row_matches_wire_id(&row, sender, replaces_id)
        {
            row.stanza_id.map(|stanza_id| stanza_id.id)
        } else {
            None
        }
    })
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
            let tombstoned = apply_retraction_tombstone(
                mam_storage,
                deps.sm_session_registry,
                archive_jid,
                &retraction.retracts_id,
                message,
            )
            .await;
            if tombstoned {
                if let Some(state) = deps.web_socket_state {
                    crate::server::routes::websocket::link_preview_refs::clear_current_message_preview_refs(
                        state.deps.app_state.db_pool.global_actor(),
                        archive_jid,
                        &retraction.retracts_id,
                    )
                    .await;
                }
            }
        }
    }
}
