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
| `ingress.maintenance.runs{phase,outcome}` | `ingress_maintenance_runs_total` | `IngressMaintenanceFailing` |
| `ingress.maintenance.terminalized_messages` | `ingress_maintenance_terminalized_messages_total` | Terminalization progress |
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
`IngressCnpgQueriesMissing` warns if GC eligibility, GC age,
`cnpg_waddle_ingress_nonterminal_age_oldest_seconds`, or the backlog query's
`cnpg_waddle_ingress_nonterminal_messages{kind="none"}` sentinel is absent for
15m. The age query always returns one row (zero for no backlog) and the
backlog query always emits the `none` sentinel row, so a missing or failing
query cannot masquerade as healthy receipt completeness.
`IngressUnresolvedEffectsGrowing` warns on a positive counter increase over
1h, grouped by kind. It sees only locally executed effects, not obligations
that were recorded but never executed; it is not a current queue gauge. Use
`IngressNonTerminalBacklog` for the canonical row-level view.

`IngressNonTerminalBacklog` warns when any canonical row older than 10 minutes
remains non-terminal, sustained for another 10m (roughly 20m after creation,
plus scrape/evaluation delay). Its `kind` is the unreceipted intent family;
`terminalization` means all receipts exist but terminalization itself is
missing; `none` is the always-zero sentinel and never fires. A row counts once per pending family, even with several intents in
that family, so summing families can count one row more than once. The 10m
age threshold is a generous multiple of the 5s Phase C budget: investigate
missing receipts or failing maintenance. Check
`ingress.maintenance.runs{outcome!="complete"}` (Prometheus:
`ingress_maintenance_runs_total{outcome!="complete"}`) and the pending pairs
below. GC cannot reclaim these rows. Both CNPG queries run only on the primary.

The dedicated ingress authority pool defaults to 4 connections per pod;
`WADDLE_INGRESS_DB_POOL_SIZE` overrides it. Transactions and retries are
bounded (the soak measured 23 ms p99 transaction time). The connection
budget is approximately 77 at a 3-pod rollout peak against 100 PostgreSQL
connections, below the 80% alert threshold; re-derive it before raising
the pool override.

## Periodic maintenance

Each pod runs bounded maintenance at startup, after committed decisions, and
on a jittered 30-second periodic tick, including when no traffic arrives.
The tick resets after a run. A partial, failed or timed-out pass schedules a
continuation with exponential backoff from 1 second to 30 seconds; a complete
pass resets that backoff.

The lineage attestation gate covers the entire pass. Terminalization first
pages receipt-complete, non-terminal messages older than a 60-second grace
period in `(created_at, message_key)` order. Each row is locked and its receipt
completeness rechecked before terminalization. Contended or failed rows leave
the pass partial while pagination continues to later rows. Continuations retain
the keyset cursor so a contended prefix cannot starve later rows, then wrap to
retry skipped rows. Retention GC follows. Each phase has a timeout inside a hard pass
deadline. Maintenance shares the bounded ingress pool and holds at most one
connection at a time.

`ingress.maintenance.runs` labels phases as `pass`, `terminalization`, or
`retention_gc`, with outcomes `complete`, `partial`, `failed`, or `timed_out`.
The `pass` series includes attestation and hard-deadline failures.
`IngressMaintenanceFailing` warns on failed or timed-out passes in the last
hour. A partial pass can be ordinary bounded backlog progress or skipped
contended rows; use repeated partial outcomes together with the backlog and
`ingress.maintenance.terminalized_messages` to distinguish draining from
stalled work. All maintenance series are zero-seeded at startup.
Existing `ingress.gc.*` series retain their GC-specific meaning and outcomes.

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

### V1014 cutover (#1739–#1743, PR #1752)

The recovery follow-ups ride a second one-shot Recreate. Expect and do not
treat as incidents: (1) every retained XEP-0198 session fails to resume and its
unacked outbound frames are discarded (clients reconnect and catch up via MAM);
(2) ingress obligations that were in flight at cutover are abandoned — observer
plugin runs, XEP-0357 notification candidates and pending rows that had not yet
been inserted — because V1014 deletes all canonical rows, intents, receipts,
aliases and SM refs; (3) queued `pending_delivery` rows and archives are kept,
and rows claimed by a discarded session are released once at startup
(`pending_delivery_startup_migrations` marker `ingress_v1014_pending_claim_reset_v1`)
so they flush on the first reconnect; (4) `groupchat_notification_recovery`
rows without a canonical `message_key` are deleted once when the inbox schema
adds the column. `IngressNonTerminalBacklog` starts from an empty table after
the cutover; any post-cutover row is new work. Roll-forward only: a pre-V1014
image refuses the ledger.

