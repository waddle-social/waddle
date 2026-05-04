//! XEP-0160: Best Practices for Handling Offline Messages — dedicated suite.
//!
//! Tracks issue #209 (durable offline DM delivery and reconnect semantics).
//!
//! The locked-in design (see PR description) is: MAM is the canonical archive,
//! `pending_delivery` is a thin durable pointer/payload table flushed on first
//! non-negative-priority presence of a fresh (non-SM-resumed) session. Tests
//! marked `#[ignore]` await the implementation; remove the attribute as each
//! capability lands.
//!
//! Citations refer to `xeps/xep-0160.xml` unless otherwise noted.

use waddle_xmpp::disco::{server_features, Feature};

// -----------------------------------------------------------------------------
// §5 Service Discovery — feature advertisement
// -----------------------------------------------------------------------------

/// XEP-0160 §5: server SHOULD advertise `msgoffline`.
#[test]
fn xep0160_server_advertises_msgoffline_feature() {
    assert!(server_features().contains(&Feature::offline_messages()));
}

// -----------------------------------------------------------------------------
// §3 Process Flow — intake (storage trigger)
// -----------------------------------------------------------------------------

/// §3 step 2/4: store offline when recipient has zero resources with
/// non-negative presence priority at intake time.
#[test]
#[ignore = "TODO #209: pending_delivery store + DmRouting classifier"]
fn xep0160_stores_offline_when_no_non_negative_resource_online() {
    todo!("classify_dm_intake produces pending=Archived(_) when online_resources is empty");
}

/// §3 step 2 (note): negative-priority resources do not count as available
/// for bare-JID delivery — server stores offline.
#[test]
#[ignore = "TODO #209: classifier respects RFC 6121 §8.5.2 priority routing"]
fn xep0160_treats_negative_priority_resources_as_unavailable_for_storage() {
    todo!("only resources with priority >= 0 count toward 'recipient is online'");
}

/// §3 step 3: when offline storage is unavailable (queue full), server
/// returns `<service-unavailable/>` to sender. Locked Q9b.
#[test]
#[ignore = "TODO #209: quota enforcement"]
fn xep0160_queue_full_returns_service_unavailable_to_sender() {
    todo!("max_rows quota enforced; bounce wire shape per RFC 6120 §8.3");
}

// -----------------------------------------------------------------------------
// §4 Handling of Message Types — eligibility matrix
// -----------------------------------------------------------------------------

/// §4: `chat` SHOULD be stored offline.
#[test]
#[ignore = "TODO #209: classifier type matrix"]
fn xep0160_chat_messages_are_stored_offline() {
    todo!();
}

/// §4: `normal` (and unset type) SHOULD be stored offline.
#[test]
#[ignore = "TODO #209: classifier type matrix"]
fn xep0160_normal_messages_are_stored_offline() {
    todo!();
}

/// §4: `chat` containing only XEP-0085 chat-state content SHOULD NOT be stored.
#[test]
#[ignore = "TODO #209: classifier excludes chat-states-only"]
fn xep0160_chat_states_only_message_is_not_stored_offline() {
    todo!();
}

/// §4: `groupchat` SHOULD NOT be stored offline.
#[test]
#[ignore = "TODO #209: classifier type matrix"]
fn xep0160_groupchat_messages_are_not_stored_offline() {
    todo!();
}

/// §4: `headline` SHOULD NOT be stored offline.
#[test]
#[ignore = "TODO #209: classifier type matrix"]
fn xep0160_headline_messages_are_not_stored_offline() {
    todo!();
}

/// §4 + RFC 6121 §8.5.2.1.4: `error` to fully-offline recipient is silently
/// dropped — neither stored nor bounced. RFC mandates MUST silently ignore.
#[test]
#[ignore = "TODO #209: classifier silently drops error-type to offline"]
fn xep0160_error_messages_are_silently_dropped_when_recipient_offline() {
    todo!();
}

// -----------------------------------------------------------------------------
// §3 Process Flow — flush (delivery on initial available presence)
// -----------------------------------------------------------------------------

/// §3 step 5: flush triggers on first non-negative-priority presence of a
/// fresh (non-SM-resumed) session. Locked Q7a.
#[test]
#[ignore = "TODO #209: presence-driven flush trigger"]
fn xep0160_flushes_on_first_non_negative_presence_of_fresh_session() {
    todo!();
}

/// §3 step 5: priority transition from negative to non-negative on a session
/// that has not yet been flushed-to also triggers flush. Locked Q7d.
#[test]
#[ignore = "TODO #209: presence-priority transition triggers flush"]
fn xep0160_negative_to_non_negative_priority_transition_triggers_flush() {
    todo!();
}

