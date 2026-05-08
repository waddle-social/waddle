use super::*;

pub(super) async fn archive_groupchat_event(
    deps: &Deps<'_>,
    room: BareJid,
    sender: FullJid,
    message: Box<Message>,
    sender_nickname_generation: u64,
) {
    let Some(mam_storage) = deps.mam_storage else {
        debug!(
            room = %room,
            sender = %sender,
            "ArchiveGroupchat: no mam_storage in Deps; skipping (test fixture?)"
        );
        return;
    };
    // Per XEP-0313 §5.1.3 the eligibility check ran inside
    // `MucArchiveHandler` before this event was emitted —
    // the interpreter only persists. Mirrors the legacy
    // `archive_groupchat_message` projection: derive a
    // fresh archive id, stamp the canonical
    // `<stanza-id by='room'/>` for replay, then persist.
    // `sender_nickname_generation` rides on the event so
    // we don't pay a second `RoomActor::GetRoomSnapshot`
    // round-trip per archive write (Copilot review on
    // PR #279).
    let archive_id =
        match archive_groupchat_message(mam_storage, &room, &message, sender_nickname_generation)
            .await
        {
            Some(id) => id,
            None => return,
        };
    debug!(
        room = %room,
        archive_id,
        "ArchiveGroupchat: persisted"
    );
}
