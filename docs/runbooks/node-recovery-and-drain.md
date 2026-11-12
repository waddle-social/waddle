# Node recovery and drain

Procedures for one `waddle-server` node in the clustered production
deployment: namespace `waddle`, Deployment `waddle-server`, CloudNativePG
cluster `postgresql`, database `waddle`, schema `public`.

Status: written from the source at this commit. None of these procedures
has been rehearsed on a cluster. Where a step reads a metric, the metric
name is the OpenTelemetry instrument name from
[the metrics reference](../operations/server-metrics.md); the Prometheus
name is derived there. Alerts named below that live under
`infrastructure/waddle.cloud/rules/staged/` are not evaluated anywhere yet.

SQL is read as the `postgres` superuser inside the primary instance pod:

```sh
PRIMARY=$(kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o name)
psql_primary() { kubectl -n waddle exec -i "$PRIMARY" -c postgres -- psql -v ON_ERROR_STOP=1 -d waddle "$@"; }
```

Every query below only reads. Do not repair claim, lease, outbox or
stream-management rows by hand: each is fenced by an epoch or a lease that
the owning node checks, and a manual write can hand two nodes the same
authority.

## Graceful drain

`SIGTERM` and `SIGQUIT` stop accepting, drain and exit; the Deployment
controller starts the replacement. The server does not re-exec itself.

What happens, in order (`server/crates/waddle-server/src/shutdown_deadline.rs`):

1. The pod's `preStop` sleeps `preStopSleepSeconds` (5 s) so the endpoint
   is removed before the drain starts.
2. Connections are drained (at most half of `WADDLE_DRAIN_TIMEOUT_SECS`,
   default 30 s) and resumable XEP-0198 sessions are detached and promoted
   to durable custody inside the same budget.
3. In parallel, room claims are released inside
   `WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS` (default 5 s).
4. The node's identity is retired (5 s reserved) and a terminal sweep
   writes its departure (2 s reserved).

The chart's `terminationGracePeriodSeconds` (52 s by default) is the sum of
these plus a 5 s kill margin, and `validations.yaml` refuses a smaller
value.

To drain one node:

```sh
kubectl -n waddle delete pod <pod>          # sends SIGTERM, honours the grace period
kubectl -n waddle get pod -w
```

Check afterwards:

| Check | Healthy |
|---|---|
| `xmpp.sm.drain_timeout` | did not increase |
| `waddle.clustering.claims_abandoned_on_drain` | did not increase |
| `SELECT count(*) FROM public.clustering_claims c JOIN public.clustering_nodes n USING (node_id) WHERE n.expired` | reaches 0 within one orphan-reaper interval (120 s by default) |

A drain that hits its deadline does not prove its work finished: claims it
did not release stay until the node's lease expires, and another node then
takes them. That is the node-loss path below, not data loss.

Do not delete both pods at once. With clustering enabled the two replicas
hold each other's failover capacity.

## Node loss and claim or slot steal

A node that dies without draining leaves three kinds of state, each
released by time, not by an operator:

| State | Table | Released when | Default |
|---|---|---|---|
| Node lease | `clustering_nodes` | its heartbeat is older than `WADDLE_CLUSTERING_NODE_LEASE_TTL_MS` | 30 s |
| Entity claims (rooms, sessions) | `clustering_claims` | the orphan reaper of a live node finds the owner's lease expired, or a writer steals the claim | reaper every 120 s |
| Keypair slot (libp2p identity) | `clustering_keypair_slots` | its heartbeat is older than `WADDLE_CLUSTERING_LEASE_TTL_MS` | 30 s |

Read the state:

```sql
SELECT node_id, expired, now() - heartbeat AS heartbeat_age FROM public.clustering_nodes ORDER BY heartbeat;
SELECT entity_type, node_id, count(*) FROM public.clustering_claims GROUP BY 1, 2 ORDER BY 1, 2;
SELECT slot_index, holder_node, expired, now() - heartbeat AS heartbeat_age FROM public.clustering_keypair_slots ORDER BY 1;
```

