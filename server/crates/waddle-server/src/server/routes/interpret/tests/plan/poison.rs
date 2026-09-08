use super::*;
use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use std::collections::HashSet;
use waddle_xmpp::inbox::storage::InboxStorageError;
use waddle_xmpp::inbox::storage::{
    GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey,
};
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::mam::{
    ArchivedMessage, MamArchiveKind, MamQuery, MamResult, MamStorageError, StoreOutcome,
    TerminalTombstoneOutcome,
};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::xep::CallThreadDuration;
use waddle_xmpp_core::xep0359::OriginId;

pub(super) struct PoisonMam(pub InMemoryMamStorage);
pub(super) struct PoisonInbox(pub InMemoryInboxStorage);
#[async_trait::async_trait]
impl MamStorage for PoisonMam {
    async fn store_message(
        &self,
        _archive_jid: &BareJid,
        _message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        panic!("planning wrote MamStorage::store_message")
    }
    async fn store_message_fenced(
        &self,
        _archive_jid: &BareJid,
        _message: &ArchivedMessage,
        _fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        panic!("planning wrote MamStorage::store_message_fenced")
    }
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        self.0
            .query_messages(archive_jid, archive_kind, query)
            .await
    }
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.0.get_message(archive_id).await
    }
    async fn replace_with_tombstone(
        &self,
        _archive_id: &str,
        _tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        panic!("planning wrote MamStorage::replace_with_tombstone")
    }
    async fn replace_with_terminal_tombstone(
        &self,
        _archive_id: &str,
        _tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        panic!("planning wrote MamStorage::replace_with_terminal_tombstone")
    }
    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.0
            .get_message_by_stanza_id(archive_jid, stanza_id)
            .await
    }
    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.0
            .get_message_by_message_id(archive_jid, message_id)
            .await
    }
    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.0
            .get_message_by_sender_and_origin_id(archive_jid, archive_kind, sender, origin_id)
            .await
    }
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.0
            .get_message_by_archive_or_stanza_id(archive_jid, stanza_id)
            .await
    }
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        self.0.count_messages(room_jid).await
    }
    async fn delete_before(
        &self,
        _room_jid: &BareJid,
        _before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        panic!("planning wrote MamStorage::delete_before")
    }
}
#[async_trait::async_trait]
impl InboxStorage for PoisonInbox {
    async fn list(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        self.0.list(user).await
    }
    async fn list_threads(
        &self,
        user: &BareJid,
        room: &BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError> {
        self.0.list_threads(user, room).await
    }
    async fn list_all_threads(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        self.0.list_all_threads(user).await
    }
    async fn upsert(
        &self,
        _user: &BareJid,
        _entry: InboxEntry,
        _increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        panic!("planning wrote InboxStorage::upsert")
    }
    async fn upsert_with_groupchat_notification_recovery(
        &self,
        _user: &BareJid,
        _entry: InboxEntry,
        _increment_unread: bool,
        _recovery: Option<GroupchatNotificationRecovery>,
    ) -> Result<InboxEntry, InboxStorageError> {
        panic!("planning wrote InboxStorage::upsert_with_groupchat_notification_recovery")
    }
    async fn insert_groupchat_notification_recovery(
        &self,
        _recovery: GroupchatNotificationRecovery,
    ) -> Result<(), InboxStorageError> {
        panic!("planning wrote InboxStorage::insert_groupchat_notification_recovery")
    }
    async fn list_pending_groupchat_notification_recoveries(
        &self,
        limit: usize,
    ) -> Result<Vec<GroupchatNotificationRecovery>, InboxStorageError> {
        self.0
            .list_pending_groupchat_notification_recoveries(limit)
            .await
    }
    async fn mark_groupchat_notification_recovery_completed(
        &self,
        _key: &GroupchatNotificationRecoveryKey,
    ) -> Result<u64, InboxStorageError> {
        panic!("planning wrote InboxStorage::mark_groupchat_notification_recovery_completed")
    }
    async fn prune_completed_groupchat_notification_recoveries(
        &self,
        _cutoff_ms: i64,
        _limit: usize,
    ) -> Result<u64, InboxStorageError> {
        panic!("planning wrote InboxStorage::prune_completed_groupchat_notification_recoveries")
    }
    async fn mark_read(
        &self,
        _user: &BareJid,
        _partner: &BareJid,
        _thread_id: Option<&str>,
    ) -> Result<Option<InboxEntry>, InboxStorageError> {
        panic!("planning wrote InboxStorage::mark_read")
    }
    async fn total_unread(&self, user: &BareJid) -> Result<u64, InboxStorageError> {
        self.0.total_unread(user).await
    }
    async fn mark_call_thread_ended(
        &self,
        _room: &BareJid,
        _thread_id: &str,
        _ended: DateTime<Utc>,
        _duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        panic!("planning wrote InboxStorage::mark_call_thread_ended")
    }
    async fn mark_direct_call_thread_ended(
        &self,
        _user: &BareJid,
        _partner: &BareJid,
        _thread_id: &str,
        _ended: DateTime<Utc>,
        _duration: &CallThreadDuration,
    ) -> Result<(), InboxStorageError> {
        panic!("planning wrote InboxStorage::mark_direct_call_thread_ended")
    }
}
use waddle_xmpp::pending_delivery::storage::{
    PendingStorageError, ReleaseRowsForOutboundSequencesOutcome,
};
use waddle_xmpp::pending_delivery::{
    InsertOutcome, PendingRow, PendingRowId, SmSessionId, TombstoneScrubbedPendingRows,
};
pub(super) struct PoisonPending(pub InMemoryPendingDeliveryStorage);
#[async_trait::async_trait]
impl PendingDeliveryStorage for PoisonPending {
    async fn insert(&self, _row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::insert")
    }
    async fn insert_fenced(
        &self,
        _row: PendingRow,
        _origin_stream_id: &str,
    ) -> Result<InsertOutcome, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::insert_fenced")
    }
    async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
        self.0.list(recipient).await
    }
    async fn list_unclaimed_after(
        &self,
        recipient: &BareJid,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        self.0.list_unclaimed_after(recipient, after, limit).await
    }
    async fn list_unoutboxed_archived(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        self.0.list_unoutboxed_archived(limit).await
    }
    async fn mark_notification_outboxed(
        &self,
        _id: &PendingRowId,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::mark_notification_outboxed")
    }
    async fn claim_for_session(
        &self,
        _recipient: &BareJid,
        _session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::claim_for_session")
    }
    async fn claim_batch_for_session(
        &self,
        _recipient: &BareJid,
        _session: &SmSessionId,
        _after: Option<&PendingRowId>,
        _limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::claim_batch_for_session")
    }
    async fn delete_claimed(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::delete_claimed")
    }
    async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::delete_row")
    }
    async fn release_claim(&self, _session: &SmSessionId) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::release_claim")
    }
    async fn release_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::release_row")
    }
    async fn release_row_if_session(
        &self,
        _id: &PendingRowId,
        _expected_session: &SmSessionId,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::release_row_if_session")
    }
    async fn release_rows_for_outbound_sequences(
        &self,
        _recipient: &BareJid,
        _session: &SmSessionId,
        _sequences: &HashSet<u32>,
    ) -> ReleaseRowsForOutboundSequencesOutcome {
        panic!("planning wrote PendingDeliveryStorage::release_rows_for_outbound_sequences")
    }
    async fn record_pushed_at(
        &self,
        _id: &PendingRowId,
        _sequence: u32,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::record_pushed_at")
    }
    async fn delete_acked_in_window(
        &self,
        _session: &SmSessionId,
        _from_exclusive: u32,
        _to_inclusive: u32,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::delete_acked_in_window")
    }
    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
        self.0
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }
    async fn stamp_unstamped_claims(&self, _now_ms: i64) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::stamp_unstamped_claims")
    }
    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
        self.0.count(recipient).await
    }
    async fn delete_older_than(
        &self,
        _cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::delete_older_than")
    }
    async fn scrub_for_tombstone(
        &self,
        _target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::scrub_for_tombstone")
    }
    async fn snapshot_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<Vec<PendingRowId>, PendingStorageError> {
        self.0.snapshot_for_tombstone(target).await
    }
    async fn scrub_for_tombstone_with_row_ids(
        &self,
        _target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<TombstoneScrubbedPendingRows, PendingStorageError> {
        panic!("planning wrote PendingDeliveryStorage::scrub_for_tombstone_with_row_ids")
    }
}
