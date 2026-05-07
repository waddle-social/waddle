use super::*;

pub(super) fn message_hook_effect_launches_match_room(
    effect: &ExtensionEffect,
    source_room: Option<&RoomJid>,
) -> bool {
    let ExtensionEffect::EnrichMessage(envelope) = effect else {
        return true;
    };
    envelope.enrichments.iter().all(|enrichment| {
        enrichment.launches.iter().all(|launch| {
            let Some(source_room) = source_room else {
                return false;
            };
            launch
                .context
                .room
                .as_ref()
                .is_some_and(|launch_room| launch_room == source_room)
        })
    })
}
