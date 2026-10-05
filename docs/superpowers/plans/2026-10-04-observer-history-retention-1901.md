# Bounded retention for terminal room-observer history (#1901) Implementation Plan

> Revised after architecture review round 1 (gpt-6-astra, high): findings R1-1..R1-5 addressed inline and marked **[R1-n]**.

**Goal:** Give the six `extension_room_*` tables (which deliberately have no FK cascade from `ingress_messages`) their own bounded, auditable retention: settled observer history ages out after a documented horizon, nothing that is still active or still needed for duplicate suppression, corrections, retractions, or pending publication is ever collected, and operators get a reviewed-manifest disposition for genuinely unsupported work.

**Spec:** GitHub issue #1901 (follow-up to #1776 / #1899). Related docs: `server/docs/operations/ingress-authority.md` (§"Retention and unresolved effects", §"Periodic maintenance", #1901 pointer at lines 1087-1093), `docs/extensions/room-observations.md`.

## Facts the design rests on

- No observer table has a timestamp column (`ingress_uow/room_observation/schema.rs`). Only `extension_room_observation_work` is in the migration ledger (V1022); the other five tables are created by startup DDL after migrations run.
- `work.status`: active = `pending|leased|started`; final = `completed|terminal|stale`. `publications.status`: `pending|published|stale`. Receipts (`extension_room_observation_receipts`, PK `(plugin_id, generation, room_jid, message_key)`) are written once at settlement and are the observer-side duplicate-suppression evidence; the ingress-side twin in `ingress_effect_receipts` already cascades with the canonical row.
- Canonical replay horizon: `ALIAS_RETENTION = 8 days` after `terminal_at` (`ingress_substrate/mod.rs:46`). Recovery (`ingress/maintenance.rs`) only touches **non-terminal** canonical rows, so an observer receipt can only be consulted again while its canonical row is non-terminal.
- Corrections and retractions resolve their target through `extension_room_source_revisions` → `extension_room_sources` (`sources.rs:157-179`, `568-596`); a missing source yields `unknown_correction_target` / no-op.
- Maintenance pass phases: attestation → terminalization → recovery → retention GC, each recorded as `ingress.maintenance.runs{phase,outcome}` (`maintenance.rs:169-242`). Attribute enums are sealed in `waddle-xmpp/src/telemetry/attributes.rs`.
- Operator dispositions are reviewed-manifest SQL runbooks (`ingress-authority.md` §"Repair for abandoned obligations (#1749)"), never kind predicates, always with `RETURNING` audit output.

## Retention policy (to be documented verbatim in the runbook)

