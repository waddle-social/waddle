# Offline operational qualification contract

This standalone, unpublished experiment implements the first offline slice of
#2046 W1.8b: validate a typed manifest, expand a deterministic workload plan, and
account for supplied observations. It has its own workspace and lockfile and no
production consumers. It contains no network client, server launcher, credential
handling, provisioning, process signalling, fault injection, or execution
adapter. The examples contain synthetic observations, not measured evidence.

## Commands

From this directory, using an installed repository-compatible Rust toolchain:

```sh
cargo run --offline --locked -- plan examples/small-run.json > /tmp/waddle-plan.json
cargo run --offline --locked -- account examples/small-run.json examples/synthetic-observations.json > /tmp/waddle-accounting.json
```

Both commands read bounded local JSON files and emit JSON to stdout. Each input
is limited to 8 MiB. Unknown fields, malformed JSON and unknown enum variants
are rejected; input contents and parser excerpts are never printed on errors.
The schema has no free-text values, addresses, account names, endpoints, tokens,
or secrets. Numeric client/room/run IDs are synthetic references only. Never add
credentials or real user data to manifests or observation artifacts.

An exit code of zero means contract processing succeeded. The result always has
`qualification: "not_assessed"`. `accounting_status: "complete"` means collection
covered the entire planned duration and generator observations were complete and
within budget; it can still contain late pairs, timeouts, and duplicate receipts.
It is not a performance, durability, UI visibility, or production qualification.

## Frozen offered workload

The manifest has no defaults: it records the schema/run IDs, declared client
count, ordered room/sender/recipient lists, payload bytes, offer interval,
warmup/measurement/drain durations, delivery timeout, generator lag budget,
retry policy, repetition count and cooldown. `retries` only accepts `disabled`.
Every message has one offered send; no reconnect or message retry is inferred.

- A repetition starts at time zero, then runs warmup, measurement and drain.
  Subsequent repetitions start after the previous drain and configured cooldown;
  there is no cooldown after the final repetition. No offers occur during drain
  or cooldown. Every phase is at most one hour; total duration is at most one day.
- Offers occur at `start + sequence * offer_interval_ms`, independently of all
  observations. Warmup and measurement must be exact multiples of the positive
  interval. Warmup may be zero; measurement must contain at least one offer.
  An offer exactly on the warmup boundary belongs to measurement; the active
  end boundary is excluded. Sequences start at zero in each repetition, whose
  ID starts at one. Repetitions repeat the same distribution and phase schedule.
- The ordered room list is used round-robin. On each visit to a room, its ordered
  sender list advances round-robin. Recipients are the room's exact configured
  list, including the sender only when explicitly listed. IDs are unique within
  each list and range from 1 through `client_count`. This is a workload contract;
  it does not infer XMPP membership, echo semantics or authorized room access.
- Every planned message records its payload size, sender, room, recipients, due
  time and deadline. Each recipient-message pair is expected from the outset.
  Deadlines are `due_at_ms + delivery_timeout_ms`, never actual-send time plus
  timeout. Drain must be at least the timeout, covering every planned deadline.
- Hard bounds are 1,000 clients, 100 rooms, 10 repetitions, 10,000 offers,
  100,000 expected pairs, 65,536 bytes per payload, 64 MiB offered payload and
  256 MiB recipient payload across all phases/repetitions. These are payload
  accounting limits, not estimates of wire, storage, replication, or billing
  cost. No workload is sent by this executable.

The plan embeds the validated manifest and total duration/offers/pairs/payload
bytes. JSON serialization is deterministic for the same manifest. List order is
intentional workload data; reordering a room or sender list changes the plan.

## Observation and result contract

`ObservationSet` embeds the complete matching manifest, a collection close time,
offer outcomes (`sent` or `not_sent`) and protocol receipt timestamps. The exact
typed manifest must match, preventing accidental accounting against another
payload, schedule, distribution or run ID. This binding does not authenticate
the supplied observations. All times
are integer milliseconds from the same monotonic run origin; collection close
cannot exceed the planned duration. Offer observations are unique per message.
Receipt observations must reference an expected pair and a recorded send, and
cannot precede that send or exceed collection close. Duplicate receipts remain
visible, with all their timestamps sorted. Input is bounded to one offer per
message and four times the expected pair count in total receipt observations;
larger observations are rejected, never truncated or reported as passing.

Accounting retains a result for **every** expected pair, including warmup and
future offers. The earliest recorded receipt defines the pair outcome:

