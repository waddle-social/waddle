# Issue #209 — Offline DM durability + reconnect semantics: roadmap

Tracking doc for the multi-PR implementation of issue [#209](https://github.com/waddle-social/waddle/issues/209). Started from a multi-turn grilling session that locked the design tree (Q1–Q10) before any code landed; this file records what shipped, what's queued, and the rationale linking each task back to the locked decisions.

## Status at a glance

| Phase | PR | State | What landed |
|---|---|---|---|
| Foundation | [#339](https://github.com/waddle-social/waddle/pull/339) | ✅ merged | Typed `DmRouting` classifier, `pending_delivery` storage trait + in-memory + libSQL backend, `OfflineDeliveryHandler`, presence-driven flush, `<service-unavailable/>` bounce on quota |
| Slice (d) phases 2–3 | [#344](https://github.com/waddle-social/waddle/pull/344) | ✅ merged | `DatabaseSmPersistence` libSQL/Postgres backend, `InMemorySmSessionRegistry` plumbed through `SmPersistenceStorage`, restart restoration |
| Slice (d) phase 4 | [#346](https://github.com/waddle-social/waddle/pull/346) | ✅ merged | Q6 SM-expiry promotion (alt-resource → offline → service-unavailable), graceful-shutdown drain, persist-after-promotion ordering |
| Q7 lifecycle | [#358](https://github.com/waddle-social/waddle/pull/358) | 🟡 in review | SM-ack-keyed deletion of `pending_delivery` rows + pre-ack session-death re-flush |
| Janitor + flush-time block | [#348](#pr-348--claim-expiry-janitor--xep-0191-flush-time-block-re-eval--planned) | ⬜ planned | Claim-expiry janitor + XEP-0191 flush-time block re-eval |
| Receipt-time plumbing | [#349](#pr-349--original_receipt_at-plumbing-through-detachedsession--planned) | ⬜ planned | `original_receipt_at` plumbing through `DetachedSession.unacked_stanzas` |
| Test-coverage debt | [#350](#pr-350--test-coverage-debt--planned) | ⬜ planned | Un-ignore XEP-0160 integration tests + extend dedicated XEP-0198/0334/0280/0313/0203/0359/0191 suites |
| Storage performance | [#351](#pr-351--storage-performance--planned) | ⬜ planned (low priority) | N+1 `list_unacked` JOIN; atomic transactions in `Database` abstraction |

## Locked design (from grilling session)

| Lock | Decision | Status |
|---|---|---|
| Q1 | Storage architecture: MAM canonical + `pending_delivery` pointer/payload table | ✅ shipped |
| Q2 | Lifecycle: pure XEP-0160 §3 trigger at intake + SM-expiry promotion | ✅ shipped |
| Q3 | Centralized `DmRouting` classifier as single source of truth | ✅ shipped |
| Q4 | `PendingPayload::Archived(StanzaId) \| Transient(Message)` typed enum | ✅ shipped |
| Q5 | Wire shape on flush (preserve `to`, stamp stanza-id only on Archived, `<delay/>` per XEP-0203) | ✅ shipped |
| Q6 | Alt-resource → offline-storage → `<service-unavailable/>` priority chain | ✅ shipped (#346) |
| Q7a | Flush triggers on first non-neg-priority presence of fresh session | ✅ shipped |
| Q7b | SM-ack-keyed deletion of `pending_delivery` rows | 🟡 in review (#358) |
| Q7c | Pre-ack session-death → release row for re-flush | 🟡 in review (#358) |
| Q7d | Priority transition negative→non-negative also triggers flush | ✅ shipped |
| Q8=B | SM session/unacked persistence durable across restart | ✅ shipped (#344+#346) |
| Q9 | Per-recipient row count cap, server-wide config, refuse-on-overflow | ✅ shipped |
| Q10a | Single-resource flush; other resources via MAM catch-up | ✅ shipped |
| Q10b | Skip inbox bump for `Transient` payloads | ✅ shipped |
| Q10d | Server best-efforts; client dedupes via XEP-0359 stanza-id | ✅ shipped |
| Final fork #1 | XEP-0191 block list evaluated at intake AND flush | 🟡 partial — intake done, flush planned (#348) |
| Final fork #3 | `sm_sessions.carbons_enabled` persisted | ✅ shipped (#344) |

## Issue #209 acceptance criteria

| Criterion | State |
|---|---|
| Offline 1:1 chat messages durable and recoverable | ✅ |
| Reconnect/initial presence exposes missed messages without duplicates | ✅ |
| MAM includes correct missed messages unless hints forbid | ✅ |
| Inbox/unread updated for offline recipients | ✅ |
| Hints control storage and fanout | ✅ |
| Dedicated tests: offline / partial / restart / MAM catch-up / replay / hints | 🟡 partial — 19 still `#[ignore]` |

---

## PR #347 — Q7b + Q7c SM-ack lifecycle (planned)

**Goal:** make `pending_delivery` row deletion correct under crash and pre-ack session death. Currently rows are deleted on `SendResult::Sent` regardless of whether the recipient actually acknowledged; this defeats the durability guarantee.

### Acceptance

- A `pending_delivery` row is deleted only when an SM `<a/>` ack from the recovering session covers the flush stanza's outbound sequence.
- If the recovering SM session dies before ack, the row's `flushed_in_session` tag is cleared (`release_row`) so a subsequent resource picks it up.
- Stanza-id-based deduplication on the client handles the rare case where the recipient receives both the SM-replay stanza and a re-flushed pending row (locked Q10d — server is best-efforts).

### Files to touch

- `server/crates/waddle-server/src/pending_delivery.rs` — `flush_for_resource`: assign each push an outbound-sequence claim, defer delete until SM-ack.
- `server/crates/waddle-xmpp/src/stream_management/session_registry.rs` — wire SM `<a/>` handling to invoke a new `pending_delivery::confirm_acked(session_id, sequence_max)` callback, plus invoke `pending_delivery::release_session(session_id)` on session-expiry/take_session.
- `server/crates/waddle-xmpp/src/pending_delivery/storage.rs` — extend trait with `delete_acked_through(session_id, sequence_max)` so per-session range delete is one DB op.

### Test plan

- New test: `pending_row_deleted_only_after_sm_ack` — push a flush, do not ack, verify row stays; ack, verify row gone.
- New test: `pending_row_released_on_pre_ack_session_death` — push a flush, kill the session pre-ack, verify row's `flushed_in_session` cleared and a fresh resource flush picks it up.
- Un-ignore `xep0160_pending_row_survives_pre_ack_session_death_for_reflush` (currently `#[ignore]`'d).

### Risks

- The SM unacked queue's outbound-sequence numbering must align with what the flush function tags onto rows. Off-by-one risks.
- Concurrent flush + ack races — the per-session lock in storage already serializes; verify the ordering invariants.

---

## PR #348 — Claim-expiry janitor + XEP-0191 flush-time block re-eval (planned)

**Goal:** address two unrelated but small follow-ups: orphaned-claim cleanup and XEP-0191 §2 step 4 conformance at flush time.

### Acceptance

**Claim-expiry janitor:**

- A periodic task sweeps `pending_delivery` rows whose `flushed_in_session` references a session no longer in the SM registry.
- Each orphan row's `flushed_in_session` is cleared via `release_row` so it becomes eligible for re-flush.
- Orphan window is bounded — typical sweep interval 60s; configurable via `WADDLE_PENDING_DELIVERY_JANITOR_INTERVAL`.

**XEP-0191 flush-time block re-eval:**

- Before flushing a `pending_delivery` row, re-load the recipient's blocklist and skip rows whose sender is now blocked.
- Skipped rows are deleted (don't need to retry — the block is final until lifted).

### Files to touch

- `server/crates/waddle-server/src/pending_delivery.rs` — new `claim_expiry_janitor` async task spawned from `create_router`; `flush_for_resource` consults blocklist before each row.
- `server/crates/waddle-xmpp/src/pending_delivery/storage.rs` — extend trait with `list_orphaned_claims(known_sessions: &[SmSessionId]) -> Vec<PendingRowId>` so the janitor can ask one query.

### Test plan

- New test: `janitor_releases_rows_with_dead_sessions` — populate rows tagged with two sessions; remove one from registry; verify janitor releases its rows but leaves the other.
- New test: `flush_drops_pending_row_when_sender_blocked_after_intake` — un-ignore the existing one in `xep0160_offline_message_handling.rs:351` (currently TODO #209: flush-time block re-evaluation).

### Risks

- Janitor sweep intervals colliding with active flushes — the per-row lock semantics already serialize, but verify under load.

---

## PR #349 — `original_receipt_at` plumbing through `DetachedSession` (planned)

**Goal:** replace the `Utc::now()` fallback in `sm_promotion::promote_session_unacked` with the actual server-receipt time of each unacked stanza, so the XEP-0203 `<delay/>` advertised on Q6-promoted offline replays is the real failed-delivery time.

### Acceptance

- `DetachedSession.unacked_stanzas` shape changes from `Vec<(u32, String)>` to `Vec<DetachedUnackedStanza { sequence: u32, stanza_xml: String, original_receipt_at: DateTime<Utc> }>`.
- Every `record_*detached*` call site passes the actual receipt time.
- `DatabaseSmPersistence` schema gains the `original_receipt_at_ms` column on `sm_unacked` (already there but currently set to `Utc::now()` at promote time; the change is to populate it correctly at append).
- Test: `xep0160_promoted_stanzas_carry_original_receipt_time_in_delay` un-ignored.

### Files to touch

- `server/crates/waddle-xmpp/src/stream_management/session_registry.rs` — struct shape + `record_detached_outbound`, `record_detached_outbound_at`, `record_outbound_for_detached_stream*`, `record_stanza_for_detached_*`.
- `server/crates/waddle-xmpp/src/stream_management/persistence.rs` — `PersistedUnackedStanza::original_receipt_at` already exists; just ensure it's populated correctly on append (not on read).
- `server/crates/waddle-server/src/sm_promotion.rs` — drop the `original_receipt_fallback` parameter; use `row.original_receipt_at` directly.
- Cross-cutting: every site that constructs a `DetachedSession` (production + tests) gets the new field. Estimated 20+ touch sites.

### Test plan

- Un-ignore `xep0160_promoted_stanzas_carry_original_receipt_time_in_delay`.
- Un-ignore `xep0160_flushed_message_carries_delay_with_original_receipt_time` (already passing for `pending_delivery`'s direct intake path, but verify against SM-promoted rows too).
- Existing 50 SM-related + 9 sm_promotion tests must stay green after the field plumbing.

### Risks

- High touch count — 20+ files. Compile-driven refactor: change the struct, fix every break.
- Wall-clock skew across servers under clustered deployments (out of scope but worth noting in commit message).

---

## PR #350 — Test-coverage debt (planned)

**Goal:** discharge the test-coverage backlog flagged by reviewers across PRs #339, #344, #346.

### Acceptance

**Un-ignore `xep0160_offline_message_handling.rs` cases that are now testable:**

- `xep0160_pending_delivery_survives_server_restart` — drop `#[ignore]`, exercise via `DatabasePendingDeliveryStorage` with a temp file + restart.
- `xep0160_sm_session_resumable_after_server_restart` — drop `#[ignore]`, exercise via `DatabaseSmPersistence` round-trip.
- `xep0160_sm_expired_unacked_promoted_to_alt_resource_when_available` — drop `#[ignore]`, exercise via `sm_promotion::promote_session_unacked`.
- `xep0160_sm_expired_unacked_promoted_to_pending_delivery_when_no_alt_resource` — same.
- `xep0160_sm_expired_unacked_returns_service_unavailable_when_storage_refuses` — same.
- `xep0160_sm_expiry_promotion_reuses_intake_classifier` — verify by introspecting the `sm_promotion` module's classifier delegation (or via a property test).
- `xep0160_graceful_shutdown_drains_unacked_into_pending_delivery` — drop `#[ignore]`, exercise the drain task via test fixture.
- `xep0160_concurrent_resources_first_presence_wins_via_lock` — exercise via the registry-level lock test.
- `xep0160_sm_resumed_session_does_not_reflush_pending_delivery` — verify resume path doesn't re-trigger flush.
- `xep0160_sm_resumption_preserves_carbons_enabled_state` — round-trip through `SmPersistenceStorage`.
- `xep0160_flush_drops_pending_row_when_sender_blocked_after_intake` — depends on PR #348.
- `xep0160_pending_row_survives_pre_ack_session_death_for_reflush` — depends on PR #347.

**Extend dedicated XEP suites** (per CLAUDE.md "XEP custom test-suite hard rule"):

- `tests/xep0198_session_registry.rs` — add SM-persistence regression: detach session, simulate restart, resume successfully. Cover the persist-after-promotion contract on `drain_expired` / `drain_all_for_shutdown` / `confirm_drained`.
- `tests/xep0334_message_processing_hints.rs` — extend with classifier-matrix integration coverage (no-store / no-permanent-store / store / no-copy in the offline-DM context).
- `tests/xep0280_message_carbons.rs` — extend: offline flush is single-resource, not carbon-fanned-out (locked Q10a).
- `tests/xep0313_mam.rs` — extend: MAM-as-recovery for live-delivered-but-unacked at crash; `<no-permanent-store/>` not archived.
- New: `tests/xep0203_delayed_delivery.rs` — `<delay/>` shape on flush + Q6-promoted stanzas.
- New: `tests/xep0359_stanza_id.rs` — stamping invariants across MAM + flush + SM replay.
- New: `tests/xep0191_blocking_offline.rs` — block at intake AND flush (depends on #348).

### Files to touch

- `server/crates/waddle-xmpp/tests/xep0160_offline_message_handling.rs` — un-ignore + implement bodies.
- The dedicated test files listed above (extending existing ones, creating new ones).

### Risks

- Some tests require fixture setup that crosses crate boundaries (sm_promotion lives in waddle-server, but the dedicated XEP-0160 suite is in waddle-xmpp). Consider whether to mirror via a `test-utils` feature or move some tests to integration tests in waddle-server.

---

## PR #351 — Storage performance (planned, low priority)

**Goal:** address the two bot-flagged optimization items that don't affect correctness.

### Acceptance

- **Single JOIN restore** — `SmPersistenceStorage::list_all_sessions_with_unacked() -> Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>` so `restore_from_persistence` reads everything in one query rather than N+1.
- **Atomic upsert+append** — extend `crate::db::Database` with a transaction primitive (`begin_tx() -> TxGuard`); use it in `InMemorySmSessionRegistry::store_session` to make session-write + N unacked-appends atomic.

### Files to touch

- `server/crates/waddle-server/src/db/mod.rs` — add transaction support (driver-aware; `BEGIN`/`COMMIT` for both SQLite and Postgres).
- `server/crates/waddle-xmpp/src/stream_management/persistence.rs` — extend trait with the JOIN method.
- `server/crates/waddle-server/src/sm_persistence.rs` — implement the JOIN query.
- `server/crates/waddle-xmpp/src/stream_management/session_registry.rs` — use the transaction in `store_session`'s persistence write.

### Risks

- Adding transaction support to `Database` is a bigger refactor than this single feature warrants. Possibly defer indefinitely unless a real workload pushes against the N+1.

---

## Out of scope of #209

These came up during the grilling but are tracked as separate concerns:

| Item | Scope |
|---|---|
| **S2S `<service-unavailable/>` bounce** | Currently logs the conformance gap (XEP-0160 §3 step 3). Requires the s2s subsystem; not on the #209 critical path. |
| **MAM XEP-0313 §4.4 per-user storage preferences** | Classifier doesn't consult per-user MAM prefs. Acknowledged as deferred during the original grilling; separate issue. |
| **Issue [#210](https://github.com/waddle-social/waddle/issues/210) — normalize archive semantics for receipts/markers/carbons/hints/states** | The umbrella for unifying server-side semantics across XEP-0184/0333/0280/0334/0085. The typed `DmRouting` classifier from #209 is the foundation #210 will build on. |

---

## Maintenance notes

- Update this file when each PR opens, lands, and merges.
- The "Status at a glance" table is the single source of truth for sequencing.
- Any newly-discovered work should be added as a new section with PR-style breakdown (Goal / Acceptance / Files / Tests / Risks).

Last updated: 2026-05-05 (PR #346 in review pass 3, slice (d) phase 4 functionally complete).
