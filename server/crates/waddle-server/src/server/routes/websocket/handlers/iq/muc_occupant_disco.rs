use super::*;
use xmpp_parsers::stanza_error::StanzaError;

/// XEP-0045 §6.6: a disco request addressed to an occupant JID
/// (`room@service/nick`) must not be answered with room information.
///
/// Returns `Some(error)` when `target_to` is a full JID on the MUC
/// domain (#1265 item 10):
/// - requester is NOT an occupant of the room → `<bad-request/>`
///   (§6.6 MUST);
/// - requester IS an occupant → Waddle does not implement the
///   optional §6.6 pass-through to the occupant's client, so the
///   truthful answer is `<feature-not-implemented/>`.
///
/// Returns `None` for every other target so callers fall through to
/// their normal routing.
pub(super) async fn muc_occupant_disco_error(
    state: &WebSocketState,
    target_to: Option<&str>,
    muc_domain: &str,
    requester: Option<&FullJid>,
) -> Option<StanzaError> {
    let room_jid = target_to
        .and_then(|target| target.split_once('/').map(|(bare, _)| bare))
        .and_then(|bare| bare.parse::<BareJid>().ok())
        .filter(|room_jid| room_jid.domain().as_str() == muc_domain && room_jid.node().is_some())?;

    let is_occupant = match requester {
        None => false,
        Some(requester) => match get_room_actor(state, &room_jid).await {
            None => false,
            Some(room_actor) => match room_actor.ask(GetSnapshot).await {
                Ok(snapshot) => snapshot.room.find_nick_by_real_jid(requester).is_some(),
                Err(error) => {
                    warn!(
                        room = %room_jid,
                        error = ?error,
                        "Failed to load room snapshot for occupant-JID disco"
                    );
                    false
                }
            },
        },
    };

    Some(if is_occupant {
        feature_not_implemented_iq_error("Disco pass-through to occupants is not supported.")
    } else {
        bad_request_iq_error("Disco requests to occupant JIDs are not allowed for non-occupants.")
    })
}
