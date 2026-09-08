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
| `ingress.effects.unresolved{kind}` (local executions only) | `ingress_effects_unresolved_total` | `IngressUnresolvedEffectsGrowing` |
| CNPG old non-terminal canonical messages by pending intent family | `cnpg_waddle_ingress_nonterminal_messages{kind}` | `IngressNonTerminalBacklog` |
| CNPG oldest non-terminal message older than 10m (seconds; zero when empty) | `cnpg_waddle_ingress_nonterminal_age_oldest_seconds` | `IngressCnpgQueriesMissing` |
| CNPG GC eligibility and oldest eligible age | `cnpg_waddle_ingress_gc_eligible_messages`, `cnpg_waddle_ingress_gc_oldest_eligible_age_seconds` | `IngressGcBacklog`, `IngressGcAge`, `IngressCnpgQueriesMissing` |

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
`IngressCnpgQueriesMissing` warns if GC eligibility, GC age, or
`cnpg_waddle_ingress_nonterminal_age_oldest_seconds` is absent for 15m. The
non-terminal age query always returns one row, including zero for no backlog,
so a missing query cannot masquerade as healthy receipt completeness.
`IngressUnresolvedEffectsGrowing` warns on a positive counter increase over
1h, grouped by kind. It sees only locally executed effects, not obligations
that were recorded but never executed; it is not a current queue gauge. Use
`IngressNonTerminalBacklog` for the canonical row-level view.

