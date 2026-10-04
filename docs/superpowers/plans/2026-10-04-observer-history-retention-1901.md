# Bounded retention for terminal room-observer history (#1901) Implementation Plan

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
| `extension_room_sources` + its `extension_room_source_revisions` | `captured_at_ms <= cutoff`; no work row (any status) and no publication row (any status) references `source_key`; the source is not referenced by a newer source revision chain still retained (revisions of one source share `source_key`, so delete them together) |
| `extension_room_observers` | never collected (configuration; generation revocation is the existing procedure) |

`OBSERVER_HISTORY_RETENTION = Duration::days(8)` (= `ALIAS_RETENTION`; one constant in `ingress_uow/room_observation/retention.rs`, documented as "same horizon as canonical retention, measured from the observer row's own settlement"). The canonical-row predicate, not the horizon alone, is what preserves duplicate suppression across the replay horizon: a receipt outlives every non-terminal canonical row regardless of age. Sources older than the horizon with no referencing work lose correction/retraction targeting; the runbook states that a correction of a message older than 8 days is recorded as `unknown_correction_target` (already an existing receipt category).

## Migration V1023 (`db/migrations/waddle.rs`, SQLite + PostgreSQL)

- `CREATE TABLE IF NOT EXISTS` for `extension_room_publications`, `extension_room_observation_receipts`, `extension_room_sources`, `extension_room_source_revisions` with the exact startup DDL (the way V1022 did for `work`), so the `ALTER`s below are valid on a fresh database. Startup DDL in `schema.rs` is updated to include the new columns for the `IF NOT EXISTS` create path and must stay idempotent against the migrated shape.
- `ALTER TABLE extension_room_observation_work ADD COLUMN settled_at_ms BIGINT NULL`;
  `ALTER TABLE extension_room_publications ADD COLUMN settled_at_ms BIGINT NULL`;
  `ALTER TABLE extension_room_observation_receipts ADD COLUMN recorded_at_ms BIGINT NOT NULL DEFAULT 0`;
  `ALTER TABLE extension_room_sources ADD COLUMN captured_at_ms BIGINT NOT NULL DEFAULT 0`.
