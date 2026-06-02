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