| Outcome | Meaning at collection close |
| --- | --- |
| `on_time` | Receipt at or before the fixed deadline, inclusive. |
| `late` | Receipt after the fixed deadline. It does not become an on-time success. |
| `timed_out` | No receipt and the deadline has elapsed, inclusive. |
| `censored` | Offer is due but collection closed before its deadline. |
| `not_yet_offered` | Collection closed before the fixed offer time. |

Per-message generator outcomes and per-pair outcomes are separate. A missing
send observation or explicit `not_sent` makes accounting inconclusive, even when
the associated pair deadlines elapsed. A recorded send retains its scheduling
lag (`actual send - due`); lag above the manifest's explicit budget makes the
whole run inconclusive. The example budget is zero. Any early collection close
also makes accounting inconclusive. No offer, deadline, distribution or latency
origin is moved to accommodate a slow generator.

Observed latency is named `protocol_receipt_latency_ms` and measured from the
fixed offered time, including generator lag. No UI-render, user-visible or
durable-storage latency is inferred. There are no success-only percentiles or
pass thresholds. `all_phases` and `measurement` summaries retain the full
expected-pair denominator, including late, timed-out, censored and future pairs;
their five outcome counts sum to `expected_pairs`. Warmup results stay in the
artifact but are excluded from the measurement summary. Exact pairs, generator
times, fixed deadlines and duplicates remain inspectable in the same report.

The sample has eight expected pairs: six in measurement, with three on time,
one late, two timed out, and one duplicate receipt. This demonstrates accounting
of synthetic data; it supplies no evidence about Waddle delivery behavior.

## Gates before any execution adapter or operational run

The owner selected a local isolated environment for later qualification. No
manifest here selects or authorizes a target. Before implementing or running an
adapter, review and enforce all of these gates:

1. **Isolation and authority:** choose the environment, ownership, allowed
   endpoints/processes and fault scope; provision dedicated accounts, rooms,
   namespaces, database/storage and authorization state. Enforce that isolation
   in the runner and independently verify it before any mutation. A label or
   run-ID prefix alone does not establish isolation. Define a bounded resource,
   duration/cost budget, abort conditions and cleanup scope; fail closed when
   prerequisites are absent. Credentials must remain outside artifacts.
2. **Stable account/auth/storage identity:** use production-like authentication,
   authorization and persistent account/storage identity across measured
   restarts. The existing `TestServer` boot-time seeding deletes/recreates
   accounts and may use a test permission backend; using it unchanged cannot
   establish restart/durability behavior. Map synthetic IDs to stable identities
   outside these artifacts, with an independently reviewed mapping contract.
3. **Replication-aware durability oracle:** select and record actual database
   topology and synchronous/asynchronous replication/commit settings; inspect
   WAL positions, replication progress and timeline changes around failure.
   Distinguish client offer/send, protocol acceptance/SM acknowledgement,
   primary commit, replica durability, recipient protocol receipt and replay
   recovery as separate milestones. A receipt or an SM acknowledgement alone
   does not prove failover durability. Define expected pairs and reconciliation
   from durable authority for each scenario before injecting any fault.
4. **Real deployment comparison:** a one-versus-two-server-replica comparison
   requires actual identified deployments and real rolling replacement under
   the same frozen workload, not only in-process fixture counts. Record image,
   features/config, topology, rollout controller behavior and resource limits.
   Distinguish server replication from database/storage replication and hardware
   fault domains. The offline plan cannot qualify rollout or failover safety.
5. **Measurement contract:** implement authenticated WebSocket/XMPP behavior
   using the applicable protocol types/specs, exact message correlation,
   observer completeness, clock/monotonic-origin mapping, connection/room setup,
   receiver readiness, backpressure and fixed-offer scheduling. Define separate
   measured UI-visibility and durability oracles if those claims are desired.
   Treat generator saturation, observer loss or an uncertain fault boundary as
   inconclusive; record all expected outcomes rather than dropping samples.

No execution adapter, deployment, fault, or qualification is included in this
slice. The gates above remain unresolved and block operational claims.

## Verification

```sh
cargo fmt --check
cargo test --offline --locked
cargo clippy --offline --all-targets --locked -- -D warnings
```

Tests cover invalid/bounded manifests and strict schemas, an exact repeated
schedule/distribution, deterministic serialization, complete expected-pair
denominators, inclusive deadline boundaries, late and duplicate receipts,
censoring, missing/not-sent/lagged generator observations, warmup exclusion,
invalid observation correlations and timestamps, and observation-order stability.
These tests prove local contract/accounting behavior only.

The standalone `operational-qualification` workflow runs these checks on
matching pushes and on merge-group candidates without publishing. It is not part
of the required integration aggregate: fork pull requests do not receive this
push check in the base repository, and merge-group execution depends on the
workflow being present in the candidate. Hosted execution remains separate
evidence from the local results.