### V1015/V1016 cutover (#1756, PR #1758)

Keyed recipient SM append receipts rode a further one-shot `Recreate`. Unlike
V1012 and V1014 the migrations themselves are **purely additive**: V1015 creates
`sm_ingress_appends`, V1016 indexes it by `accepting_stream_id`, and nothing is
reset: the migration transaction deletes no canonical row, intent, receipt,
alias, SM ref or obligation row. Both statements are `CREATE ... IF NOT EXISTS`
because the SM store's runtime initializer creates the same table when it opens,
so either may run first.

**Additive DDL is not a harmless cutover, and the two must not be conflated.**
The `Recreate` around it still stops every replica, and the outgoing binary's
SIGTERM path drains: it promotes each live connection's unacked queue and calls
`confirm_drained`, which deletes the durable session row on success
(`server/session_janitors.rs`). So normally drained XEP-0198 sessions are
**retired, not preserved** — those clients reconnect fresh and catch up through
MAM rather than resuming. Only sessions whose drain fails or times out keep a
durable row to resume from. A failed post-cutover resume is the expected outcome
here, not an incident.

Likewise, V1015/V1016 delete no obligation rows — but that is not the same as no
obligation being abandoned. If SIGTERM reaches a replica during post-commit
Phase C, the canonical row and its intents stay durable while execution stops
without writing a receipt, and nothing re-drives it: there is no recovery
executor (RFC 0018 stated limitation (i), tracked as #1755), and periodic
maintenance only terminalizes work whose receipts are already complete. Absent a
same-origin retry that obligation stays unresolved indefinitely. Inspect
unresolved effects after this cutover rather than assuming the additive
migration left none.

`Recreate` was required for the **writers**, not the schema. The ledger is an
exactly-once gate only if every writer consults it. A replica still running the
previous build keeps the unkeyed append path, so during a rolling window an old
owner could append with no ledger row while a concurrent replay allocated the
same `(obligation, resource)` a second time — the duplicate the migration exists
to prevent. Stopping every old writer before the first keyed writer starts is
what makes the gate total for **appends made after the cutover**.

It is not retroactive, and the migration is additive rather than a reset, so it
mints no proof for work already in flight. An append the previous binary
committed unkeyed, whose progress transaction then failed, leaves a real SM queue
entry and an unresolved canonical obligation but **no ledger row**. A
post-cutover retry reads an empty key and appends that resource again. The
window is bounded by the obligations outstanding at cutover; treat a duplicate
delivery reported across the cutover boundary as expected rather than as a gate
failure, and prefer draining outstanding obligations before a comparable cutover
in future.

Rollback is fail-closed **only back to the ledger itself**. The migration ledger
guard makes a binary whose catalog lacks a version recorded in `_migrations`
refuse to start (`pre_v1010_catalog_refuses_a_v1010_ledger_until_roll_forward`,
`server/crates/waddle-server/src/db/migrations/tests.rs`), so an image between
`43860571` (#1671, the ledger) and #1758 cannot come up against this database.
The guard fires at startup only, which is exactly why `Recreate` was needed at
cutover and not at the flip-back (#1765): it does nothing about an old pod that
is already running.

**Never roll back past `43860571` (#1671).** Images older than the ledger have no
guard. Their runner carries the removed "hard-cut protection", which drops and
recreates `_migrations` on an unknown version and replays its whole catalog —
including global V0001 and waddle V1001, which destructively drop and recreate the
auth, channel and message tables. A deep rollback is therefore data loss, not a
refused startup. Treat the ledger commit as the rollback floor for this database.

#1758 also bumped three wire versions — `remote_user_side_effect.v3`,
`remote_resource_route.v6` and `deliver_ordered.v10`. Because the cutover rode
its own `Recreate`, and because the flip-back rolled two builds of identical
server code, the first genuine mixed-version window for `deliver_ordered.v10`
is the next deploy that changes server source.

**Ledger lifetime.** The obligation owns the proof, not the stream. A successful
resume deletes the detached snapshot while the logical stream continues, so
session-scoped evidence would vanish while the obligation was still retryable.
Only two paths remove a row: retirement of gap-covered proofs when a session is
deleted, and ingress retention GC once the canonical row is reclaimed, at which
point the obligation can never be retried again.

**If the append ledger looks wrong.** Proof that outlives its payload suppresses
the retry that would have recovered the stanza, so the failure mode is silent
loss rather than a duplicate. List candidates in a read-only snapshot:

Every incident predicate is applied **before** `LIMIT` so the cheapest healthy
shapes are excluded early. That is not sufficient on its own: several healthy
terminal-session outcomes described below satisfy every predicate, so with enough
of them an oldest-first page returns the same benign rows forever and never
reaches a newer real loss. The query is therefore keyset-paginated — carry
`(appended_at_ms, message_key, receipt_kind, semantic_identity_hash, resource)`
from the last row of each page into the next, and keep paging until a page is
short:

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SELECT a.message_key, a.receipt_kind,
       encode(a.semantic_identity_hash, 'hex') AS semantic_identity_hash,
       a.resource, a.accepting_stream_id, a.sequence,
       to_timestamp(a.appended_at_ms / 1000.0) AS appended_at
FROM sm_ingress_appends a
JOIN ingress_messages m
  ON m.message_key = a.message_key::uuid
 AND m.terminal_at IS NULL
LEFT JOIN sm_sessions s
  ON s.stream_id = a.accepting_stream_id
LEFT JOIN sm_unacked u
  ON u.stream_id = a.accepting_stream_id
 AND u.sequence = a.sequence
LEFT JOIN ingress_delivery_receipts d
  ON d.message_key = a.message_key::uuid
 AND d.kind = a.receipt_kind
 AND d.semantic_identity_hash = a.semantic_identity_hash
 AND d.resource = a.resource
LEFT JOIN ingress_effect_receipts e
  ON e.message_key = a.message_key::uuid
 AND e.kind = a.receipt_kind
 AND e.semantic_identity_hash = a.semantic_identity_hash
WHERE u.stream_id IS NULL   -- the payload is gone
  AND s.stream_id IS NULL   -- and so is the whole session, not merely acked or gapped
  AND d.message_key IS NULL -- this resource was never receipted as delivered
  AND e.message_key IS NULL -- nor was its effect settled generically
  -- keyset cursor: omit on the first page, then carry the last row forward
  AND (a.appended_at_ms, a.message_key, a.receipt_kind,
       a.semantic_identity_hash, a.resource)
      > (:last_appended_at_ms, :last_message_key, :last_receipt_kind,
         :last_semantic_identity_hash, :last_resource)
ORDER BY a.appended_at_ms, a.message_key, a.receipt_kind,
         a.semantic_identity_hash, a.resource
LIMIT 50;
COMMIT;
```

Each predicate excludes one healthy shape. A sequence that was acknowledged or is
covered by the replay gap legitimately has no `sm_unacked` row, so `u` alone
proves nothing. A message may stay non-terminal because a *different* unresolved
intent holds it open while this resource was delivered normally, so the
`ingress_delivery_receipts` join is what separates an undelivered resource from a
healthy one. Not every settled append reaches that table, though: a recorded
`RelayFullJid` that falls back to a local detached session writes a ledger row but
settles generically through `ingress_effect_receipts`, so `e` excludes those too.
Both `receipt_kind` and `semantic_identity_hash` are selected because one
canonical message can hold several routes to the same resource; without them a
candidate cannot be tied back to a specific obligation, its recorded intent or its
progress rows. Note the `::uuid` casts: the ledger stores `message_key` as text
while the ingress tables use `uuid`.

Rows that survive every predicate are **candidates, and the query cannot promote
them to a verdict.** A deleted session is not proof of loss: the proof is meant
to outlive its session, and several healthy outcomes delete the session and its
`sm_unacked` rows while the proof legitimately stands. The two easiest to reach
are —

- the retained frame was **acknowledged during resume**, which is delivery; and
- the payload was **promoted to `pending_delivery`** on expiry, which is a
  durable handoff, not a destruction.

— and the full promotion outcome set below adds more. None leaves a delivery or
effect receipt, so all of them satisfy the query. Worse,
`pending_delivery` rows carry no reference back to the obligation that produced
them, so a promoted payload **cannot currently be correlated to its ledger row at
all** — that missing link is itself part of what #1760 has to fix.

So before treating a candidate as the #1760 shape (durable append proof must not
outlive the payload it stands for), rule out every healthy way its session can
have ended: a `pending_delivery` row to the same recipient around `appended_at`
(the `Queued` outcome), a live redelivery or any other handled promotion outcome
for that stream, and acknowledgement during resume.

**A deleted session has eight terminal outcomes, not two, and six of them are
healthy.** Promotion classifies every unacked stanza
(`sm_promotion/types.rs::PromotedOutcome`): `Redelivered` to an alternate
resource, `Queued` into `pending_delivery`, `Bounced` per XEP-0160 §3,
`Dropped`, `NotPromotable`, `Unparseable`, `Scrubbed` by a racing
XEP-0424/0425 tombstone, and `StorageFailure`. Only `StorageFailure` blocks
`confirm_drained`; every other outcome counts as handled and the durable session
row is deleted. So a healthy `Redelivered` or `Scrubbed` stanza leaves a
non-gap-covered proof with **no** `pending_delivery` row and **no** receipt for
the original obligation — passing the query and both checks above. Correlate the
promotion outcome for that stream, not just `pending_delivery`.

**And the acknowledgement check above often cannot be performed at all.**
`last_acked` is a column on `sm_sessions`, so deleting the session deletes the
acknowledgement frontier along with `sm_unacked`. Accepted `<a/>` handling
advances live stream state and aggregate counters rather than a per-sequence
durable record. For a candidate whose session is already gone there is therefore
usually no retained database evidence that a given `a.sequence` was
acknowledged: treat that question as **historically indeterminate** rather than
resolving it against the row's absence, which is the same mistake in a different
direction.

**The query also has a hard blind spot in the other direction: it finds the loss
only before anything retries it.** Once a retry runs against a stale proof, the
keyed append returns `AlreadyAppended`, which counts as allocated, so
`execute_detached::record_resource` writes the matching delivery receipt even
though the payload is gone — and the aggregate can then terminalize. From that
point `d.message_key IS NULL` excludes the lost resource, and `m.terminal_at IS
NULL` excludes it once the message settles. A message lost this way looks
*delivered* in every table this query inspects. Treat an empty result as "no
loss caught in its pre-retry window", never as "no loss". Corroborating a
suspected loss after that point means comparing the recipient's archive or
client-visible history against the canonical envelope, not querying the ledger.

Two further caveats: the sequence comparison is modulo 2^32 while SQL is not
wrap-aware — the server deliberately performs that comparison in Rust — and a row
can simply be in flight, so re-run before acting.

Record the row and its full obligation identity. Do not delete ledger entries by
hand. No ledger surgery.

### archive_seq cutover (#1770 stage 1, PR #1771)

Per-archive commit ordinals require a further one-shot `Recreate`. Every old
writer must stop before the new binary backfills the archive: old inserts omit
`archive_seq` and fail once the new binary makes that column `NOT NULL`.
A rolling window cannot safely mix these writers. Expect a brief full outage
and client reconnects; the drain and unresolved-obligation caveats above still
apply.

At startup, the MAM store's `ensure_schema` adds the column, ranks legacy rows
within each archive by `(timestamp, id)`, seeds `mam_archive_sequences` from
per-archive maxima without lowering existing counters, and enforces `NOT NULL`
and `UNIQUE (room_jid, archive_seq)`. Backfill ranks every row of any archive
that still contains a NULL ordinal, so restarting an interrupted backfill is
safe with no old writers. PostgreSQL sets `NOT NULL`; SQLite rebuilds the
legacy table with that constraint. This is store-owned schema, not a new
ingress migration-ledger version.

After every replica runs the new digest and is ready, verify the backfill in
one read-only PostgreSQL snapshot:

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SELECT COUNT(*) FROM mam_messages WHERE archive_seq IS NULL;

SELECT m.room_jid, MAX(m.archive_seq) AS max_archive_seq, s.next_seq
FROM mam_messages m
LEFT JOIN mam_archive_sequences s ON s.archive_jid = m.room_jid
GROUP BY m.room_jid, s.next_seq
HAVING s.next_seq IS NULL OR s.next_seq < MAX(m.archive_seq);

SELECT room_jid, archive_seq, COUNT(*)
FROM mam_messages
GROUP BY room_jid, archive_seq
HAVING COUNT(*) > 1;
COMMIT;
```

The NULL count must be **0**; the counter and duplicate queries must return
**no rows**. Every retained archive needs a counter at least its maximum
ordinal. A counter above that maximum is valid after deletion; never lower it.

**Rollback is not supported.** A pre-cutover binary can still read the table
because it ignores the extra column, but its inserts fail on `NOT NULL`.
Flipping back to an old image does not restore service; reverting the schema
to enable such a rollback is not a supported procedure. Roll forward with an
ordinal-aware binary. Do not treat this store-owned change as a ledger startup
refusal.

Once the rollout and verification complete, open a follow-up PR restoring
`RollingUpdate` (`maxSurge: 1`, `maxUnavailable: 0`), following #1758 → #1765.
That flip-back restores the deployment strategy, not the pre-cutover binary
or schema. Archive ordinals do not yet enforce concurrent live dispatch order;
RFC 0018 §3.7 records stage 2 and #1770 remains open.

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

In Grafana Explore, verify decisions, unresolved kinds, histogram buckets,
maintenance and GC; absent metrics are not evidence of healthy zero activity:

```promql
sum by (class) (rate(ingress_decisions_total[10m]))
sum by (kind) (increase(ingress_effects_unresolved_total[1h]))
histogram_quantile(0.99, sum by (le) (rate(ingress_tx_duration_seconds_bucket[10m])))
sum by (outcome) (increase(ingress_gc_runs_total[1h]))
sum by (phase, outcome) (increase(ingress_maintenance_runs_total[1h]))
sum(increase(ingress_maintenance_terminalized_messages_total[1h]))
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


## Detached delivery progress

For direct detached fanout, `ingress_delivery_receipts` tracks completed full
JIDs by canonical message and complete effect receipt identity. An aggregate
route receipt exists only after every recorded resource has completed. A
partial fanout therefore remains non-terminal even if today's registry offers
only a subset of its unfinished resources. Ordinary duplicate ingress retries
only unfinished recorded targets and restores the canonical message payload;
a resource that has reconnected can receive the retry live.

Since #1756 this path allocates exactly one durable queue entry per (recorded
obligation, resource), with no retry-induced duplicate. The stream-independent
`sm_ingress_appends` ledger is consulted before any session work and its primary
key is the gate, so neither a concurrent duplicate decision nor a crash between
the append and its progress commit allocates a second entry. Four limits remain
explicit.

XEP-0198 still permits a client-observed duplicate after an uncertain
acknowledgement.

**"No session" means no session *and no prior proof*.** When neither exists the
append does not happen and the obligation stays unresolved for its recorded route
to retry or degrade. But the ledger is consulted *before* the session lookup
(`session_registry/resources.rs`, `get_ingress_append` then `void_allocation`,
both ahead of `find_session_id_matching`), so a proof that outlived its session
returns `AlreadyAppended` and never reaches the no-session path.
`SmKeyedAppendOutcome::is_allocated` counts that as allocated, and callers record
delivery progress on exactly that condition — so the lost message is recorded as
**delivered**, not left outstanding. `keyed_append_older_proof_precedes_missing_session_lookup`
pins the ordering. This is the #1760 loss shape in its most severe form and it is
why the query above cannot be the last word.

**Retirement ignores the acknowledgement frontier.** `void_gap_covered` reads
only `replay_gap_through` from `sm_sessions` and deletes every proof the gap
covers (`sm_persistence/ingress_append.rs`); it never consults `last_acked`, while
the registry-side `void_allocation` does. So if append progress fails, the client
later acknowledges that sequence, and a re-detach advances the gap past it before
the session is deleted, a valid delivered proof is retired and recovery can
allocate the same obligation again. `acknowledged_allocation_is_never_voided_by_a_later_gap`
establishes such a proof is valid; the two voiding rules disagree about it. The
divergence is the enumeration problem #1760 exists to remove.

**The guarantee covers keyed appends only.** In a clustered route whose
remote-resource owner refresh resolves locally against a detached recipient,
`deliver_local_full_jid_after_target_refresh` passes no append context
(`clustering/route_bridge/delivery/local.rs:109-127`), so that append is unkeyed
and a failed effect-receipt write can let recovery append the same resource
again. That residue is tracked by #1760.

Earlier committed progress survives restart and is excluded from later
decisions. Progress writes and the final aggregate receipt share one
epoch-attested transaction under the canonical message lock. Lock contention
leaves the obligation retryable.

When investigating a pending direct route, compare its recorded fanout with
its resource progress rows using the full effect receipt key. A missing
resource is outstanding delivery work, not evidence that the aggregate can be
settled. Do not synthesize a receipt from today's smaller registry audience.
MUC groupchat occupant fanout does not use these progress rows.

### Pending delivery and SM database placement

`WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL` and `WADDLE_XMPP_SM_DATABASE_URL`
must be colocated with the global
ingress database on every backend, including deployments without clustering.
SQLite requires the same URL as `WADDLE_DATABASE_URL`; PostgreSQL requires the
same live database/schema identity. Startup rejects a separate store before
initializing its schema or hydrating retained SM sessions. Leaving either
override unset shares the global database
pool. In-memory pending and SM storage are available only when the global database itself
is in memory; it does not survive restart.

SM session and replay tables must participate in the same V1014 reset as ingress
and pending claims. A separate retained-session store would leave replay copies
bound to claims that the reset released, allowing expiry promotion to enqueue
the same pending delivery again. Split SM stores are therefore rejected even
when clustering is disabled.