`IngressNonTerminalBacklog` warns when any canonical row older than 10 minutes
remains non-terminal, sustained for another 10m (roughly 20m after creation,
plus scrape/evaluation delay). Its `kind` is the unreceipted intent family;
`terminalization` means all receipts exist but terminalization itself is
missing. A row counts once per pending family, even with several intents in
that family, so summing families can count one row more than once. The 10m
age threshold is a generous multiple of the 5s Phase C budget: investigate a
receipt-completeness or terminalization regression (#1749). GC cannot reclaim
these rows. Both CNPG queries run only on the primary.

The dedicated ingress authority pool defaults to 4 connections per pod;
`WADDLE_INGRESS_DB_POOL_SIZE` overrides it. Transactions and retries are
bounded (the soak measured 23 ms p99 transaction time). The connection
budget is approximately 77 at a 3-pod rollout peak against 100 PostgreSQL
connections, below the 80% alert threshold; re-derive it before raising
the pool override.

## Retention and unresolved effects

GC retains canonical messages for eight days from `terminal_at`, and keeps
rows with live stream references. Intents without matching receipts prevent
terminalization. Reconciliation that adds omitted intents clears `terminal_at`
in the same transaction. GC also checks receipt completeness while holding the
canonical-row lock, so unresolved effects protect a message even when its
terminal timestamp is stale. #1658 adds the recovery executor; #1657 durably
records unfinished effects but does not replay them automatically. Never delete protected rows to silence alerts.
Watch table bytes/live/dead tuples including `ingress_effect_receipts`, and
CNPG eligible/retained-reference counts alongside reclamation totals.

The collector and CNPG backlog/oldest-age queries share this eligibility
predicate: `terminal_at IS NOT NULL AND terminal_at <= now() - interval '8 days'
AND receipts_complete AND (has_alias OR has_delivery OR NOT has_ref)`, where
the three reference booleans mean a matching row exists in
`ingress_origin_aliases`, `ingress_deliveries`, and `ingress_sm_refs`, respectively. `receipts_complete` means every recorded
intent has a receipt with the same message key, kind, and semantic identity
hash. Expired delivery markers are GC work only when receipts are complete,
including when no alias or stream reference remains. GC removes aliases and
delivery markers even when a stream reference retains the canonical row.
The retained-reference gauge counts expired rows with `has_ref OR NOT
receipts_complete`; this includes stale terminal rows with pending intents.
Receipt-complete rows with stream references can also be eligible until their
aliases and delivery markers are removed; the stream reference retains the
canonical row itself. Eligible age is measured
from `terminal_at`, not from expiry. The Rust predicate parity test pins both
collector dialects and both CNPG eligibility aggregates to this definition.

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
max by (kind) (cnpg_waddle_ingress_nonterminal_messages)
max(cnpg_waddle_ingress_nonterminal_age_oldest_seconds)
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


## Non-terminal backlog triage

In `psql` on the primary, use a read-only role (the CNPG queries also run
under `pg_monitor`) and a consistent snapshot. Empty `pending_pairs` denotes
terminalization-only work; every old non-terminal row appears.

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
WITH pending AS (
  SELECT intent.message_key, intent.kind, intent.semantic_identity_hash,
         CASE intent.kind
           WHEN 0 THEN 'archive'
           WHEN 1 THEN 'route_direct'
           WHEN 2 THEN 'route_muc'
           WHEN 3 THEN 'route_occupant_pm'
           WHEN 4 THEN 'recipient_sm_append'
           WHEN 5 THEN 'carbons'
           WHEN 6 THEN 'inbox_project'
           WHEN 7 THEN 'notification_activity_preview'
           WHEN 8 THEN 'call_signal'
           WHEN 9 THEN 'pin'
           WHEN 10 THEN 'extension'
           WHEN 11 THEN 'error_reply'
           WHEN 12 THEN 'dispatch_to_room_remote'
           WHEN 13 THEN 'room_subject_mutation'
           WHEN 14 THEN 'retraction_tombstone'
           WHEN 15 THEN 'dm_pin_mutation'
           WHEN 16 THEN 'group_dm_membership_grant'
           WHEN 17 THEN 'group_dm_invite_ledger'
           WHEN 18 THEN 'link_preview_media_ref'
           WHEN 19 THEN 'muc_invite_membership_grant'
           WHEN 20 THEN 'muc_invite_ledger'
           WHEN 21 THEN 'groupchat_notification_recovery'
           WHEN 22 THEN 'pending_delivery'
           WHEN 23 THEN 'tombstone_replay_deletion'
           WHEN 24 THEN 'relay_carbons'
           WHEN 25 THEN 'room_observer'
           WHEN 26 THEN 'dm_call_thread_state'
           ELSE 'kind_' || intent.kind::text
         END AS kind_family
  FROM ingress_effect_intents intent
  WHERE NOT EXISTS (
    SELECT 1 FROM ingress_effect_receipts receipt
    WHERE receipt.message_key = intent.message_key
      AND receipt.kind = intent.kind
      AND receipt.semantic_identity_hash = intent.semantic_identity_hash
  )
)
SELECT message.message_key, message.created_at,
       coalesce(jsonb_agg(jsonb_build_object(
         'kind', pending.kind,
         'semantic_identity_hash', encode(pending.semantic_identity_hash, 'hex'),
         'kind_family', pending.kind_family
       ) ORDER BY pending.kind, pending.semantic_identity_hash)
         FILTER (WHERE pending.kind IS NOT NULL), '[]'::jsonb) AS pending_pairs,
       CASE WHEN count(pending.kind) = 0 THEN 'terminalization' END AS missing_step
FROM ingress_messages message
LEFT JOIN pending USING (message_key)
WHERE message.terminal_at IS NULL
  AND message.created_at <= now() - interval '10 minutes'
GROUP BY message.message_key, message.created_at
ORDER BY message.created_at, message.message_key;
COMMIT;
```

## Repair for abandoned obligations (#1749)

The pre-fix rows hold obligations that can never execute: the remote occupant
copy was never sent, and #1658's recovery executor does not exist. Stale
activity mutations must not be replayed. These repair receipts record
**abandonment, not delivery**. Only archived content (message bodies in the
room MAM archive, XEP-0313) is recoverable by affected occupants; bodyless chat
states and markers were never archived. Running this repair is the operator's
decision, after reviewing the affected messages and accepting that loss.

The incident window starts at the #1657 cutover, `2026-09-08T11:02Z`, and ends
only when **both replicas run the fixed image**. Record the actual rollout
completion timestamp: during the mixed `deliver_ordered.v8`/`v9` rollout,
an `UnsupportedEnvelope` NACK also leaves rows non-terminal. Do not use the
first upgraded replica's start time as the end of the window.

First run this read-only dry run, replacing the rollout timestamp placeholder.
Keep its output as the reviewed manifest: every selected canonical key,
including keys with no pending pairs, and the exact pending integer-kind and
hex-hash pairs. The family labels reuse the monitoring query's mapping.
Kind 2 also includes `RouteMucSystemBroadcast`, and kind 7 includes activity
outside rooms. Never choose repair targets with a kind predicate; select an
explicit reviewed `message_key` manifest instead.

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
WITH pending AS (
  SELECT intent.message_key, intent.kind, intent.semantic_identity_hash,
         CASE intent.kind
           WHEN 0 THEN 'archive'
           WHEN 1 THEN 'route_direct'
           WHEN 2 THEN 'route_muc'
           WHEN 3 THEN 'route_occupant_pm'
           WHEN 4 THEN 'recipient_sm_append'
           WHEN 5 THEN 'carbons'
           WHEN 6 THEN 'inbox_project'
           WHEN 7 THEN 'notification_activity_preview'
           WHEN 8 THEN 'call_signal'
           WHEN 9 THEN 'pin'
           WHEN 10 THEN 'extension'
           WHEN 11 THEN 'error_reply'
           WHEN 12 THEN 'dispatch_to_room_remote'
           WHEN 13 THEN 'room_subject_mutation'
           WHEN 14 THEN 'retraction_tombstone'
           WHEN 15 THEN 'dm_pin_mutation'
           WHEN 16 THEN 'group_dm_membership_grant'
           WHEN 17 THEN 'group_dm_invite_ledger'
           WHEN 18 THEN 'link_preview_media_ref'
           WHEN 19 THEN 'muc_invite_membership_grant'
           WHEN 20 THEN 'muc_invite_ledger'
           WHEN 21 THEN 'groupchat_notification_recovery'
           WHEN 22 THEN 'pending_delivery'
           WHEN 23 THEN 'tombstone_replay_deletion'
           WHEN 24 THEN 'relay_carbons'
           WHEN 25 THEN 'room_observer'
           WHEN 26 THEN 'dm_call_thread_state'
           ELSE 'kind_' || intent.kind::text
         END AS kind_family
  FROM ingress_effect_intents intent
  WHERE NOT EXISTS (
    SELECT 1 FROM ingress_effect_receipts receipt
    WHERE receipt.message_key = intent.message_key
      AND receipt.kind = intent.kind
      AND receipt.semantic_identity_hash = intent.semantic_identity_hash
  )
)
SELECT message.message_key, message.created_at,
       coalesce(jsonb_agg(jsonb_build_object(
         'kind', pending.kind,
         'semantic_identity_hash', encode(pending.semantic_identity_hash, 'hex'),
         'kind_family', pending.kind_family
       ) ORDER BY pending.kind, pending.semantic_identity_hash)
         FILTER (WHERE pending.kind IS NOT NULL), '[]'::jsonb) AS pending_pairs,
       CASE WHEN count(pending.kind) = 0 THEN 'terminalization' END AS missing_step
FROM ingress_messages message
LEFT JOIN pending USING (message_key)
WHERE message.terminal_at IS NULL
  AND message.created_at >= timestamptz '2026-09-08T11:02:00Z'
  AND message.created_at <= timestamptz '<both-replicas-fixed-at>'
GROUP BY message.message_key, message.created_at
ORDER BY message.created_at, message.message_key;
COMMIT;
```

Then connect `psql` to the primary with the **application role, never
`pg_monitor`**. Replace all placeholders and expand the two `VALUES` lists
from the reviewed output; if every selected key has an empty pending list,
omit the `reviewed_pending` INSERT entirely. Preserve the `RETURNING` output
and successful commit result with the dry-run output as the audit record.
The epoch singleton is locked first, then canonical rows in key order, then
child rows, following [ingress epoch guards](ingress-epoch-guards.md). The
transaction-local xid proof is harmless at epoch 0 and required at later
epochs. This does not activate or change the protocol epoch.

The transaction aborts if a key disappeared, terminalized, falls outside the
window, or its pending pairs differ in either direction. On any error,
`ROLLBACK` and perform a fresh dry run and review; do not weaken the checks.
The repeatable-read snapshot and canonical locks also prevent accepting a
concurrent change silently.

```sql
\set ON_ERROR_STOP on
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ WRITE;
SELECT epoch FROM ingress_protocol_epoch WHERE id = 1 FOR SHARE;
SET LOCAL waddle.protocol_epoch = '<current epoch>';
SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true);

CREATE TEMP TABLE reviewed_messages (
  message_key uuid PRIMARY KEY
) ON COMMIT DROP;
CREATE TEMP TABLE reviewed_pending (
  message_key uuid NOT NULL REFERENCES reviewed_messages (message_key),
  kind integer NOT NULL,
  semantic_identity_hash bytea NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
  PRIMARY KEY (message_key, kind, semantic_identity_hash)
) ON COMMIT DROP;
INSERT INTO reviewed_messages (message_key) VALUES
  ('<reviewed-message-key>'::uuid);
INSERT INTO reviewed_pending (message_key, kind, semantic_identity_hash) VALUES
  ('<reviewed-message-key>'::uuid, <reviewed-kind>, decode('<reviewed-hex-hash>', 'hex'));

SELECT message.message_key
FROM ingress_messages message
JOIN reviewed_messages reviewed USING (message_key)
ORDER BY message.message_key
FOR UPDATE OF message;

DO $$
BEGIN
  IF (SELECT epoch::text FROM ingress_protocol_epoch WHERE id = 1)
       IS DISTINCT FROM current_setting('waddle.protocol_epoch') THEN
    RAISE EXCEPTION 'epoch changed or singleton missing; abort repair';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM reviewed_messages) OR EXISTS (
    SELECT 1 FROM reviewed_messages reviewed
    LEFT JOIN ingress_messages message USING (message_key)
    WHERE message.message_key IS NULL OR message.terminal_at IS NOT NULL
       OR message.created_at < timestamptz '2026-09-08T11:02:00Z'
       OR message.created_at > timestamptz '<both-replicas-fixed-at>'
  ) THEN
    RAISE EXCEPTION 'manifest empty, missing, terminal, or outside incident window';
  END IF;
  IF EXISTS (
    WITH actual_pending AS (
      SELECT intent.message_key, intent.kind, intent.semantic_identity_hash
      FROM ingress_effect_intents intent
      JOIN reviewed_messages reviewed USING (message_key)
      WHERE NOT EXISTS (
        SELECT 1 FROM ingress_effect_receipts receipt
        WHERE receipt.message_key = intent.message_key
          AND receipt.kind = intent.kind
          AND receipt.semantic_identity_hash = intent.semantic_identity_hash
      )
    )
    (SELECT * FROM actual_pending EXCEPT SELECT * FROM reviewed_pending)
    UNION ALL
    (SELECT * FROM reviewed_pending EXCEPT SELECT * FROM actual_pending)
  ) THEN
    RAISE EXCEPTION 'pending pairs differ from reviewed manifest; abort repair';
  END IF;
END $$;

INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash)
SELECT message_key, kind, semantic_identity_hash FROM reviewed_pending
ON CONFLICT DO NOTHING
RETURNING *;

UPDATE ingress_messages SET terminal_at = now()
WHERE message_key IN (SELECT message_key FROM reviewed_messages)
  AND terminal_at IS NULL
RETURNING message_key;
COMMIT;
```
