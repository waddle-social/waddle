# Ingress authority

[RFC 0018](../rfcs/0018-ingress-authority-cutover.md) defines the transaction
that decides responsibility for inbound messages. Planning captures effects
without writes; the transaction commits canonical identity, durable effects,
payload-complete intents, envelope, receipts, and resumable-stream references
and checkpoint together. Only a committed decision advances XEP-0198 `h`.
Post-commit effects do not determine acceptance. Cluster-global
(sender bare JID, target, origin-id) aliases decide duplicates and repair
recorded durable effects; MAM no longer makes an independent dedupe decision.

## Decision classes

The `class` label uses snake_case names of the RFC §3.5 decisions:

- Advancing acceptance/replay: `accepted`, `existing_committed`,
  `existing_consistent`, `existing_repaired`, `existing_divergent`,
  `owner_first_acceptance`, `owner_duplicate`.
- Advancing semantic refusals: `alias_conflict`, `semantic_malformed`,
  `authorization_denied`, `policy_denied`, `capture_overflow`. The committed
  rejection records the standard stanza error; it still advances `h`.
- Non-advancing refusals: `principal_missing`, `claim_fence_missing`,
  `room_generation_stale`, `frontier_stale`, `sm_ordinal_conflict`,
  `intent_contradiction`, `storage`, `serialization_exhaustion`, `timeout`
  (before commit), `ambiguous_commit`, `lineage`, `epoch_unsupported`.

A non-advancing resumable message leaves a hole and ends the transport; resume
starts before the hole. An ephemeral stream receives a typed stream error
and closes. A timeout after commit never reverses the handled disposition.
An ambiguous commit must not be treated as proof of rollback: a retry can
recover the recorded wire-position binding as `existing_committed`.

## Metrics and alerts

OTLP counters translate to the following Prometheus families. Transaction
latency is a seconds histogram; confirm `le`-labelled buckets are present.

| Instrument | Prometheus family | Alert(s) |
| --- | --- | --- |
| `ingress.decisions{class}` | `ingress_decisions_total` | `IngressInfraDecisions`, `IngressFenceDecisions`, `IngressIdentityDecisions`, `IngressRetryExhausted`, `IngressSeriesMissing` |
| `ingress.alias.outcomes{outcome}` | `ingress_alias_outcomes_total` | `IngressAliasConflicts` |
| `ingress.tx.retries` | `ingress_tx_retries_total` | Retry pressure context |
| `ingress.gc.runs{outcome}` | `ingress_gc_runs_total` | `IngressGcFailing` |
| `ingress.gc.reclaimed_messages` | `ingress_gc_reclaimed_messages_total` | Reclamation progress |
| `ingress.tx.duration` | `ingress_tx_duration_seconds_bucket` (also `_sum`, `_count`) | `IngressTxSlow` |
| `ingress.effects.unresolved{kind}` | `ingress_effects_unresolved_total` | `IngressUnresolvedEffectsGrowing` |

`IngressInfraDecisions` is critical: a positive rate of storage, serialization
exhaustion, timeout or ambiguous-commit decisions for 10m means messages are
being refused. `IngressIdentityDecisions` pages on any `intent_contradiction`,
`lineage` or `epoch_unsupported` occurrence in the last hour; check durable
identity consistency and the deployed binary/ledger lineage.
`IngressFenceDecisions` covers `principal_missing`, `claim_fence_missing`,
`room_generation_stale`, `frontier_stale` and `sm_ordinal_conflict`.
Fence refusals sustained for 10m, alias conflicts, exhausted
retries and failed, timed-out or unattested GC runs are warnings (an
`unattested` run was skipped because lineage attestation failed; nothing
was collected). `IngressTxSlow` warns on
p99 above 2s. The CNPG-query-based `IngressGcBacklog` warns when eligible rows
persist for 6h; `IngressGcAge` warns above 9 days.
`IngressSeriesMissing` warns when all decision series disappear, or a live
`waddle-server` instance has none, for 15m. These counters are zero-registered
at startup, so absence indicates missing telemetry even on idle pods.
`IngressCnpgQueriesMissing` warns if either eligibility or age series is
absent for 15m, so missing custom queries cannot masquerade as healthy GC.
`IngressUnresolvedEffectsGrowing` warns on a positive counter increase over
1h, grouped by kind: this is observed unresolved work, not a current queue gauge.

