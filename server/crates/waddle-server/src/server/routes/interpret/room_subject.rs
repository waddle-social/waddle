use super::*;
use waddle_xmpp::muc::RoomSubjectTexts;

pub(super) async fn persist_room_subject_event(
    deps: &Deps<'_>,
    room: BareJid,
    texts: RoomSubjectTexts,
    setter: BareJid,
    setter_nick: String,
    set_at: chrono::DateTime<chrono::Utc>,
) {
    let Some(room_registry) = deps.room_registry else {
        debug!(
            room = %room,
            "PersistRoomSubject: no room_registry in Deps; skipping"
        );
        return;
    };
    let room_actor = match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            debug!(
                room = %room,
                "PersistRoomSubject: room not registered; skipping"
            );
            return;
        }
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "PersistRoomSubject: room registry lookup failed; skipping"
            );
            return;
        }
    };
    if let Err(error) = room_actor
        .ask(SetSubject {
            texts,
            setter: setter.clone(),
            setter_nick,
            set_at,
        })
        .await
    {
        warn!(
            room = %room,
            setter = %setter,
            error = ?error,
            "PersistRoomSubject: SetSubject ask failed; subject left at previous state"
        );
    }
}