- Backfill at migration time (`now_ms` = migration time, so pre-existing history ages out 8 days after the upgrade, never immediately): work rows with final status → `settled_at_ms = now`; publications `published|stale` → `settled_at_ms = now`; receipts → `recorded_at_ms = now`; sources → `captured_at_ms = now`.
- Indexes: `extension_room_observation_work_settled ON (status, settled_at_ms)`, `extension_room_publications_settled ON (status, settled_at_ms)`, `extension_room_observation_receipts_recorded ON (recorded_at_ms)`, `extension_room_sources_captured ON (captured_at_ms)`.
- PG: `GRANT SELECT` on the four tables to `pg_monitor` (follows V1021's pattern) so the inspection SQL in the runbook works under the monitoring role. No epoch-guard triggers (the tables are not in `EPOCH_GUARDED_TABLES`; keep it that way, document why: they are not canonical authority).
- Update the hard-coded version lists/max in `db/migrations/tests.rs` (lines ~42, 159, 180, 251, 315, 475, 786, 958). V1022's constants and checksum are untouched.

## Writers (same transaction as the state change)

- `work.rs::finish`: set `settled_at_ms = now_ms` whenever the new status is not `pending`; `claim`'s `stale/source_changed` and `terminal/retry_exhausted` transitions set it too.
- `sources.rs::stale_source_work` and `stale_generation`: set `settled_at_ms` on the work and publication rows they stale.
- `publications.rs::mark_published` and the `stale` mark inside `publication()`: set `settled_at_ms`.
- `sources.rs::terminal_receipt`: `recorded_at_ms = now_ms`. `capture`: `captured_at_ms`.
- `now_ms` comes from the existing caller-supplied clock parameter; no new `Utc::now()` inside the repository.

## GC phase (`ingress/maintenance.rs` + new `ingress_uow/room_observation/retention.rs`)

- New `IngressMaintenancePhase::ObserverRetention` (sealed enum in `attributes.rs`, added to `ALL` so it is zero-registered) recorded after `RetentionGc`, combined into the pass outcome with the existing precedence.
- `RoomObservationRepository::collect_expired(tx, now_ms, limit) -> Result<ObserverRetentionBatch, ObservationError>` runs, in one transaction per batch of `GC_BATCH_LIMIT = 256` rows, the four deletes above in this order: publications → work → receipts → sources(+revisions). Candidate selection is `ORDER BY <ts>, <pk> LIMIT ?` with the protective `NOT EXISTS` predicates in the same statement, PG `FOR UPDATE SKIP LOCKED` on the candidate rows (reuse the `locked()` helper pattern), SQLite `begin_immediate`. The driver loop in maintenance uses `budget.retention` (cooperative 2 s / hard 6 s / 100 ms lock / 250 ms statement timeouts via `install_gc_timeouts`), stops when a batch is short, and returns `Partial` when the deadline cuts it. Attestation already gated the pass; the observer tables are not epoch-guarded so no epoch lock is taken.
- Telemetry: `ingress.maintenance.reclaimed_observer_rows{table}` (unit `{row}`) with a new sealed attribute enum `ObserverHistoryTable { Work, Publication, Receipt, Source }`, zero-registered for every label at startup; failures ride `ingress.maintenance.runs{phase=observer_retention,outcome}`.
- `trigger_maintenance` and the periodic tick already drive the pass; no new scheduler.

## Operator disposition (runbook, no code path that resets work)

- New section in `ingress-authority.md`: "Disposition for unsupported observer work (#1901)". Reviewed-manifest pattern copied from #1749: (1) `REPEATABLE READ READ ONLY` dry run listing `work.id` rows that are `pending` with no configured plugin generation (compare against `extension_room_observers`) or whose room cannot be restored, plus `pending` publications whose `extension_room_sources` row is missing or whose `work.status <> 'completed'`; (2) a write transaction as the application role, temp manifest tables `ON COMMIT DROP`, `FOR UPDATE` in key order, a `DO $$` block that raises if any manifest row is `leased`/`started` with an unexpired lease (uncertain work is never reset), if `attempt` changed, or if the plugin generation became configured; (3) `UPDATE extension_room_observation_work SET status='terminal', terminal_category='operator_unsupported', body='', lease_id=NULL, lease_until_ms=NULL, settled_at_ms=<now> ... RETURNING *`, `UPDATE extension_room_publications SET status='stale', settled_at_ms=<now> ... RETURNING *`, and `INSERT INTO extension_room_observation_receipts (..., category='operator_unsupported', recorded_at_ms) ... ON CONFLICT DO NOTHING RETURNING *`. No `ingress_effect_receipts` row is written (that would claim the effect executed); the canonical row stays subject to its own terminalization/recovery policy and the runbook says so. Receipts record **abandonment, not callback success**.
- `operator_unsupported` is added to the typed terminal/receipt category set in `work.rs` so readers parse it; the GC treats it like any terminal row.

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
6. `source_revision_chain_survives_until_every_revision_is_unreferenced`.
7. Migration: `db/migrations/tests.rs` upgrade test from a V1022 database with pre-existing rows asserts the backfilled timestamps equal migration time and that no row is collectable immediately after upgrade.
8. Existing suites (`room_observation/tests.rs`, `invocation_fence.rs`, `execute_observer_*`, `gc_send_attempt_tests`) stay green.

## Verification

`cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` and with default features; `cargo nextest run -p waddle-server` filtered to `ingress_uow::`, `ingress::`, `db::` with `WADDLE_TEST_POSTGRES_URL` set; `gc_monitoring_predicate_matches_collector` unchanged; full workspace nextest once at the end.