/// Locked Q7b: SM-resumed session does NOT re-flush; in-flight stanzas are
/// recovered by XEP-0198 resumption replay.
#[test]
#[ignore = "TODO #209: SM-resumed sessions skip pending_delivery re-flush"]
fn xep0160_sm_resumed_session_does_not_reflush_pending_delivery() {
    todo!();
}

/// Locked Q7c: per-user-bare-JID lock — if two resources race for first
/// presence, only one drains the queue; rows tagged `flushed_in_session`
/// rather than deleted until SM-ack.
#[test]
#[ignore = "TODO #209: per-user flush lock + flushed_in_session tagging"]
fn xep0160_concurrent_resources_first_presence_wins_via_lock() {
    todo!();
}

/// Locked Q7b: row deleted only on SM-ack of the flush stanza. If the
/// recovering session dies pre-ack, the row becomes re-flushable to a
/// subsequent resource.
#[test]
#[ignore = "TODO #209: row lifecycle keyed on SM-ack"]
fn xep0160_pending_row_survives_pre_ack_session_death_for_reflush() {
    todo!();
}

// -----------------------------------------------------------------------------
// §3 Process Flow — replayed wire shape
// -----------------------------------------------------------------------------

/// §3 example + XEP-0203 §4.1: replayed message includes `<delay/>` with
/// `from = recipient's server` and `stamp = original receipt time`
/// (NOT flush time). Locked Q5b, Q6c.
#[test]
#[ignore = "TODO #209: <delay/> stamping"]
fn xep0160_flushed_message_carries_delay_with_original_receipt_time() {
    todo!();
}

/// Locked Q5a: replayed stanza preserves the sender's original `to` (bare
/// JID) — server does not rewrite to the recovering full JID.
#[test]
#[ignore = "TODO #209: preserve original `to` on replay"]
fn xep0160_flushed_message_preserves_original_to_attribute() {
    todo!();
}

/// Locked Q5c: Archived flushes carry server-stamped XEP-0359 `<stanza-id/>`
/// matching the MAM ID; Transient flushes (no MAM row) do NOT carry one.
#[test]
#[ignore = "TODO #209: stanza-id stamping per XEP-0359 §3.1"]
fn xep0160_archived_flush_includes_xep0359_stanza_id() {
    todo!();
}

#[test]
#[ignore = "TODO #209: stanza-id stamping per XEP-0359 §3.1"]
fn xep0160_transient_flush_omits_xep0359_stanza_id() {
    todo!();
}

/// Locked Q5e: server preserves all sender-set extension elements on replay
/// (hints, origin-id, etc.) — appends but does not strip or rewrite.
#[test]
#[ignore = "TODO #209: preserve sender-set extension elements"]
fn xep0160_flushed_message_preserves_sender_extensions() {
    todo!();
}

// -----------------------------------------------------------------------------
// XEP-0334 hint interactions (locked Q3 / Q4)
// -----------------------------------------------------------------------------

/// XEP-0334 §5.2: `<no-store/>` excludes the stanza from MAM AND from
/// `pending_delivery`.
#[test]
#[ignore = "TODO #209: classifier honors <no-store/>"]
fn xep0160_no_store_hint_skips_pending_delivery() {
    todo!();
}

/// XEP-0334 §5.1 + §3 use case: `<no-permanent-store/>` excludes from MAM
/// but is eligible for `pending_delivery` as `Transient` payload. Locked Q4.
#[test]
#[ignore = "TODO #209: PendingPayload::Transient for <no-permanent-store/>"]
fn xep0160_no_permanent_store_uses_transient_pending_payload() {
    todo!();
}

/// XEP-0334 §5.4: `<store/>` overrides defaults (e.g. forces archive of a
/// `headline` or a body-less stanza). Excludes type='error' per §6 ¶3.
#[test]
#[ignore = "TODO #209: <store/> override semantics"]
fn xep0160_store_hint_forces_offline_storage_except_for_error_type() {
    todo!();
}

/// XEP-0334 §6 ¶3: hints contained in stanzas of type='error' are ignored.
#[test]
#[ignore = "TODO #209: classifier ignores hints in type='error'"]
fn xep0160_hints_in_error_stanzas_are_ignored() {
    todo!();
}

// -----------------------------------------------------------------------------
// XEP-0198 SM-expiry promotion (locked Q2 / Q6)
// -----------------------------------------------------------------------------