Expected sequence after a node dies: its `clustering_nodes` row stops
renewing; a surviving node's reaper or the next writer to a claimed entity
takes each claim with a higher `claim_epoch`; the replacement pod leases a
free or lapsed keypair slot and joins. Clients of the dead node reconnect
and resume against the survivor within the XEP-0198 resume window.

Signals:

- `waddle.clustering.claim.mutations` and `waddle.clustering.fence.results`
  rise during the takeover and settle. A `rejected` fence result is the
  dead or partitioned node being refused, which is the fence working.
- `waddle.clustering.keypair_lease.outcomes{outcome="fencing_lost"}` on a
  node that is still running means its slot was taken while it lived:
  its renewals stalled for longer than the lease lifetime. That node stops
  its clustering scope. Restart the pod; check database latency for the
  cause.
- `waddle.clustering.keypair_lease.outcomes{outcome="exhausted"}` means a
  starting pod found no free slot: more pods than keypairs, or slots still
  inside their lease lifetime. Wait one lifetime before concluding the pool
  is too small.

If a partitioned node keeps running after takeover, it cannot write under
its old claims: every fenced write checks the claim epoch. Bytes it already
wrote to a client cannot be retracted. Delete the pod.

## Stuck keypool activation

Symptom: every pod exits at startup, `waddle.restricted_runtime.startup.refusals{stage="startup"}`
rises, and the log names the keypair lease attachment. The enrolled
generation is still pending.

```sql
SELECT generation, state, activation_after, activation_after <= now() AS due, slot_count
  FROM public.clustering_keypool WHERE singleton;
SELECT count(*) AS leased_rows FROM public.clustering_keypair_slots;
```

A generation becomes active only when `owner-phase` runs after
`activation_after` **and** `clustering_keypair_slots` is empty. The server
never activates it.

| Reading | Action |
|---|---|
| `state = 'pending'`, `due = f` | Wait until `activation_after`, then run the owner Job again |
| `state = 'pending'`, `due = t`, `leased_rows = 0` | Run the owner Job again; it activates the generation and exits 0 |
| `state = 'pending'`, `due = t`, `leased_rows > 0` | A server is leasing against a pending generation, which the attachment refuses; find and stop it (`SELECT * FROM pg_stat_activity WHERE datname = 'waddle'`), then run the owner Job |
| No row | The database was prepared without a clustering plan while the server is started clustered; run the owner Job with clustering configured |

