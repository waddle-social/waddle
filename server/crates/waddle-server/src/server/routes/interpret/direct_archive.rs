use super::*;

pub(super) async fn archive_direct(
    deps: &Deps<'_>,
    archive_jid: BareJid,
    from: BareJid,
    to: BareJid,
    message: Box<Message>,
) {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            archive_jid = %archive_jid,
            from = %from,
            to = %to,
            "ArchiveDirect: no mam_storage in Deps; skipping (test fixture?)"
        );
        return;
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
    match mam_storage.store_message(&archive_jid, &archived).await {
        Ok(archive_id) => {
            debug!(
                archive_jid = %archive_jid,
                archive_id,
                "ArchiveDirect: persisted"
            );
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
        }
    }

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
        waddle_xmpp::xep::xep0424::extract_retraction_from_message(&message)
    {
        apply_retraction_tombstone(
            mam_storage,
            deps.sm_session_registry,
            &archive_jid,
            &retraction.retracts_id,
            &message,
        )
        .await;
    }
}
