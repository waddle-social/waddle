# #2046 workload and budget measurement plan

Package A of [#2046](https://github.com/waddle-social/waddle/issues/2046)
requires the supported workload and its latency, memory and recovery
budgets to be recorded from measurement before implementation, and not
taken from vendor actor benchmarks or a modelled identity count. No such
measurement exists. This plan states what tooling the repository has, what
is missing, and exactly what has to be run and recorded.

It contains no budget and no result. Workload sizes and numerical budgets
are owner decisions ([#2106](https://github.com/waddle-social/waddle/issues/2106));
nothing here proposes a value for them. Nothing was run for this record.

## What exists

| Piece | Location | What it does | What it does not do |
|---|---|---|---|
| Workload contract and planner | `server/experiments/operational-qualification` (`plan`) | Validates a workload manifest and expands it into a deterministic schedule: every offer with sender, room, recipients, due time and deadline | Sends nothing. It has no network client |
| Observation accounting | same crate (`account`) | Takes the manifest and supplied observations (per-offer send times, per-pair receipt times) and classifies every expected pair as on time, late, timed out, censored or not yet offered, with generator lag | Collects nothing. It reports no percentile and no pass threshold, and always sets `qualification: "not_assessed"` |
| Baseline fanout profile | `server/experiments/operational-qualification/examples/baseline-fanout.json` | One room, two alternating senders, ten recipients, one 512-byte offer per second | It is an accounting exercise, not an agreed workload |
| Evidence template | `server/experiments/operational-qualification/BASELINE-EVIDENCE.md` | Lists what a run must record: source and image identity, hardware, topology, placement, replication mode, collector health, resource readbacks | Every field is "unverified" |
| Isolation declaration, host controller, canary | same crate (`local-isolation`), `host-controller/`, `host-canary/` | Validate and build the confinement for the #2106 environment | Build-only. No guest has booted and no environment is provisioned |
| Server metrics | [`docs/operations/server-metrics.md`](../operations/server-metrics.md) | Server-side latency and backlog: `xmpp.muc.fanout.latency`, `xmpp.muc.broadcast_build.duration`, `xmpp.sm.request.latency`, `ingress.tx.duration`, `waddle.ingress.entry.preparation.duration`, `http.server.request.duration`, the queue and custody gauges | Client-observed latency; memory |
| Test harnesses | `waddle-ws-test-support` (`TestServer`, `WsXmppClient`), `tests/cluster_support` | Start real server processes, one or two nodes, and drive WebSocket clients | Unsuitable for budgets: accounts are deleted and recreated at each start and the permission backend can be the test one, which the experiment's own README rules out for restart and durability evidence |

`docs/ci-performance.md` and `docs/ci-cache-benchmarks.md` measure CI
duration, not server workload. No crate has a benchmark target.

## What is missing

1. **An execution adapter.** Nothing connects clients, sends the planned
   offers and records receipts. The experiment forbids writing one before
   its gates are reviewed (`README.md`, "Gates before any execution adapter
   or operational run": isolation and authority, stable account and storage
   identity, a replication-aware durability oracle, and the rest of that
   list). This plan does not add one.
2. **The environment.** #2106's isolated two-replica environment is not
   provisioned.
3. **Scenarios the manifest cannot express.** The schema has rooms with
   senders and recipients, fixed-interval offers and `retries: "disabled"`.
   It has no connection events, so a reconnect storm cannot be planned, and
   no delivery kind, so a multi-device direct-message flow can only be
   represented by mapping a "room" to a recipient group in the adapter.
4. **Percentiles, memory and recovery time.** The accounting report keeps
   every pair's latency and outcome but computes no percentile. Nothing
   captures memory or the time from a fault to restored delivery.

## Scenarios

Each scenario needs a manifest (the offered workload), a placement (which
node each client connects to and which node owns the room), and the
recordings in the next section. Placement is not part of the manifest: the
adapter applies it and the run must record the node each client actually
connected to and the room's actual owner, because planned placement does
not prove a route.

| # | Scenario | Manifest | Placement | Expressible today |
|---|---|---|---|---|
| S1 | Same-node room flow | A room manifest of the shape of `baseline-fanout.json`; sizes to be fixed by the owner | Every client and the room owner on one node | Planning and accounting: yes. Execution: no adapter |
| S2 | Cross-node room flow | The same manifest as S1, unchanged, so the two are comparable | Senders on the node that does not own the room; recipients split across both nodes | As S1 |
| S3 | Same-node device flow | One recipient group per account: its devices are the recipients, one correspondent is the sender | All devices of the account on the account-owner node | Planning: yes, with the group standing for the account. The adapter must send direct messages, not groupchat; the schema does not say so |
| S4 | Cross-node device flow | As S3 | Devices of one account split across both nodes | As S3 |
| S5 | Hot-room fanout | One room at the size the owner fixes, within the schema's bounds (1,000 clients, 10,000 offers, 100,000 expected pairs, 64 MiB offered payload) | Recipients split across both nodes; run once with the owner on each side | As S1 |
| S6 | Reconnect storm | Not expressible: needs planned disconnects and reconnects, and stream-management resumption or fresh bind per client | Clients on both nodes; the storm caused by a node restart and, separately, by client-side disconnects | No. Needs a schema extension and an adapter |
| S7 | Recovery after node loss | S2's manifest running while one node is killed | As S2 | Planning and accounting: yes, for delivery around the fault. The fault itself needs the #2106 environment's verified process identities |

For every scenario the mixed configuration matters: the committed topology
is two replicas at 1 CPU and 4 GiB each with clustering enabled, two
PostgreSQL instances and the `Recreate` strategy
(`infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml`).
Results from one replica, or from chart defaults, do not stand in for it.

## What each run records

Use `BASELINE-EVIDENCE.md` as the record. In addition to its fields:

| Quantity | Source | Notes |
|---|---|---|
| Hardware, kernel, CPU and memory limits as configured and as read back | The #2106 controller's resource reader and guard | Configuration is not readback, and readback is not enforcement under pressure; the template keeps the three apart |
| Source SHA, image digest, build features, effective server configuration | The run's image and rendered manifests | The handoff's `release_git_sha` and the pod's `WADDLE_GIT_SHA` identify the build |
| Offered and actual send time per message; generator lag | Adapter observations, accounted by `account` | Lag above the manifest's budget makes the run inconclusive; the offered time is never moved |
| Protocol receipt latency per expected pair, from the fixed offered time | `account` report, `measurement` phase | Report p50, p95 and p99 over all expected pairs of the phase, with late, timed-out and censored pairs counted in the denominator and reported beside them. A percentile over successful pairs only is not acceptable |
| Server-side latency | `xmpp.muc.fanout.latency`, `xmpp.muc.broadcast_build.duration`, `ingress.tx.duration`, `xmpp.sm.request.latency` histograms for the run interval | Compare with the client-observed figure; a gap is queueing outside the instrumented span |
| Memory | Per pod: cgroup `memory.current` sampled through the run and `memory.peak` at its end | Record idle, steady and peak; attribute to the pod, not the node |
| Backlog and custody at start, during and after | `ingress.effects.unresolved`, `xmpp.push.queue.parked`, `waddle.room_effect_outbox.depth`, `waddle.muc.destroy_completion.depth`, `xmpp.remote_owner_mirror.pending`, the CNPG ingress gauges | A run that ends with custody it did not start with has not drained |
| Recovery time (S6, S7) | Time from the recorded fault to the first offer whose every pair is on time, and to the last late or timed-out pair | Also record `xmpp.sm.resume.results` by outcome, `waddle.clustering.claim.mutations`, and readiness of each pod |
| Duplicates | `account` report | A duplicate receipt is kept, not folded into success |
| Observer health | Adapter and collector readiness and coverage | Collector failure or generator saturation makes the affected evidence inconclusive (#2106) |

Each scenario is run at least three times with the manifest's own
repetitions and cooldown; the record keeps every run, not the best.

## Commands

These exist and are offline. From
`server/experiments/operational-qualification`:

```sh
# Expand a manifest into its schedule
cargo run --offline --locked -- plan examples/baseline-fanout.json > plan.json

# Account for observations an adapter recorded against the same manifest
cargo run --offline --locked -- account examples/baseline-fanout.json observations.json > accounting.json

# The contract's own tests
cargo test --offline --locked
```

`observations.json` is an `ObservationSet`: the manifest embedded verbatim,
`closed_at_ms`, one outcome per offer and the receipt timestamps
(`examples/synthetic-observations.json` shows the shape with synthetic
data).

There is no command that runs a workload, locally or anywhere else, and
none was added: the repository has no harness that sends a planned
workload to a server, and the experiment's gates come before one is
written.

## Order of work

1. The owner fixes, for S1 to S5, the manifest sizes and, for each recorded
   quantity, the budget it is compared against.
2. The #2106 environment is provisioned and its isolation verified.
3. The adapter's gates are reviewed, then the adapter is written against
   the existing `ObservationSet` contract, with placement recorded from
   what the server reports.
4. The manifest schema gains connection events for S6, and a delivery kind
   if S3 and S4 are to be planned without an adapter-side convention.
5. A post-processing step computes the percentiles from the accounting
   report, with the full denominator.
6. S1 to S5 are run on the committed topology and recorded; then S6 and S7.
7. The recorded figures become the budgets the failure matrix and the
   release evidence refer to.