The owner Job is created from the suspended CronJob, with the server at
zero replicas, as in
[the cutover runbook](restricted-runtime-cutover.md#phase-b-cutover-window-outage)
step 7. Do not update `clustering_keypool` by hand: its guard triggers
refuse it, and the activation check exists to keep an old server's lease
from surviving into the new generation.

## Restricted attach refusal at startup

A pod under the restricted database role refuses to start instead of
serving on a database it cannot verify. It never falls back to the
owner-as-runtime path. The refusal is counted in
`waddle.restricted_runtime.startup.refusals` by `stage` and printed once on
standard error as `Error: …` followed by its causes.

| Stage | Error text begins | Meaning | Action |
|---|---|---|---|
| `handoff` | `the operator-supplied database handoff is invalid` | The document is not a version 3 handoff, a field is missing, nil or repeated | Re-emit it with `emit-handoff`; compare with `emit-handoff --verify` |
| `handoff` | `the operator-supplied release identity differs from WADDLE_GIT_SHA` | The handoff was composed for another release | The chart composes it per release; check `restrictedRuntime.releaseGitSha` equals the pod's `WADDLE_GIT_SHA` |
| `handoff` | `restricted startup requires WADDLE_GIT_SHA` | The variable is unset or not 40 lowercase hex characters | Fix the release values |
| `handoff` | `the operator-supplied deployment identity differs from WADDLE_DEPLOYMENT_UUID` | Handoff and pod name different deployments | Never change `deployment.uuid`; find which side is wrong |
| `handoff` | `restricted runtime cannot run a lineage enrollment or adoption action` | `WADDLE_DB_LINEAGE_ACTION` is set | Unset `deployment.lineageAction` |
| `handoff` | `cooperative catalog fencing is not held` | The cutover record's mutator policy is not `quiesced_for_serving_interval` | Correct the cutover record |
| `preflight` | `restricted runtime connection host, port, or socket differs` | `WADDLE_DATABASE_URL` does not name the endpoint in the cutover record | The URL must be the direct read-write Service, not a pooler |
| `preflight` | `restricted runtime TCP connections require PostgreSQL sslmode=verify-full` | The URL does not verify the server certificate | Use the chart's composed URL |
| `preflight` | `restricted runtime connection does not target the configured database` | Wrong database name | Fix the URL |
| `preflight` | `configured pool login differs from attested runtime login` | The URL's login is not the runtime login | Fix the credential |
| `preflight` | `the pinned startup preflight did not complete` | A pinned OID, the migration ledger or the epoch manifest differs, or the connection failed. The cause follows | If a pinned object was recreated, run the owner Job to publish a fresh handoff. Reconcile the backend before retrying; an incomplete preflight is not a rollback |
| `preflight` | `the pinned startup preflight pool close timed out` | The throwaway pool did not close in time | Check for the backend in `pg_stat_activity`, then let the pod retry |
| `serving_pool` | `the restricted runtime could not attach its serving pool` | Lineage, the complete migration ledger or the runtime ACL did not verify on the serving pool | A release with new migrations needs the owner Job first |
| `startup` | names a store, for example `failed to attach MUC destroy completion outbox` | That store's relation is missing, has another shape or owner, or the runtime's privileges differ from the grant matrix | Run the owner Job. If it refuses, its error names the relation or privilege |
| `startup` | `clustering claim authority attachment failed` | The database was prepared without a clustering plan | Run the owner Job with clustering configured |

`CrashLoopBackOff` on every pod after a release usually means the release
changed owner preparation and the owner Job was not run; see
[after the cutover](restricted-runtime-cutover.md#after-the-cutover).

## Ingress entry refusals

`waddle.ingress.entry.transactions` counts every ingress transaction by
authority and outcome.

| Outcome | Meaning | Action |
|---|---|---|
| `epoch_unsupported` | The database's protocol epoch is above the one this binary implements | An older binary is running against a newer database. Stop the rollout or roll forward; do not lower the epoch |
| `refused` | The entry could not prove its timeouts or the epoch row | Treat as an invariant violation; collect the pod log and stop promotion |
| `preparation_timeout` | The epoch preparation waited longer than its clamp (100 ms lock, 250 ms statement for writers) | Look for a long transaction holding the epoch relation: `SELECT pid, state, now() - xact_start FROM pg_stat_activity WHERE datname = 'waddle' ORDER BY xact_start` |
| `acquisition_timeout` | No connection was available before the deadline | Pool exhaustion; compare with `PostgresBackendsHigh` |

## Destroy completion stuck

A room destroy commits its tombstone and queues a completion
(`clustering_muc_destroy_outbox`) that delivers the XEP-0045 notifications
and finishes cleanup. The requesting session drains it inline; otherwise
every node's janitor leases due rows.

```sql
SELECT count(*) AS waiting,
       to_timestamp(min(available_at_ms) / 1000.0) AS oldest_available,
       count(*) FILTER (WHERE lease_token IS NOT NULL) AS leased
  FROM public.clustering_muc_destroy_outbox;
```

| Reading | Meaning | Action |
|---|---|---|
| Rows waiting, none leased, `oldest_available` in the past | No janitor is draining | Check `waddle.janitor.sweeps{janitor="destroy_completion"}` on each pod; a pod without it has a wedged janitor: restart that pod |
| Rows leased, lease older than a few minutes | The holder died mid-completion | Nothing: the lease lapses and another janitor takes it |
| The same row waits across many sweeps | The completion fails every time | Read the pod log for `Failed to drain completed MUC destroy work`; the room stays tombstoned and cannot be recreated until the completion finishes |

A destroy request can be answered with an error although the room is
destroyed, when another node's janitor leases the completion first. The
room's state is the tombstone, not the reply.

## Relay uncertainty backlog

A delivery relayed to another node ends acknowledged, refused, or
uncertain. Uncertain means the reply was lost: the peer may have
committed. The sender never treats that as a rejection and never resends
blind; the obligation is recovered by looking it up at the authority.

Signals: `waddle.relay.ordered.outcomes{outcome="uncertain"}`,
`waddle.muc.remote_dispatch.outcomes{outcome=~"uncertain|timed_out"}`,
`waddle.muc.remote_acceptance.oldest_pending_age`. These have no call site
yet; until they do, use `ingress.effects.unresolved`,
`ingress.maintenance.recovered_obligations` and
`ingress.maintenance.unrecoverable_obligations`.

1. Establish whether the peer is alive: `clustering_nodes` above, and the
   pod.
2. If the peer is dead, its claims move as in node loss, and maintenance
   recovers the obligations against the new owner. Watch
   `ingress.maintenance.recovered_obligations` rise and
   `ingress.effects.unresolved` stop growing.
3. If both nodes are alive and outcomes stay uncertain, the relay between
   them is failing: check the swarm port (7900) and NetworkPolicy, and
   whether the two pods run different releases
   (`incompatible_peer`). Two message versions live at once is a rollout
   that should have been a coordinated cutover.
4. `ingress.maintenance.unrecoverable_obligations` above zero is the
   stop condition: an obligation maintenance could not recover. Do not
   delete it; collect the row and the log.

## Stream management custody backlog

Custody of unacknowledged stanzas lives in the stream's durable session
(`sm_sessions`, `sm_unacked`), and after a detach in pending delivery.

Signals and their meaning:

| Signal | Meaning |
|---|---|
| `xmpp.sm.unacked_evicted` by `path` | Stanzas left an attached replay window without acknowledgement; message-loss risk for a resuming client |
| `xmpp.sm.resume.results` by `outcome` | `not_found` after a rollout older than the resume window is expected; `replay_gap`, `handled_too_high` and `detached_divergence` are not |
| `xmpp.sm.detached.append.rejected` | A node appended to a detached session it does not own; `not_owner` after a takeover is the fence working |
| `xmpp.pending_delivery.unresolved_poison_pill` | A pending-delivery row could not be resolved; always zero when healthy |
| `xmpp.sm.send_window_pause_timeouts` | Peers stopped acknowledging |

```sql
SELECT count(*) AS detached_sessions, to_timestamp(min(detached_at_ms) / 1000.0) AS oldest_detach FROM public.sm_sessions;
SELECT count(*) AS unacked FROM public.sm_unacked;
SELECT count(*) AS pending FROM public.pending_delivery;
```

Column names differ between schema versions; if a query fails, read the
table's columns first (`\d public.sm_sessions`).

`sm_sessions` holds detached sessions. A growing count, or an
`oldest_detach` far older than the resume window, means sessions are not
being resumed or expired, so check the SM expiry janitor's
heartbeat (`waddle.janitor.sweeps{janitor="sm_expiry"}`). Rows are removed
by acknowledgement, resumption or expiry. Deleting them discards messages
the server has promised to deliver.

## Actor or pump panic

`waddle.io.pump.exits{cause="panicked"}`, `waddle.io.custodian.retirements`
and `waddle.connection_actor.retirements{cause=~"panic|abandoned"}` are
declared without call sites yet. Until they are recorded, a panic is
visible in the pod log (`panicked at`) and, if it aborts the process, as a
container restart. After a panic check that the affected account has
exactly one binding and that no claim is left on a node that no longer
holds the session:

```sql
SELECT entity_type, entity, node_id, claim_epoch FROM public.clustering_claims ORDER BY claim_epoch DESC LIMIT 50;
```