| Row | Collectable when **all** hold |
|---|---|
| `extension_room_observation_work` final (`completed|terminal|stale`) | `settled_at_ms <= now - OBSERVER_HISTORY_RETENTION`; no `extension_room_publications` row with `work_id = id AND status = 'pending'`; no `ingress_messages` row with `message_key = work.message_key AND terminal_at IS NULL` (recovery may still rebuild the kind-28 effect for a non-terminal canonical row and must find its evidence) |
| `extension_room_publications` `published|stale` | `settled_at_ms <= cutoff` (the publication's own settlement), independent of its work row; `pending` is never collected |
| `extension_room_observation_receipts` | `recorded_at_ms <= cutoff`; no active (`pending|leased|started`) work for the same `(plugin_id, generation, room_jid, message_key)`; no non-terminal `ingress_messages` row for `message_key` |
| `extension_room_sources` + its `extension_room_source_revisions` | **Only retracted sources** (`retracted = 1`): `captured_at_ms <= cutoff`; no work row (any status) and no publication row (any status) references `source_key`. Revisions are deleted first, in budgeted batches; the source row is deleted only when, under the source lock, zero revisions remain and the predicates still hold **[R1-4]**. **Non-retracted source identity is never collected by this GC** **[R1-2]**: room correction validation (`groupchat_validation.rs:96,183`) accepts a correction of any archived message by its continuously-joined sender with no age cutoff, and capture resolves the target through `extension_room_source_revisions` (`sources.rs:465`), so a live subscription needs the identity for as long as the room archive holds the message. Source identity lifetime is therefore the archive's lifetime (the archive has no retention today); `source_json` stays with it. Bounding live source identity is explicitly out of scope and recorded as a follow-up tied to any future MAM retention. |
| `extension_room_observers` | never collected (configuration; generation revocation is the existing procedure) |

`OBSERVER_HISTORY_RETENTION = Duration::days(8)` (= `ALIAS_RETENTION`; one constant in `ingress_uow/room_observation/retention.rs`, documented as "same horizon as canonical retention, measured from the observer row's own settlement"). The canonical-row predicate, not the horizon alone, is what preserves duplicate suppression across the replay horizon: a receipt outlives every non-terminal canonical row regardless of age. What this GC bounds is **execution history** (work, publications, receipts) and retracted sources; a correction or retraction of a non-retracted source keeps resolving for as long as the archive holds the message.

## Migration V1023 (`db/migrations/waddle.rs`, SQLite + PostgreSQL)

- `CREATE TABLE IF NOT EXISTS` for `extension_room_publications`, `extension_room_observation_receipts`, `extension_room_sources`, `extension_room_source_revisions` with the exact startup DDL (the way V1022 did for `work`), so the `ALTER`s below are valid on a fresh database. Startup DDL in `schema.rs` is updated to include the new columns for the `IF NOT EXISTS` create path and must stay idempotent against the migrated shape.
- `ALTER TABLE extension_room_observation_work ADD COLUMN settled_at_ms BIGINT NULL`;
  `ALTER TABLE extension_room_publications ADD COLUMN settled_at_ms BIGINT NULL`;
  `ALTER TABLE extension_room_observation_receipts ADD COLUMN recorded_at_ms BIGINT NOT NULL DEFAULT 0`;
  `ALTER TABLE extension_room_sources ADD COLUMN captured_at_ms BIGINT NOT NULL DEFAULT 0`.
- Backfill at migration time (`now_ms` = migration time, so pre-existing history ages out 8 days after the upgrade, never immediately): work rows with final status → `settled_at_ms = now`; publications `published|stale` → `settled_at_ms = now`; receipts → `recorded_at_ms = now`; sources → `captured_at_ms = now`.
- Indexes: `extension_room_observation_work_settled ON (status, settled_at_ms)`, `extension_room_publications_settled ON (status, settled_at_ms)`, `extension_room_observation_receipts_recorded ON (recorded_at_ms)`, `extension_room_sources_captured ON (captured_at_ms)`.
- PG: `GRANT SELECT` on **all six** observer tables to `pg_monitor` (follows V1021's pattern; `extension_room_observers` and `extension_room_observation_work` have no grant today, `waddle.rs:1191`, `schema.rs:21`) so every inspection query in the runbook works under the monitoring role **[R1-5]**. The PostgreSQL migration test executes the runbook's inspection statements under `SET ROLE pg_monitor` and asserts they succeed. No epoch-guard triggers (the tables are not in `EPOCH_GUARDED_TABLES`; keep it that way, document why: they are not canonical authority).
- Update the hard-coded version lists/max in `db/migrations/tests.rs` (lines ~42, 159, 180, 251, 315, 475, 786, 958). V1022's constants and checksum are untouched.

## Writers (same transaction as the state change)

- `work.rs::finish`: set `settled_at_ms = now_ms` whenever the new status is not `pending`; `claim`'s `stale/source_changed` and `terminal/retry_exhausted` transitions set it too.
- `sources.rs::stale_source_work` and `stale_generation`: set `settled_at_ms` on the work and publication rows they stale.
- `publications.rs::mark_published` and the `stale` mark inside `publication()`: set `settled_at_ms`.
- `sources.rs::terminal_receipt`: `recorded_at_ms = now_ms`. `capture`: `captured_at_ms`.
- `now_ms` comes from the existing caller-supplied clock parameter; no new `Utc::now()` inside the repository.

## GC phase (`ingress/maintenance.rs` + new `ingress_uow/room_observation/retention.rs`)

- New `IngressMaintenancePhase::ObserverRetention` (sealed enum in `attributes.rs`, added to `ALL` so it is zero-registered) recorded after `RetentionGc`, combined into the pass outcome with the existing precedence.
- `RoomObservationRepository::collect_expired(tx, now_ms, limit) -> Result<ObserverRetentionBatch, ObservationError>` runs, in one transaction per batch, the deletes above in this order: publications → work → receipts → retracted-source revisions → retracted sources. The budget `GC_BATCH_LIMIT = 256` bounds **physical row deletions across all five statements** (each `DELETE ... WHERE pk IN (SELECT ... LIMIT remaining)` consumes the remaining budget), so a retracted source with thousands of revision mappings is drained over several batches and its source row goes only once its revision count is zero under lock **[R1-4]**; `ObserverRetentionBatch { publications, work, receipts, revisions, sources }` reports the counts and `exhausted = total == limit`. Candidate selection is `ORDER BY <ts>, <pk> LIMIT ?` with the protective `NOT EXISTS` predicates in the same statement, PG `FOR UPDATE SKIP LOCKED` on the candidate rows (reuse the `locked()` helper pattern), SQLite `begin_immediate`. The driver loop in maintenance uses `budget.retention` (cooperative 2 s / hard 6 s / 100 ms lock / 250 ms statement timeouts via `install_gc_timeouts`), stops when a batch is short, and returns `Partial` when the deadline cuts it. Attestation already gated the pass; the observer tables are not epoch-guarded so no epoch lock is taken.
- Telemetry: `ingress.maintenance.reclaimed_observer_rows{table}` (unit `{row}`) with a new sealed attribute enum `ObserverHistoryTable { Work, Publication, Receipt, Source }`, zero-registered for every label at startup; failures ride `ingress.maintenance.runs{phase=observer_retention,outcome}`.
- `trigger_maintenance` and the periodic tick already drive the pass; no new scheduler.

## Operator disposition (runbook, no code path that resets work) — revised **[R1-1][R1-3]**

- New section in `ingress-authority.md`: "Disposition for unsupported observer work (#1901)". Reviewed-manifest pattern copied from #1749 (`ingress-authority.md:1452-1624`), with two reviewed classes:
  - **Class A, unsupported work**: `pending` work whose `(plugin_id, generation)` is no longer the configured generation in `extension_room_observers`, or whose room the operator has established as permanently unrestorable.
  - **Class B, stranded publications**: `pending` publications for a room the operator has established as permanently lost, **independently of work status and source existence** (successful settlement creates `completed` work plus `pending` publications atomically, `work.rs:345`, and `process_room` returns before publishing when the room cannot be restored, `actor.rs:109`, so this case is otherwise invisible).
- Procedure: (1) `REPEATABLE READ READ ONLY` dry run that produces the manifest of work ids, publication ids and the exact pending ingress obligation pairs `(message_key, kind = 28, semantic_identity_hash)` from `ingress_effect_intents` lacking a receipt, one per Class A row; (2) a write transaction as the application role, temp manifest tables `ON COMMIT DROP`, `FOR UPDATE` in key order on `extension_room_observation_work`, `extension_room_publications` and `ingress_messages`, epoch proof exactly as #1749; a `DO $$` block that raises if any manifest work row is `leased`/`started` with an unexpired lease (uncertain work is never reset), if `attempt` or `status` changed since the dry run, if the plugin generation became configured again, if a manifest publication is no longer `pending`, or if the set of pending ingress pairs differs in either direction; (3) writes, each `RETURNING *`:
  `UPDATE extension_room_observation_work SET status='terminal', terminal_category='operator_unsupported', body='', lease_id=NULL, lease_until_ms=NULL, settled_at_ms=<now>`;
  `UPDATE extension_room_publications SET status='stale', settled_at_ms=<now>` (Class A dependants and Class B rows; the completed callback outcome on the work row is preserved);
  `INSERT INTO extension_room_observation_receipts (..., category='operator_unsupported', recorded_at_ms)` `ON CONFLICT DO NOTHING`;
  **and** `INSERT INTO ingress_effect_receipts` for the exact reviewed `(message_key, 28, hash)` pairs — the same abandonment receipt #1749 writes, because maintenance holds the canonical row non-terminal until that exact receipt exists (`ingress_substrate/maintenance.rs:113`) and terminal work is no longer claimable (`work.rs:75`); without it the canonical row, and through the GC guard the work/receipt rows, would be pinned forever. The runbook states that this receipt records **abandonment, not callback success**, mirrors what normal terminal settlement writes (`sources.rs:223`), and that terminalization then settles the canonical row on the next pass, after which observer retention collects the history at the horizon.
- `operator_unsupported` is added to the typed terminal/receipt category set in `work.rs` so readers parse it; the GC treats it like any terminal row.
- Test (SQLite + PostgreSQL): disposition SQL applied by the test → maintenance pass terminalizes the canonical row → clock past horizon → retention collects the work, receipt and stale publication; the ingress receipt row is the abandonment receipt, and no `completed` category is fabricated.

## Docs

- `ingress-authority.md`: new §"Observer history retention" (horizon table above, the distinction from the `ingress_send_attempts` cascade: *send attempts die with the canonical row via FK; observer history has no FK and dies by its own clock plus the canonical non-terminal guard*), backlog inspection SQL (`SELECT status, terminal_category, count(*) ... GROUP BY`, oldest `settled_at_ms` per status, pending publications with missing sources, rows protected only by a non-terminal canonical row), the disposition section, and the V1023 note. Update the #1901 pointer paragraph and the metrics table.
- `docs/extensions/room-observations.md`: one paragraph on retention.

## Tests (TDD, seams = `RoomObservationRepository` public methods + `run_maintenance_pass` + migration runner; SQLite and PostgreSQL pairs via `IngressFixture`, PG halves run with `WADDLE_TEST_POSTGRES_URL`)

New module `ingress_uow/room_observation/tests/retention.rs` (covered by the nextest `postgres` group through `test(ingress_uow::)`):

1. `active_work_is_never_collected`: `pending`, `leased`, `started` rows (lease expired or not) older than the horizon survive repeated `collect_expired`; their sources and receipts survive.
2. `pending_publication_protects_work_and_source`: completed work with a `pending` publication older than the horizon survives; after `mark_published` + clock advance beyond the horizon, work, publication and (once unreferenced) the source are collected.
3. `nonterminal_canonical_row_protects_receipts_and_work`: terminal work whose `ingress_messages` row is non-terminal survives past the horizon; after `terminalize` it is collected on the next pass.
4. `retention_expiry_collects_completed_terminal_and_stale_history`: rows settled before the cutoff are collected, rows settled after it survive; counts per table match the returned batch and the counter samples (`telemetry::test_support`).
5. `repeat_collection_is_idempotent_and_bounded`: 300 expired rows → first pass with `limit = 256` reports `Partial`/short batch semantics correctly, second pass collects the rest, third pass is a no-op with zero deletes.
6. `retracted_source_with_oversized_revision_chain_drains_within_budget`: one retracted, unreferenced source with more than 256 revision mappings; each pass deletes at most the budget, the source row survives until the last revision is gone, a concurrent correction/retraction that re-references the source mid-drain stops the deletion under lock **[R1-4]**.
6b. `aged_nonretracted_source_still_resolves_a_valid_correction`: after GC passes beyond the horizon, a correction to the retained target under the unchanged subscription still creates work (no `unknown_correction_target`) **[R1-2]**.
6c. `operator_disposition_terminalizes_and_then_ages_out` per the disposition section **[R1-1][R1-3]**, including a Class B stranded publication whose work is `completed` and whose source exists.
6d. PostgreSQL: the runbook inspection SQL runs under `SET ROLE pg_monitor` **[R1-5]**.
7. Migration: `db/migrations/tests.rs` upgrade test from a V1022 database with pre-existing rows asserts the backfilled timestamps equal migration time and that no row is collectable immediately after upgrade.
8. Existing suites (`room_observation/tests.rs`, `invocation_fence.rs`, `execute_observer_*`, `gc_send_attempt_tests`) stay green.

## Verification

`cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` and with default features; `cargo nextest run -p waddle-server` filtered to `ingress_uow::`, `ingress::`, `db::` with `WADDLE_TEST_POSTGRES_URL` set; `gc_monitoring_predicate_matches_collector` unchanged; full workspace nextest once at the end.
