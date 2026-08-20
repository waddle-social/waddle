# Ingress Shadow Soak Runbook

## Purpose & scope

The ingress shadow (#1656) runs the atomic ingress transaction against the
shadow tables only and records its decisions. It has no authority over
delivery or the XMPP stream-management `h` value. This soak gates the #1657
authority cutover: prove on production traffic that the shadow admits,
decides, retires, and reclaims its own state — and survives restarts — without
touching the authoritative delivery path.

## Prerequisites and fail-closed boot

Do not enable the flag unless the deployment uses PostgreSQL, the server image
has the `clustering` feature, clustering is enabled, node identity is
available, and `WADDLE_DEPLOYMENT_UUID` is set. The dedicated shadow pool must
also open successfully.

These are fail-closed startup prerequisites. With the flag enabled, a missing
prerequisite makes the server refuse to boot; it is not a partially enabled
shadow. Production satisfies them with PostgreSQL, clustering enabled, and the
deployment UUID configured.

## Activation

Set the HelmRelease ConfigMap-backed environment value:

```yaml
config:
  extraEnv:
    WADDLE_INGRESS_SHADOW_ENABLED: "true"
```

Keep the #1695 / #1657 soak-gate comment beside this value. Do not use
`containerExtraEnv`: release publishing rewrites it. The chart's
`checksum/config` annotation rolls the pods after the ConfigMap changes. Flux
observes its OCI source on its normal five-minute interval. The rollout path is
the server pipeline render from `server/env.cue`,
`cue vet -d '#PublishedValues'`, and `flux push` of
`gitops-waddle-server:latest`; Flux then applies the rendered HelmRelease.

This PR also adds `spec.monitoring.customQueriesConfigMap` to the `postgresql`
CNPG Cluster. CloudNativePG reloads monitoring configuration through the
instance manager without restarting PostgreSQL, but verify it on the live
operator before the window opens: after Flux applies the change,
`kubectl --context teleport.waddle.social-production -n waddle get cluster postgresql`
must still show both instances with their original start times
(`cnpg_pg_postmaster_start_time` unchanged) and no switchover event.

The deployment is a RollingUpdate with `maxSurge: 1` and `maxUnavailable: 0`.
Expect as many as three server pods during the rollout. The current baseline is
44 PostgreSQL backends of 100; allow four dedicated shadow-pool connections per
pod, for approximately 77 connections at the three-pod peak. Watch this during
the rollout; do not accept a rollout that approaches the connection limit.

## Day-0 checklist

Use the production context for every cluster operation; all commands below are
read-only except the rollout restart in the churn exercise.

1. Confirm the rendered ConfigMap contains
   `WADDLE_INGRESS_SHADOW_ENABLED: "true"`:

   ```sh
   kubectl --context teleport.waddle.social-production -n waddle get configmap waddle-server-config -o yaml | rg -C 3 'WADDLE_INGRESS_SHADOW_ENABLED'
   ```

2. Confirm both steady-state replicas report
   `ingress_shadow_enabled == 1` and neither reports a disabled skip:

   ```promql
   ingress_shadow_enabled
   ingress_shadow_skips_total{reason="disabled"}
   ```

   Require one `instance` series for each replica, value `1` for the first
   query and `0` for the second. Do not treat a transient surge pod as one of
   the two required steady-state replicas.

3. Confirm every new metric family is present before starting the window. Use
   these exact Prometheus names:

   ```text
   ingress_shadow_candidates_total
   ingress_shadow_decisions_total
   ingress_shadow_admissions_total
   ingress_shadow_completions_total
   ingress_shadow_alias_outcomes_total
   ingress_shadow_tx_retries_total
   ingress_shadow_skips_total
   ingress_shadow_enabled
   ingress_shadow_gc_runs_total
   ingress_shadow_gc_reclaimed_messages_total
   ingress_shadow_tx_duration_seconds_bucket
   ingress_shadow_tx_duration_seconds_sum
   ingress_shadow_tx_duration_seconds_count
   ingress_shadow_oldest_outstanding_submission_age_seconds
   ingress_shadow_aborted_total

   cnpg_waddle_ingress_table_total_bytes
   cnpg_waddle_ingress_table_live_tuples
   cnpg_waddle_ingress_table_dead_tuples
   cnpg_waddle_ingress_gc_eligible_messages
   cnpg_waddle_ingress_gc_oldest_eligible_age_seconds
   cnpg_waddle_ingress_gc_retained_referenced_messages
   cnpg_waddle_ingress_messages_count
   cnpg_waddle_ingress_cohort_count
   cnpg_waddle_ingress_streams_open_streams
   cnpg_backends_total
   cnpg_pg_settings_setting{name="max_connections"}
   cnpg_collector_pg_wal{value="size"}
   cnpg_collector_pg_wal{value="volume_size"}
   cnpg_collector_wal_bytes
   cnpg_last_error

   kubelet_volume_stats_used_bytes
   kubelet_volume_stats_capacity_bytes
   kubelet_volume_stats_available_bytes
   ```

   `ingress_shadow_tx_duration_seconds_bucket` is the repository's first
   classic-histogram consumer: confirm at least one `le`-labelled series is
   queryable (not only the `_sum`/`_count`), otherwise `IngressShadowTxSlow`
   evaluates over nothing.

   For the kubelet families, require the `namespace="waddle"` series for all
   four PVCs: `postgresql-1`, `postgresql-1-wal`, `postgresql-2`, and
   `postgresql-2-wal`. Inspect the two direct scrape sources read-only when a
   series is absent:

   ```sh
   kubectl --context teleport.waddle.social-production get --raw /api/v1/namespaces/waddle/pods/postgresql-1:9187/proxy/metrics
   node="$(kubectl --context teleport.waddle.social-production get nodes -o jsonpath='{.items[0].metadata.name}')"
   kubectl --context teleport.waddle.social-production get --raw "/api/v1/nodes/${node}/proxy/metrics"
   ```

4. Post the Day-0 baselines to [#1695](https://github.com/waddle-social/waddle/issues/1695): `rate(cnpg_collector_wal_bytes[10m])`,
   `B(T0)` where `B(T) = sum(cnpg_waddle_ingress_table_total_bytes)` evaluated
   at `T` (one scalar; also record the per-table values for context),
   PostgreSQL backend use, and `cnpg_waddle_ingress_streams_open_streams`. Record the
   dashboard link and the two replica instance names with the post.

## Observation window

The observation window is at least 10 consecutive days. Define `T0` as the
first scrape at which both replicas have `ingress_shadow_enabled == 1` and
`ingress_shadow_candidates_total{outcome="parked"} > 0`. Do not backdate T0.

## Checkpoints

Record these checkpoints in #1695:

| Checkpoint | Record |
| --- | --- |
| T0 | Both replica instances, enabled/candidate proof, Day-0 baselines, and alert state. |
| T0 + 1 day | Cohort count by state, `D1` table bytes, `U1` terminal-unreferenced cohort count, retries, decisions, and GC status. |
| T0 + 5 days | Table bytes, `G1`, backend/PVC/WAL levels, oldest outstanding age, and alert state. |
| T0 + 10 days | Every pass calculation, cohort states, reclaimed total, `G2`, logs, and final alert state. |

## Cohort procedure

On day 1, use a known resumable session to send at least five controlled
messages. Record every origin-id and the `stream_id`, then close the session so
the stream retires. In the day-1 follow-up commit, replace the sentinel
`'1970-01-01T00:00:00Z'` and `'1970-01-02T00:00:00Z'` (`COHORT_START` and
`COHORT_END`) literals in
`infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml`
with the cohort bounds. That ConfigMap hot-reloads in CNPG; do not restart
PostgreSQL. Record `D1` and `U1`, where `U1 >= 5` is the cohort's
terminal-unreferenced count at T0 + 1 day.

## Churn exercise

Run this no earlier than day 2, after explicit approval and a scheduled window.
Use a known resumable session, then restart the deployment:

```sh
kubectl --context teleport.waddle.social-production -n waddle rollout restart deployment/waddle-server
```

Pass only if all of the following are recorded:

- The server logs `SM resumed` with the same `stream_id` on a new pod.
- On that pod, `ingress_shadow_candidates_total{outcome="parked"}`,
  `ingress_shadow_admissions_total`, and `ingress_shadow_completions_total`
  increment after resume.
- There are zero handoff-refusal, shutdown-drain-exceeded, and
  orphan-retirement-recovery warnings.
- `ingress_shadow_skips_total{reason="closed"}` and
  `ingress_shadow_skips_total{reason="queue_full"}` remain zero.
- Record and explain `ingress_shadow_aborted_total`; it is expected only for
  this declared churn restart.
- `ingress_shadow_oldest_outstanding_submission_age_seconds` returns to zero.

## Pass criteria

Every rule in the `waddle-ingress-shadow` group of
`infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml` must stay
silent throughout the window — `IngressShadowNotEnabled`,
`IngressShadowDisabledReplica`, `IngressShadowClosedDrops`,
`IngressShadowSaturated`, `IngressShadowStalledSubmission`,
`IngressShadowCandidateStarvation`, `IngressShadowAliasConflict`,
`IngressShadowRetryExhausted`, `IngressShadowInfraDecisions`,
`IngressShadowGcFailing`, `IngressShadowGcBacklog`, `IngressShadowGcAge`,
`IngressShadowTxSlow`, `PostgresBackendsHigh`, `PostgresWalVolumeHigh`,
`PostgresWalRateHigh`, `PostgresCollectorError`, `PostgresDataPvcHigh`,
`PostgresWalPvcHigh`, `PostgresDataPvcSeriesMissing`,
`PostgresWalPvcSeriesMissing`, `IngressShadowSeriesMissing`,
`PostgresCnpgSeriesMissing`, `IngressShadowCnpgQueriesMissing` — with one
exception: `IngressShadowAborted`
may fire during the declared churn restart. Record that exception and its
count in #1695. Thresholds are final before activation; if one must change
mid-window, the window restarts.

| Area | Pass criterion |
| --- | --- |
| Completion | Restart-stable, over the whole window at T0 + 10d: `sum(increase(ingress_shadow_completions_total[10d])) / sum(increase(ingress_shadow_admissions_total[10d])) >= 0.999`, and the same ratio `by (instance)` is `>= 0.999` for every pod that existed in the window (`increase` is counter-reset safe; summing over `instance` absorbs the churn-restart pod replacement). |
| Candidate quality | Over the whole window at T0 + 10d, with `c(o) = sum(increase(ingress_shadow_candidates_total{outcome="o"}[10d]))`: `c(parked) / (c(parked) + c(no_claim_fence) + c(no_principal) + c(no_capture)) >= 0.99`, globally and `by (instance)` for every pod in the window. |
| Cohort | At T0 + 10 days all three `cnpg_waddle_ingress_cohort_count{state}` series are present (the query always emits explicit zeros) with `terminal_unreferenced == 0` and `live == 0`; `sum(cohort at T0 + 10d) <= sum(cohort at T0 + 1d) - U1`; absence of a cohort series is a failed check, never a pass. |
| Retained references | `cnpg_waddle_ingress_gc_retained_referenced_messages` (terminal rows past the cutoff kept alive by long-lived streams — GC rescans them on every run) at T0 + 10d is `<= 2 × (value at T0 + 5d) + 100`. |
| Instrumentation | Every series in Day-0 step 3 is still present at T0 + 10d; `IngressShadowSeriesMissing`, `PostgresCnpgSeriesMissing` and `IngressShadowCnpgQueriesMissing` never fired. |
| Reclamation | GC is event-driven (it runs after successful submissions and retirements), so the backlog criterion carries the same traffic qualification as `IngressShadowGcBacklog`: `cnpg_waddle_ingress_gc_eligible_messages` is never above zero for six hours **while** `sum(increase(ingress_shadow_admissions_total[6h])) > 0`; an idle backlog is bounded instead by `cnpg_waddle_ingress_gc_oldest_eligible_age_seconds < 777600` (9 days) throughout; `sum(increase(ingress_shadow_gc_reclaimed_messages_total[10d])) >= U1`. |
| Growth | With `B(T) = sum(cnpg_waddle_ingress_table_total_bytes)` at `T`: `G1 = B(T0+5d) - B(T0)`, `G2 = B(T0+10d) - B(T0+5d)`. Pass when `G2 <= 1.5 * G1 + 16 MiB` and `B(T0+10d) < 1 GiB`. |
| PostgreSQL | Backends stay below 80% of `max_connections`; data and WAL PVC use stay below 70% on both instances. |
| Submission latency | p99 `ingress_shadow_tx_duration_seconds` remains below 2 seconds. |

Known instrumentation limit: retention GC runs under the worker's 2.5 s
transaction deadline and commits per candidate, so a GC run that times out
has already reclaimed rows that `ingress_shadow_gc_reclaimed_messages_total`
does not count. `IngressShadowGcFailing` firing is therefore a soak finding
against #1656's GC budget (tracked as a follow-up issue), and the cohort
state metrics — not the reclaimed counter — are the primary reclamation
evidence.

## Loki checks

Run the following queries during each checkpoint and after the churn exercise:

```logql
{service_name="waddle-server"} |= "ingress shadow" | detected_level=~"warn|error"
{service_name="waddle-server"} |= "refusing resumable SM handoff with unfinished ingress shadow work"
{service_name="waddle-server"} |= "Graceful shutdown: ingress shadow drain exceeded the shutdown budget"
{service_name="waddle-server"} |= "ingress shadow orphan retirement recovery failed"
{service_name="waddle-server"} |= "SM resumed"
```

## Rollback

Delete `WADDLE_INGRESS_SHADOW_ENABLED` from `config.extraEnv` in the
HelmRelease and reconcile it. The ConfigMap checksum rolls the pods. This stops
shadow writes **and stops GC**. Data stays in the ingress tables; rollback does
not remove it.

## Retained-state cleanup

Leave state in place when the shadow may be re-enabled or it is needed to
investigate a failure. Use manual cleanup only at epoch 0, after deciding the
state is no longer needed and with writes stopped. Delete in bounded batches,
using this lock order:

```text
epoch row FOR SHARE
ingress_effect_intents
ingress_deliveries
ingress_sm_refs
ingress_origin_aliases
ingress_messages
ingress_sm_streams
```

Never use `TRUNCATE`; guarded ingress tables reject it at every epoch. Row-wise
`DELETE` is allowed at epoch 0 and preserves the required lock ordering. Select
each batch exactly once (a temp table, keyed with a deterministic tiebreak) so
every child delete sees the same key set — only `ingress_effect_intents`
cascades; the other child tables have plain foreign keys and would abort the
transaction on a divergent batch. Repeat until the batch is empty:

```sql
BEGIN;
SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR SHARE; -- must be 0
CREATE TEMP TABLE batch ON COMMIT DROP AS
  SELECT message_key FROM ingress_messages
  ORDER BY created_at, message_key LIMIT 1000;
DELETE FROM ingress_effect_intents  WHERE message_key IN (SELECT message_key FROM batch);
DELETE FROM ingress_deliveries      WHERE message_key IN (SELECT message_key FROM batch);
DELETE FROM ingress_sm_refs         WHERE message_key IN (SELECT message_key FROM batch);
DELETE FROM ingress_origin_aliases  WHERE message_key IN (SELECT message_key FROM batch);
DELETE FROM ingress_messages        WHERE message_key IN (SELECT message_key FROM batch);
COMMIT;
```

After the message tables are empty, delete `ingress_sm_streams` in the same
epoch-locked, batched shape:

```sql
BEGIN;
SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR SHARE; -- must be 0
CREATE TEMP TABLE stream_batch ON COMMIT DROP AS
  SELECT sm_ingress_id FROM ingress_sm_streams
  ORDER BY created_at, sm_ingress_id LIMIT 1000;
DELETE FROM ingress_sm_streams
  WHERE sm_ingress_id IN (SELECT sm_ingress_id FROM stream_batch);
COMMIT;
```

`DELETE`
only marks tuples dead: run `VACUUM (ANALYZE)` on the six tables afterwards,
and if the goal was to give space back to the data PVC, schedule
`VACUUM FULL` (or `pg_repack`) with the shadow disabled — it takes an
`ACCESS EXCLUSIVE` lock on each table.

## Outcome

On pass, comment the evidence and calculations on #1695 and unblock #1657. On
failure, file it against the relevant #1656 scope with the checkpoint, metrics,
and logs; #1657 stays blocked.

## Scaffolding removal

#1657 deletes, together with the shadow itself: the shadow-scoped server
instruments (`ingress.shadow.candidates`, `admissions`, `completions`,
`aborted`, `enabled`, `oldest_outstanding_submission_age`, `gc.runs`,
`gc.reclaimed_messages`, `tx.duration`), every `IngressShadow*` alert rule,
the "Ingress shadow soak" dashboard row, and this runbook. The CNPG and
kubelet scrapes, the `postgresql-monitoring-ingress` custom queries, and the
`Postgres*` rules stay; they are production PostgreSQL observability.