/// XEP-0198 §5: unacked stanzas at SM-session expiry promoted via priority
/// chain: alt-resource → offline-storage → service-unavailable. Locked Q6a.
#[test]
#[ignore = "TODO #209: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_promoted_to_alt_resource_when_available() {
    todo!();
}

#[test]
#[ignore = "TODO #209: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_promoted_to_pending_delivery_when_no_alt_resource() {
    todo!();
}

#[test]
#[ignore = "TODO #209: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_returns_service_unavailable_when_storage_refuses() {
    todo!();
}

/// Locked Q6b: SM-expiry promotion filter re-runs the intake classifier so
/// the type/hint eligibility matrix is computed in exactly one place.
#[test]
#[ignore = "TODO #209: promotion filter delegates to classify_dm_intake"]
fn xep0160_sm_expiry_promotion_reuses_intake_classifier() {
    todo!();
}

/// XEP-0198 §5: `<delay/>` on promoted stanzas uses original (failed)
/// receipt time, not SM-expiry time. Locked Q6c.
#[test]
#[ignore = "TODO #209: original receipt time on promoted <delay/>"]
fn xep0160_promoted_stanzas_carry_original_receipt_time_in_delay() {
    todo!();
}

// -----------------------------------------------------------------------------
// Server restart durability (locked Q8 = B)
// -----------------------------------------------------------------------------

/// `pending_delivery` rows survive server restart (durable DB).
#[test]
#[ignore = "TODO #209: pending_delivery durability across restart"]
fn xep0160_pending_delivery_survives_server_restart() {
    todo!();
}

/// Q8 = B: SM session bindings + unacked queue persisted; resume succeeds
/// after restart and replays unacked stanzas.
#[test]
#[ignore = "TODO #209: persistent SM sessions"]
fn xep0160_sm_session_resumable_after_server_restart() {
    todo!();
}

/// Graceful shutdown drains live SM unacked queues into `pending_delivery`
/// via the Q6 promotion path before the process exits.
#[test]
#[ignore = "TODO #209: graceful-shutdown drain"]
fn xep0160_graceful_shutdown_drains_unacked_into_pending_delivery() {
    todo!();
}

// -----------------------------------------------------------------------------
// XEP-0191 blocking interactions (locked final fork #1)
// -----------------------------------------------------------------------------

/// XEP-0191 §2 step 4: blocked sender's stanza MUST NOT be stored offline.
#[test]
#[ignore = "TODO #209: classifier consults block list at intake"]
fn xep0160_blocked_sender_does_not_create_pending_delivery_row() {
    todo!();
}

/// Locked: re-evaluate block list at flush time. If recipient blocks the
/// sender after intake but before flush, drop the queued stanza.
#[test]
#[ignore = "TODO #209: flush re-evaluates block list"]
fn xep0160_flush_drops_pending_row_when_sender_blocked_after_intake() {
    todo!();
}

// -----------------------------------------------------------------------------
// Multi-resource and carbons (locked Q10a)
// -----------------------------------------------------------------------------

/// Q10a: `pending_delivery` flush is single-resource direct delivery, not
/// XEP-0280 carbon-fanned-out. Other resources catch up via MAM.
#[test]
#[ignore = "TODO #209: flush is not carbon-fanned"]
fn xep0160_flush_is_not_copied_via_xep0280_carbons() {
    todo!();
}

/// Locked final fork #3: SM session persistence (Q8=B) includes the
/// carbons-enabled flag so resumed sessions retain fanout state.
#[test]
#[ignore = "TODO #209: persistent carbons_enabled on sm_sessions"]
fn xep0160_sm_resumption_preserves_carbons_enabled_state() {
    todo!();
}

// -----------------------------------------------------------------------------
// Dedupe (locked Q10c / Q10d)
// -----------------------------------------------------------------------------

/// Q10c: an Archived stanza delivered via flush carries the same
/// XEP-0359 `<stanza-id/>` it had in MAM — clients dedupe via that key.
#[test]
#[ignore = "TODO #209: stable stanza-id across MAM and flush"]
fn xep0160_flush_and_mam_emit_same_stanza_id_for_same_message() {
    todo!();
}

/// Q10d: server is best-efforts (per XEP-0198 §5 line 367); duplicate
/// delivery across flush + MAM-catch-up is acceptable and resolved by the
/// client. This test asserts the server does NOT silently elide on its own
/// belief about what the client has seen.
#[test]
#[ignore = "TODO #209: server does not server-side-dedupe across channels"]
fn xep0160_server_does_not_filter_duplicates_across_channels() {
    todo!();
}
