# Push Notification Pipeline — XEP Coverage Table

## Overview

This document satisfies the #531 acceptance-criteria item *"Final test suite documents which XEPs each scenario covers"*. It maps every push-pipeline scenario (offline DM, MUC mention, channel/everyone, `<active/>` filter, XEP-0492 levels, per-DM XEP-0492 settings via `urn:waddle:dm-bookmarks:0`, DND, blocking, `<no-store/>`, restart-durability, payload privacy, sender self-suppression) to:

- the XEPs the scenario exercises,
- the specific test function name(s) that pin the scenario,
- the file each test lives in.

A separate table at the end maps each `waddle_push_*_total` Prometheus counter to its pipeline boundary and HELP-text location, so a future maintainer can audit metric ownership at a glance.

Line offsets are deliberately omitted — they drift on every edit. Each test is identified by **function name + the module/file it lives in** so `grep -rn "fn <name>"` resolves it.

The pipeline shape (kept here for reference):

```
inbound stanza
    │
    ▼
T0 emit — insert_offline_delivery_notification_candidate  (DM)
         insert_groupchat_notification_candidate           (MUC)
    │
    │ (typed evaluator `evaluate_push_gate_at_dispatch` runs here:
    │  blocking, XEP-0492, XEP-0334, XEP-0513 noping, XEP-0513
    │  `<active/>` skipped at T0, DND — all suppressions
    │  recorded via `waddle_push_suppressed_total{reason}`)
    │
    ▼
notification_candidates  (durable row, PRIMARY KEY enforces idempotency)
    │
    ▼
T1 drain — drain_pending_candidates_into_outbox
    │  (same typed evaluator re-runs as race-window guard;
    │   XEP-0513 `<active/>` filter consulted here against
    │   `notification_activity` projection)
    │
    ▼
notification_outbox  (durable row, coalesced by recipient + service +
                     node + conversation + thread + class)
    │
    ▼
Publish drain — drain_due_outbox_jobs
    │  (XEP-0357 publish to first-party Push Service node)
    │
    ▼
push_publish_jobs  (durable row, Push Service queue; provider
                   fanout in #528/#529/#530)
```

## Locations