The dedicated ingress authority pool defaults to 4 connections per pod;
`WADDLE_INGRESS_DB_POOL_SIZE` overrides it. Transactions and retries are
bounded (the soak measured 23 ms p99 transaction time). The connection
budget is approximately 77 at a 3-pod rollout peak against 100 PostgreSQL
connections, below the 80% alert threshold; re-derive it before raising
the pool override.

## Retention and unresolved effects

GC retains canonical messages for eight days from `terminal_at`, and keeps
rows with live stream references. Intents without matching receipts prevent
terminalization, so unresolved effects protect the message from GC. #1658
adds the recovery executor; #1657 durably records unfinished effects but does
not replay them automatically. Never delete protected rows to silence alerts.
Watch table bytes/live/dead tuples including `ingress_effect_receipts`, and
CNPG eligible/retained-reference counts alongside reclamation totals.

GC takes the epoch lock before canonical rows and uses `FOR UPDATE SKIP LOCKED`.
A `partial` result means bounded progress with more work pending, not failure;
`failed` or `timed_out` requires investigation. See
[ingress epoch guards](ingress-epoch-guards.md) for lock order and activation
preconditions; the cutover does not itself authorize an epoch activation.

## One-shot Recreate cutover

The production HelmRelease uses `Recreate` for #1657: all old writers stop
before ledger V1012 resets ingress state. Old binaries refuse the new ledger
version. Expect a brief full outage and client reconnects; preserved SM
continuity across the reset must not be assumed. Do not perform ledger surgery
or roll back to an old image. Verify the deployed digest on every replica,
readiness, ledger migration completion and the queries below. Then open a
follow-up PR restoring `RollingUpdate` (`maxSurge: 1`, `maxUnavailable: 0`).
This follows #1596 (`1cad23a2`) and its verified flip-back #1605 (`5dbe771c`);
Recreate is not the steady-state rollout strategy.

## Read-only verification

Use the production context explicitly. Inspect rollout strategy, actual
images, readiness, and logs (do not trigger a restart):

```sh
kubectl --context teleport.waddle.social-production -n waddle get helmrelease waddle-server -o yaml
kubectl --context teleport.waddle.social-production -n waddle get deployment waddle-server -o yaml
kubectl --context teleport.waddle.social-production -n waddle get pods -o wide
kubectl --context teleport.waddle.social-production -n waddle logs deployment/waddle-server --all-pods=true --since=30m
kubectl --context teleport.waddle.social-production -n waddle get cluster postgresql -o yaml
```

In Grafana Explore, verify decisions, unresolved kinds, histogram buckets and
GC; absent metrics are not evidence of healthy zero activity:

```promql
sum by (class) (rate(ingress_decisions_total[10m]))
sum by (kind) (increase(ingress_effects_unresolved_total[1h]))
histogram_quantile(0.99, sum by (le) (rate(ingress_tx_duration_seconds_bucket[10m])))
sum by (outcome) (increase(ingress_gc_runs_total[1h]))
max(cnpg_waddle_ingress_gc_eligible_messages)
max(cnpg_waddle_ingress_gc_oldest_eligible_age_seconds)
```

In `psql` connected to the application database with a read-only role, inspect
frontiers and compare retained reference ordinals in one consistent snapshot:

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SELECT handled_ordinal, checkpoint_h FROM ingress_sm_streams LIMIT 20;
SELECT s.sm_ingress_id, s.handled_ordinal, s.checkpoint_h,
       count(r.ingress_ordinal) AS ref_count,
       coalesce(max(r.ingress_ordinal), 0) AS last_ref_ordinal
FROM ingress_sm_streams s
LEFT JOIN ingress_sm_refs r USING (sm_ingress_id)
GROUP BY s.sm_ingress_id, s.handled_ordinal, s.checkpoint_h
HAVING s.handled_ordinal <> coalesce(max(r.ingress_ordinal), 0)
    OR s.handled_ordinal <> count(r.ingress_ordinal);
COMMIT;
```

The mismatch query should return no rows for active retained streams.
`checkpoint_h` is the contiguous wire handled count (including other stanza
types, with u32 wrap), not the message ordinal: do not compare them numerically.
Refs retain wire-position bindings until stream retirement; investigate any
mismatch against the RFC before changing data.
