use std::collections::BTreeSet;

use jid::FullJid;

use super::{
    admin_handlers, DurableRestoreState, DurableRoomOrigin, LiveRosterRestoreError, RoomActor,
};
use crate::muc::{AdminMutationId, MucRoom, RoomDurableMutation};

impl RoomActor {
    /// Commit the transferred sessions' removal obligations before publication.
    /// The predecessor owns the attempt ID, so a failed preparation can be
    /// retried on a fresh successor without duplicating committed outbox rows.
    pub(super) async fn persist_live_roster_removals(
        &mut self,
        mut attempt: AdminMutationId,
        room: &MucRoom,
        removed_sessions: Vec<FullJid>,
        already_removed: &[FullJid],
    ) -> Result<Vec<FullJid>, LiveRosterRestoreError> {
        if self.restore_state != DurableRestoreState::Ready(DurableRoomOrigin::Restored) {
            // Volatile rooms have no durable authority or outbox; retain their
            // existing in-memory restoration semantics.
            return Ok(removed_sessions);
        }
        let source_sessions: BTreeSet<_> = room
            .occupants
            .keys()
            .flat_map(|nick| room.get_occupant_sessions(nick))
            .collect();
        if source_sessions.is_empty() {
            return Ok(Vec::new());
        }
        let store = self
            .durable_store
            .clone()
            .ok_or(LiveRosterRestoreError::RemovalEffectsUnavailable)?;
        let mut proven = BTreeSet::new();
        let mut receipts = Vec::new();
        let mut previous_revision = None;
        loop {
            let receipt = store
                .load_admin_mutation_receipt(&self.room.room_jid, attempt)
                .await
                .map_err(|_| LiveRosterRestoreError::RemovalEffectsUnavailable)?;
            let Some(receipt) = receipt else {
                break;
            };
            if !self.durable_coordinates.is_some_and(|current| {
                current.lifecycle == receipt.coordinates.lifecycle
                    && receipt.coordinates.revision <= current.revision
                    && previous_revision
                        .is_none_or(|previous| receipt.coordinates.revision > previous)
            }) || receipt
                .removed_sessions
                .iter()
                .any(|session| !source_sessions.contains(session))
            {
                return Err(LiveRosterRestoreError::RemovalEffectsUnavailable);
            }
            let prior_count = proven.len();
            proven.extend(receipt.removed_sessions);
            if proven.len() == prior_count {
                // Each link must account for a new source session. This both
                // rejects unrelated proof and bounds reads by the source roster.
                return Err(LiveRosterRestoreError::RemovalEffectsUnavailable);
            }
            previous_revision = Some(receipt.coordinates.revision);
            receipts.push(attempt);
            attempt = attempt.next_restore_attempt();
        }
        let remaining: Vec<_> = removed_sessions
            .into_iter()
            .filter(|session| !proven.contains(session))
            .collect();
        if !remaining.is_empty() {
            // Another owner may revoke additional members between retries.
            // Give those effects their own deterministic attempt while replaying
            // every earlier receipt's exact removals, even after a later regrant.
            let excluded: Vec<_> = already_removed
                .iter()
                .cloned()
                .chain(proven.iter().cloned())
                .collect();
            let effects =
                admin_handlers::restored_roster_removal_effects(room, &remaining, &excluded)
                    .with_admin_mutation_id(attempt);
            self.commit_durable(RoomDurableMutation::AffiliationBatch(Vec::new()), effects)
                .await
                .map_err(|_| LiveRosterRestoreError::RemovalEffectsUnavailable)?;
            receipts.push(attempt);
            proven.extend(remaining);
        }
        self.restore_receipts_after_publication = receipts;
        Ok(proven.into_iter().collect())
    }
}