- `groupchat_inbox.rs` = `crates/waddle-server/src/server/routes/interpret/groupchat_inbox.rs` (T0 groupchat classifier + its `#[cfg(test)] mod tests`)
- `notification_outbox.rs` = `crates/waddle-server/src/notification_outbox.rs` (T0/T1 evaluator, candidate/outbox stores, in-file unit + integration tests)
- `notification_settings_projection.rs` = `crates/waddle-server/src/notification_settings_projection.rs` (XEP-0492 level/source projection, DM-bookmark publish validation + projection-mutation derivation, push-dispatch reducer, in-file `#[cfg(test)] mod tests`)
- `pubsub/tests.rs` = `crates/waddle-server/src/pubsub/tests.rs` (store-layer pubsub publish/retract/purge tests, including the `urn:waddle:dm-bookmarks:0` DM carrier)
- `tests/xep0357_push_service_ws.rs` = WS-level XEP-0357 push pipeline integration tests
- `tests/xep0492_push_enforcement_ws.rs` = pure `PushDispatchDecision`/`ConversationKind` reducer matrix (3 levels × 2 mention × 3 kinds)
- `tests/xep0492_dm_bookmarks_ws.rs` = per-DM XEP-0492 end-to-end slice via the real pubsub publish/retract entry points → projection store → push gate (#720)
- `tests/xep0513_mentions_ws.rs` = WS-level XEP-0513 + §295/§303 IQ integration tests
- `tests/notifications_restart_durability_ws.rs` = WS-level restart-durability test (this PR)
- `crates/waddle-xmpp/tests/xep0513_mentions.rs` = waddle-xmpp custom XEP-0513 conformance suite

## Scenario × XEP coverage

| Scenario | XEPs exercised | Pinning test(s) |
|---|---|---|
| Offline DM produces a durable XEP-0357 push | XEP-0160 (offline storage), XEP-0313 (MAM), XEP-0359 (stable stanza id), XEP-0357 (`<publish>` payload + `urn:xmpp:push:summary` form + `message-count`) | `xep0357_offline_dm_emits_durable_summary_pubsub_publish_job` (`tests/xep0357_push_service_ws.rs`) — full WS-level pipeline; `xep0357_payload_uses_summary_form_and_waddle_context_only` (`notification_outbox.rs`) — payload shape |
| Personal MUC mention (`occupantid` / JID) | XEP-0045 (MUC), XEP-0421 (occupant-id), XEP-0513 (`<mention/>` payload) | `xep0513_groupchat_personal_mentions_match_jid_or_occupant_id_and_respect_noping` (`groupchat_inbox.rs`); `explicit_mentions_route_and_replay_from_mam` (`tests/xep0513_mentions_ws.rs`) |
| Channel/everyone mention with permission gate | XEP-0045 + XEP-0513 §"Multi-User Chats Permissions" (`mentions#channel = moderators` policy) | `xep0513_channel_mention_downgrades_to_notify_all_for_unpermitted_sender` (`groupchat_inbox.rs`); `xep0513_channel_permission_is_frozen_per_dispatch_across_all_recipients` (`crates/waddle-xmpp/src/protocol/room/inbox.rs`) — recipient-set sweep is frozen before per-recipient classification |
| `<active/>` mention narrows to currently-active recipients | XEP-0513 §"Active" + XEP-0085 chat states (the activity projection's signal) | `xep0513_active_qualifier_distinguishes_channel_mention_classification` (`groupchat_inbox.rs`) — T0 classification; `t1_active_channel_mention_with_recent_activity_delivers` / `_with_stale_activity_suppresses_with_xep0513_active_miss` / `_with_no_activity_record_suppresses` (`notification_outbox.rs`) — T1 TTL filter |
| Notify-all (`<always/>`) push for ordinary group message | XEP-0045 + XEP-0492 (`<always/>` notification level) | `t1_drain_reevaluates_xep0492_when_projection_changes_after_insert` (`notification_outbox.rs`); `xep0492_on_mention_miss_preserves_pending_delivery_for_non_mention_dm` covers the negative case |
| Muted conversation suppresses push | XEP-0492 `<never/>` | `t1_xep0492_never_records_typed_suppressed_reason`; `xep0492_never_suppression_preserves_pending_delivery_and_audit_via_metric` (preserves delivery/storage) (`notification_outbox.rs`) |
| Per-DM notification level via `urn:waddle:dm-bookmarks:0` (mute / mentions / always) | XEP-0492 (`<notify>` levels) + XEP-0163 (PEP carrier) + Waddle `urn:waddle:dm-bookmarks:0` (DM carrier, item id = contact bare JID; ADR-009) | `dm_never_override_projects_direct_row_and_suppresses_push` + `dm_on_mention_override_gates_push_on_mention_bit` + `dm_without_override_delivers_push_via_section3_default` (`tests/xep0492_dm_bookmarks_ws.rs`) — real publish → `Direct` projection row → push gate; `derives_direct_projection_from_dm_bookmark_never_override` (`notification_settings_projection.rs`) — derive-level mapping |
| DM mute (`<never/>`) suppresses push | XEP-0492 `<never/>` over the DM carrier | `dm_never_override_projects_direct_row_and_suppresses_push` (`tests/xep0492_dm_bookmarks_ws.rs`) — asserts `effective_setting` → `PushDispatchDecision::Suppressed { reason: Never }` for both mention states; `dm_bookmark_publish_then_retract_round_trips_direct_projection` (`pubsub/tests.rs`) — store-layer `Direct`/`Never` row |
| DM return-to-default retracts the carrier item | XEP-0492 §3 default (`<always/>`) + XEP-0060 `<retract>` (sparse / override-only node) | `dm_never_override_projects_direct_row_and_suppresses_push` (`tests/xep0492_dm_bookmarks_ws.rs`) — retract clears the row, gate flips back to `Deliver`; `dm_bookmark_publish_then_retract_round_trips_direct_projection` (`pubsub/tests.rs`) — retract deletes the projection row in the publish tx; `missing_dm_bookmark_notify_deletes_existing_projection` (`notification_settings_projection.rs`) — empty `<dm-bookmark/>` derives a delete |
| Malformed DM `<notify>` rejected with `<bad-request/>` | XEP-0492 §2.1 (exactly one account-wide fallback) + XEP-0060 publish | `dm_malformed_notify_is_bad_request_and_writes_no_row` (`tests/xep0492_dm_bookmarks_ws.rs`) — `<bad-request/>` + no projection row + no `pubsub_items` row; `dm_bookmark_publish_with_malformed_notify_is_bad_request` (`pubsub/tests.rs`); `malformed_dm_bookmark_notify_is_rejected_at_publish_validation` (`notification_settings_projection.rs`) — `validate_dm_bookmark_publish` rejection |
| DND state suppresses push | `urn:waddle:dnd:0` (Waddle's PEP-backed DND projection, wired through the `DndReader` trait — real PEP reader lands in #367) | `t1_active_dnd_suppresses_with_typed_reason`; `waddle_dnd_t1_suppression_persists_audit_and_keeps_storage`; `dnd_integration_with_pep_shaped_reader_suppresses_push_only` (`notification_outbox.rs`) |
| Blocked sender suppresses push | XEP-0191 (privacy lists / blocking command) | 10 tests in `notification_outbox.rs`: `candidate_worker_applies_xep0191_to_groupchat_notifications`, `publish_worker_applies_xep0191_to_groupchat_notifications`, `xep0191_full_jid_block_added_after_coalescing_suppresses_dm_push_job`, `xep0191_blocked_dm_outbox_job_does_not_publish_push_notification`, `xep0191_full_jid_block_suppresses_dm_push_candidate`, `xep0191_full_jid_block_does_not_suppress_other_sender_resource`, `xep0191_domain_block_suppresses_dm_push_candidate`, `xep0191_blocklist_load_error_preserves_outbox_job_without_spending_attempt`, `t1_xep0191_blocked_records_typed_suppressed_reason`, `xep0191_blocked_t1_suppression_keeps_pending_delivery_intact` |
| `<no-store/>` / `<no-permanent-store/>` skip push | XEP-0334 (Message Processing Hints) | `xep0334_no_store_t1_suppression_persists_audit_and_keeps_storage`; `xep0334_no_permanent_store_t1_suppression_persists_audit_and_keeps_storage` (`notification_outbox.rs`) |
| `<noping/>` suppresses mention push | XEP-0513 §"No Ping" | `t1_noping_records_typed_suppressed_reason`; `xep0513_noping_t1_suppression_persists_candidate_and_keeps_storage` (`notification_outbox.rs`) |
| Mention-count threshold ignores all mentions when exceeded | XEP-0513 §304 (`mentions#count`) | `xep0513_mention_count_exceeded_ignores_all_mentions` (`groupchat_inbox.rs`); `xep0513_mention_count_includes_xep0372_references` (XEP-0372 reference path counts toward the same cap) |
| Unsupported group URIs do NOT elevate notification class | XEP-0513 §"Multi-User Chats Permissions" (deliberate non-advertisement of `#space`/`#server`/`#associations`/`#hats`) | `xep0513_unsupported_group_uris_do_not_elevate_notification_class` (`groupchat_inbox.rs`); `xep0513_mixed_attribute_unsupported_group_does_not_elevate_to_personal` covers the mixed-attribute attack surface |
| §303 form omits unadvertised fields | XEP-0513 §303 ("MUST be present if and only if the corresponding feature is advertised") | `xep0513_permissions_form_omits_unadvertised_fields` (`crates/waddle-xmpp/tests/xep0513_mentions.rs`) |
| §295 IQ form + canonical error envelope | XEP-0513 §295 + §303 | `xep0513_permissions_form_matches_spec_shape` + `xep0513_permissions_form_omits_channel_when_not_advertised` (`crates/waddle-xmpp/tests/xep0513_mentions.rs`); `mentions_permissions_iq_get_returns_303_form` + `mentions_permissions_iq_set_returns_forbidden` + `mentions_permissions_iq_full_jid_target_returns_bad_request_with_query_echo` + `mentions_permissions_iq_service_jid_target_returns_bad_request_with_query_echo` + `mentions_permissions_iq_get_returns_form_for_never_instantiated_room` (`tests/xep0513_mentions_ws.rs`) |
| Durable rows survive process restart | Pipeline-wide durability invariant (no specific XEP — Waddle's own contract) | `push_pipeline_durable_rows_survive_server_restart` (`tests/notifications_restart_durability_ws.rs`) |
| Push payload privacy default — no body, no sender in payload | XEP-0357 §"Privacy Considerations" | `xep0357_payload_uses_summary_form_and_waddle_context_only` (`notification_outbox.rs`) explicitly asserts `last-message-body` / `last-message-sender` fields are ABSENT |
| Sender self-notification always suppressed | Pipeline-wide invariant (rejected at `NotificationCandidate::direct_message` constructor) | `self_directed_dm_candidate_is_rejected_at_constructor`; `self_directed_dm_inserts_no_candidate_row` (`notification_outbox.rs`) |

## Pipeline observability (counter ↔ pipeline boundary)

| Counter | Pipeline boundary | Documented in |
|---|---|---|
| `waddle_push_candidate_created_total` | `insert_candidate` → Inserted arm | `prometheus.rs` HELP text |
| `waddle_push_candidate_coalesced_total` | `insert_candidate` → Duplicate arm (PRIMARY KEY hit) | `prometheus.rs` HELP text |
| `xmpp.push.suppressed{reason=…}` (OTel; Mimir alias `waddle_push_suppressed_total`) | T0 / T1 typed-evaluator Suppressed arm | `telemetry/attributes.rs::PushSuppressReason`; lockstep with `SuppressedReason::as_db_value` |
| `waddle_push_outbox_published_total` | `drain_due_outbox_jobs` → Published outcome | `prometheus.rs` HELP text |
| `waddle_push_outbox_retry_scheduled_total` | `drain_due_outbox_jobs` → RetryScheduled outcome (transient failure backoff) | `prometheus.rs` HELP text |
| `waddle_push_outbox_dead_lettered_total` | `drain_due_outbox_jobs` → Failed outcome (permanent failure → terminal `failed` status) | `prometheus.rs` HELP text |

Provider-side counters (`provider_sent`, `provider_rejected`, `expired_token`) land alongside #528/#529/#530 in a follow-up. Until those land, the alert *"provider keeps returning errors"* is NOT writable from the counters in this document — the closest signal is a sustained non-zero `waddle_push_outbox_dead_lettered_total` rate, which is alert-worthy but also fires during normal provider-side device revocation.

### Counter values reset on server restart

The counters live in process-local `AtomicU64` statics, so values reset to zero whenever the server process restarts. Prometheus `rate()` and `irate()` handle counter resets correctly; `increase()` over a restart window will under-report. Alerts that aggregate over the restart boundary should prefer `rate()`-derived signals.

## Deferred to follow-up

### Provider integration (per-blocker mapping)

| Provider | Issue | AC items this PR cannot close until it lands |
|---|---|---|
| Web Push | #528 | `Web fake provider fanout` AC; `waddle_push_provider_sent_total{provider="web"}` counter; `waddle_push_provider_rejected_total{provider="web",reason}` counter |
| APNs | #529 | `APNS fake provider fanout` AC; `waddle_push_provider_sent_total{provider="apns"}`; `waddle_push_provider_rejected_total{provider="apns",reason}`; `waddle_push_provider_expired_token_total{provider="apns"}` (APNs `Unregistered` device flow) |
| FCM | #530 | `FCM fake provider fanout` AC; `waddle_push_provider_sent_total{provider="fcm"}`; `waddle_push_provider_rejected_total{provider="fcm",reason}`; `waddle_push_provider_expired_token_total{provider="fcm"}` (FCM `UNREGISTERED` device flow) |

When a provider PR lands, the counter rename / label expansion in this document MUST land in the same PR — the labels and counter shape are part of the wire contract with operators. See the "Counter values reset on server restart" section above for the PromQL implications.

### Note on `SuppressedReason::ProviderRejected` / `ProviderTokenExpired`

`crates/waddle-server/src/notification_outbox.rs` reserves these two `SuppressedReason` variants for use "by provider slices". Today they are not emitted — but readers should be aware they are **NOT** routed through the T0/T1 evaluator (`evaluate_push_gate_at_dispatch`); the evaluator runs BEFORE publish. Provider rejections arrive AFTER the XEP-0357 publish has succeeded on the XMPP boundary, so they will be wired through the post-publish callback path in #528/#529/#530 — likely as a new `NotificationOutboxPublishOutcome` variant rather than a re-routed `Suppressed` outcome. Without this clarification, a future maintainer might wire them via the suppression path and double-count (Failed + Suppressed for the same logical job).

### Other deferred items

- **`muc#roominfo` extension form mirror of §303 fields** — XEP-0513 §303 SHOULD; tracked as the TODO marker in `crates/waddle-xmpp-core/src/disco/info.rs` (see #525 closure).
