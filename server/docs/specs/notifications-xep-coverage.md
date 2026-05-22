# Push Notification Pipeline — XEP Coverage Table

This document maps every #531 acceptance-criteria scenario to the XEPs it covers and the specific test name(s) that verify each scenario. It satisfies the #531 AC item *"Final test suite documents which XEPs each scenario covers"*.

The pipeline shape (kept here for reference):

```
inbound stanza
    │
    ▼
T0 emit — `insert_offline_delivery_notification_candidate`  (DM)
         `insert_groupchat_notification_candidate`           (MUC)
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
T1 drain — `drain_pending_candidates_into_outbox`
    │  (same typed evaluator re-runs as race-window guard;
    │   XEP-0513 `<active/>` filter consulted here against
    │   `notification_activity` projection)
    │
    ▼
notification_outbox  (durable row, coalesced by recipient + service +
                     node + conversation + thread + class)
    │
    ▼
Publish drain — `drain_due_outbox_jobs`
    │  (XEP-0357 publish to first-party Push Service node)
    │
    ▼
push_publish_jobs  (durable row, Push Service queue; provider
                   fanout in #528/#529/#530)
```

## Scenario × XEP coverage

| Scenario | XEPs exercised | Pinning test(s) |
|---|---|---|
| Offline DM produces a durable XEP-0357 push | XEP-0160 (offline storage), XEP-0313 (MAM), XEP-0359 (stable stanza id), XEP-0357 (`<publish>` payload + `urn:xmpp:push:summary` form + `message-count`) | `xep0357_offline_dm_emits_durable_summary_pubsub_publish_job` (`tests/xep0357_push_service_ws.rs:309`) — full WS-level pipeline; `xep0357_payload_uses_summary_form_and_waddle_context_only` (`notification_outbox.rs:6728`) — payload shape |
| Personal MUC mention (`occupantid` / JID) | XEP-0045 (MUC), XEP-0421 (occupant-id), XEP-0513 (`<mention/>` payload) | `xep0513_groupchat_personal_mentions_match_jid_or_occupant_id_and_respect_noping` (`groupchat_inbox.rs`); `explicit_mentions_route_and_replay_from_mam` (`tests/xep0513_mentions_ws.rs:55`) |
| Channel/everyone mention with permission gate | XEP-0045 + XEP-0513 §"Multi-User Chats Permissions" (`mentions#channel = moderators` policy) | `xep0513_channel_mention_downgrades_to_notify_all_for_unpermitted_sender` + `xep0513_channel_permission_is_frozen_per_dispatch_across_all_recipients` (`groupchat_inbox.rs`) |
| `<active/>` mention narrows to currently-active recipients | XEP-0513 §"Active" + XEP-0085 chat states (the activity projection's signal) | `xep0513_active_qualifier_distinguishes_channel_mention_classification` (`groupchat_inbox.rs`) — T0 classification; `t1_active_channel_mention_with_recent_activity_delivers` / `_with_stale_activity_suppresses_with_xep0513_active_miss` / `_with_no_activity_record_suppresses` (`notification_outbox.rs`) — T1 TTL filter |
| Notify-all (`<always/>`) push for ordinary group message | XEP-0045 + XEP-0492 (`<always/>` notification level) | `t1_drain_reevaluates_xep0492_when_projection_changes_after_insert` (`notification_outbox.rs:5420`); `xep0492_on_mention_miss_preserves_pending_delivery_for_non_mention_dm:9011` covers the negative case |
| Muted conversation suppresses push | XEP-0492 `<never/>` | `t1_xep0492_never_records_typed_suppressed_reason:8192`; `xep0492_never_suppression_preserves_pending_delivery_and_audit_via_metric:8914` (preserves delivery/storage) |
| DND state suppresses push | `urn:waddle:dnd:0` (Waddle's PEP-backed DND projection, wired through the `DndReader` trait — real PEP reader lands in #367) | `t1_active_dnd_suppresses_with_typed_reason:8682`; `waddle_dnd_t1_suppression_persists_audit_and_keeps_storage:9398`; `dnd_integration_with_pep_shaped_reader_suppresses_push_only:9474` |
| Blocked sender suppresses push | XEP-0191 (privacy lists / blocking command) | 8 tests in `notification_outbox.rs`: `xep0191_blocks_notification_candidate:3363`, `_full_jid_block_suppresses_dm_push_candidate:7393`, `_domain_block_suppresses_dm_push_candidate:7442`, `_blocked_dm_outbox_job_does_not_publish_push_notification:7369`, plus 4 more |
| `<no-store/>` / `<no-permanent-store/>` skip push | XEP-0334 (Message Processing Hints) | `xep0334_no_store_t1_suppression_persists_audit_and_keeps_storage:9254`; `xep0334_no_permanent_store_t1_suppression_persists_audit_and_keeps_storage:9325` |
| `<noping/>` suppresses mention push | XEP-0513 §"No Ping" | `t1_noping_records_typed_suppressed_reason:8490`; `xep0513_noping_t1_suppression_persists_candidate_and_keeps_storage:9173` |
| Mention-count threshold ignores all mentions when exceeded | XEP-0513 §304 (`mentions#count`) | `xep0513_mention_count_exceeded_ignores_all_mentions` (`groupchat_inbox.rs`); `xep0513_mention_count_includes_xep0372_references` (XEP-0372 reference path counts toward the same cap) |
| Unsupported group URIs do NOT elevate notification class | XEP-0513 §"Multi-User Chats Permissions" (deliberate non-advertisement of `#space`/`#server`/`#associations`/`#hats`) | `xep0513_unsupported_group_uris_do_not_elevate_notification_class` (`groupchat_inbox.rs`); `xep0513_mixed_attribute_unsupported_group_does_not_elevate_to_personal` covers the mixed-attribute attack surface |
| §303 form omits unadvertised fields | XEP-0513 §303 ("MUST be present if and only if the corresponding feature is advertised") | `xep0513_permissions_form_omits_unadvertised_fields` (`crates/waddle-xmpp/tests/xep0513_mentions.rs`) |
| §295 IQ form + canonical error envelope | XEP-0513 §295 + §303 | `xep0513_permissions_form_matches_spec_shape` + `xep0513_permissions_form_omits_channel_when_not_advertised` (`crates/waddle-xmpp/tests/xep0513_mentions.rs`); `mentions_permissions_iq_get_returns_303_form` + `_set_returns_forbidden` + `_full_jid_target_returns_bad_request_with_query_echo` + `_service_jid_target_returns_bad_request_with_query_echo` + `_get_returns_form_for_never_instantiated_room` (`crates/waddle-server/tests/xep0513_mentions_ws.rs`) |
| Durable rows survive process restart | Pipeline-wide durability invariant (no specific XEP — Waddle's own contract) | `push_pipeline_durable_rows_survive_server_restart` (`crates/waddle-server/tests/notifications_restart_durability_ws.rs`) |
| Push payload privacy default — no body, no sender in payload | XEP-0357 §"Privacy Considerations" | `xep0357_payload_uses_summary_form_and_waddle_context_only:6728` explicitly asserts `last-message-body` / `last-message-sender` fields are ABSENT |
| Sender self-notification always suppressed | Pipeline-wide invariant (rejected at `NotificationCandidate::direct_message` constructor) | `self_directed_dm_candidate_is_rejected_at_constructor:7717`; `self_directed_dm_inserts_no_candidate_row:7741` |

## Pipeline observability (counter ↔ pipeline boundary)

| Counter | Pipeline boundary | Documented in |
|---|---|---|
| `waddle_push_candidate_created_total` | `insert_candidate` → Inserted arm | `prometheus.rs` HELP text |
| `waddle_push_candidate_coalesced_total` | `insert_candidate` → Duplicate arm (PRIMARY KEY hit) | `prometheus.rs` HELP text |
| `waddle_push_suppressed_total{reason=…}` | T0 / T1 typed-evaluator Suppressed arm | `prometheus.rs::push_suppressed_reasons`; lockstep with `SuppressedReason::as_db_value` |
| `waddle_push_outbox_published_total` | `drain_due_outbox_jobs` → Published outcome | `prometheus.rs` HELP text |
| `waddle_push_outbox_retry_scheduled_total` | `drain_due_outbox_jobs` → RetryScheduled outcome (transient failure backoff) | `prometheus.rs` HELP text |
| `waddle_push_outbox_dead_lettered_total` | `drain_due_outbox_jobs` → Failed outcome (permanent failure → terminal `failed` status) | `prometheus.rs` HELP text |

Provider-side counters (`provider_sent`, `provider_rejected`, `expired_token`) land alongside #528/#529/#530 in a follow-up.

## Deferred to follow-up

- **Fake Web/APNS/FCM provider fanout tests** — block on #528/#529/#530 landing the provider boundary code with hooks for fake providers.
- **Provider-side metrics** — bumped at the per-provider dispatch boundary; same blockers.
- **`muc#roominfo` extension form mirror of §303 fields** — XEP-0513 §303 SHOULD; tracked as the TODO marker in `crates/waddle-xmpp-core/src/disco/info.rs` (see #525 closure).
