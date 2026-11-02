# Local baseline evidence template

This template and [baseline-fanout.json](examples/baseline-fanout.json) prepare
an offline smoke profile for #2106 and #2046 W1.8b. They do not provision an
environment, start a workload, establish isolation, or measure Waddle. Record
missing evidence as **unverified**. A completed template is a record of supplied
evidence, not an authorization to execute or an automatic qualification result.

## Frozen fanout profile

The profile has 12 synthetic clients in one room: clients 1 and 2 alternate as
senders within each repetition, starting with client 1 each time (76 and 74
offers respectively). Clients 3 through 12 are the expected recipients. Sender
echoes are not included in the expected pairs. Each repetition has 15 seconds
of warmup, 60 seconds of measurement and 5 seconds of drain, with one 512-byte
offer per second and no retries. Two repetitions have a 10-second cooldown.
The delivery timeout is 5 seconds and the maximum generator lag is 100 ms.
These values define the accounting exercise; they are not agreed latency or
capacity budgets. This small profile does not represent a reconnect storm, a
mixed-room workload, a slow receiver, or a qualified hot-room capacity test.

From this directory, expand the fixed schedule without opening a network socket:

```sh
cargo run --offline --locked -- plan examples/baseline-fanout.json > /tmp/waddle-baseline-plan.json
```

The plan totals are 170 seconds, 150 offers, 1,500 expected recipient-message
pairs, 76,800 offered payload bytes and 768,000 recipient payload bytes. Warmup
accounts for 300 pairs and measurement for 1,200. Payload totals are not wire,
storage or replication budgets. The existing `account` command consumes supplied
observations; the checked-in synthetic example is not evidence of a Waddle run.

## Candidate record

Complete these fields for a future run and attach the corresponding independently
captured evidence. Record no credentials, tokens, real user data or production
endpoints in the plan, observations or this record. Synthetic client IDs are not
the account mapping itself. Keep the stable synthetic identity mapping and its
access controls in a separate run-owned location.

- Candidate source SHA and clean/dirty tree state; exact image digests, build
  features, configuration hashes, artifact provenance and dependency closure:
  **unverified**.
- Run identity, timestamps, hardware and architecture, host and guest kernel,
  clock origin and monotonic-clock mapping, and the exact workload file and hash:
  **unverified**.
- Run-owned guest, cluster CA, namespace UID, deployment UUID, pod UIDs,
  container IDs and process identities; stable synthetic accounts, room, key,
  authorization and storage lineage across restarts: **unverified**.
- Real local OIDC authentication, SpiceDB authorization and an authorization
  denial control, receiver readiness and persistent identity across restart:
  **unverified**.
- Intended application topology: two Waddle replicas with four enrolled cluster
  keys and 1 CPU / 4 GiB limit per replica, two application PostgreSQL instances,
  SpiceDB with a separate store, shared uploads, local OIDC and telemetry. Actual
  placements, startup and effective configuration: **unverified**.
- Observed authenticated WebSocket connection node and room owner, including
  relevant incarnation or authority epoch, for each measured path: **unverified**.
  Planned client placement alone does not prove a same-node or cross-node route.
- Database replication mode and commit settings, primary/standby identities,
  WAL and timeline progress, and application-level reconciliation:
  **unverified**.
- Collector and receiver readiness, observation coverage and health, all planned
  offers and expected pairs, generator timing, late results, timeouts, duplicates
  and collection close: **unverified**.
- Actual transition configuration, resource peaks including any surge, fault
  identities and abort/cleanup evidence: **unverified**.

The committed [deployment configuration](../../../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml)
specifies `Recreate`, not a rolling strategy. Record and qualify the actual
transition. A future rolling test is a separate local deviation, with its own
review and surge budget; it is not evidence of the committed transition.

## Resource and isolation evidence

Keep these categories separate for every relevant CPU, memory, swap, process,
disk, I/O, log and wall-clock bound:

| Category | Evidence to attach | Status |
| --- | --- | --- |
| Configured | Intended limits and their source, including guest and per-service limits | unverified |
| Observed | Actual host/guest control readbacks and process or cgroup membership | unverified |
| Pressure enforcement | Separately reviewed pressure or timeout test and its observed result | unverified |

Configuration does not prove a control was applied; readback does not prove it
enforces a bound under pressure. Account for the entire workload tree, storage,
logs, temporary files and any deployment surge. The validator's inclusive
qualification range is at most 16 vCPUs, 48 GiB of memory and 128 GiB of disk;
it does not allocate or enforce those resources. The 32-GiB example is synthetic,
not a storage-sizing decision. Check actual provisioning and overhead before
choosing a bound.

Attach reviewed positive local controls and negative probes for the actual
boundary, actual devices/routes and relevant network changes, plus run-owned
process, socket, cgroup and filesystem identities used for cleanup. Inner
network-policy canaries do not prove the outer boundary. Missing, changed or
inconclusive controls make the affected result inconclusive. No production
endpoint, credential or data is a probe target.

## Future qualification inventory

Every item below is **not run for this candidate** until evidence for the exact
candidate and activation configuration is linked. Deterministic barriers and
state-machine tests complement real PostgreSQL role/transaction boundaries and
native multiprocess WebSocket paths; mocks alone do not prove distributed
durability. Include the executed test inventory and actual resume and relay
paths in the required PostgreSQL/multiprocess/WebSocket CI lane. The lane must
fail when prerequisites are absent. Restore hosted CI as described in the
[CI pause plan](../../../docs/planning/2046-ci-pause.md) before release.

The `server/scripts/run-nextest-shards.sh` runner writes
`.ci/nextest-archive/qualification-evidence.json` only after its required-test
check passes. The runner verifies a run-specific Nix store snapshot against the
committed Git tree. It adds the full commit SHA and a fresh 256-bit run ID to
four shard derivations, rejects outputs already present in the local store, and
builds those exact shards through a trusted local Nix daemon with remote builders
and substitution disabled and sandboxing required. The archive and ordinary
build dependencies may be reused from the trusted Nix store or caches, and the
archive build may use configured builders. It checks the shard build identities
and reads all eight JUnit files directly from the resulting store paths.
Diagnostic copies are not evidence inputs.

The atomically written sidecar records the candidate, run ID, source,
derivations, outputs, required-list and JUnit hashes, required outcomes, and
executed inventory. PostgreSQL, multiprocess, real cross-node resume, and
relay/WebSocket anchor tests must be present and pass. It contains no database
URL or report contents. Concurrent runner invocations fail rather than mix
reports. Treat a sidecar as incomplete while the
`.ci/nextest-archive/.qualification-run.lock` directory exists. After an
interrupted run, first confirm that its runner and child processes have stopped.
Then, from `server/`, run `bash scripts/run-nextest-shards.sh --recover-stale-lock`.
Recovery removes any old sidecar before releasing the stale lock; do not remove
the lock by hand. The persistent `.ci/nextest-archive/.qualification-run.guard`
prevents recovery from unlocking a live runner and must not be removed or
replaced. The claim depends on the
integrity of the local Git repository, Nix daemon, store, and host; it is not
an independent remote attestation.
Its `qualification` value is always `not_assessed`; it does not establish image
identity, deployed topology, resource enforcement, workload results, fault
results, or deployment state. A missing sidecar or a failed lane supplies no
test-lane evidence.

| Scenario | Evidence to link | Candidate status |
| --- | --- | --- |
| Concurrent same-full-JID binds | Current binding and successor fencing | unverified |
| Simultaneous resume and cancellation after claim | Single winner and durable compensation | unverified |
| Lost reply after room/SQL acceptance | Authoritative recovery of canonical identities | unverified |
| Origin/owner death around acceptance and SM `h` | Recoverable responsibility and distinct acceptance/delivery | unverified |
| Pending remote acceptance with later work and detach | Responsive control, preserved frontier and generation fencing | unverified |
| Partitioned old owner after takeover | Stale mutation/transport fencing and bounded pump termination | unverified |
| Join/history/subject/live races, ban or room recreation | Required ordering, policy and lifecycle fencing | unverified |
| Full mailbox, blocked writer, exhausted pool or slow room | Bounded resources, overload outcomes and unrelated-user progress | unverified |
| Crash around SM append, write and client ACK | Correct retained/replayed obligations and frontier | unverified |
| Lost relay hint, restart, stale route or owner movement | Reconciliation without duplicate effects or uncertain fallback | unverified |
| Planned and unplanned database failover | Commit outcome, WAL/timeline and application reconciliation across each distinct transition | unverified |
| Real process SIGKILL and committed Recreate transition | Claim/session custody, restart recovery and bounded cleanup on the actual topology | unverified |
| Actor/pump panic, drain or deadline expiry | Explicit cancellation, bounded termination and idempotent cleanup | unverified |
| Retention/GC during pending work | Custody retained until safe retirement and bounded cleanup | unverified |
| Authorization/blocking changes during queued work | Authority-boundary revalidation and account isolation | unverified |

Also qualify offered load, hot-room fanout and reconnect storms against the
agreed workload and budgets, including Kameo remote command admission, retained
requests and spawned tasks. A generator that exceeds its lag budget, a missing
observer/collector, incomplete collection or an uncertain fault boundary makes
the affected evidence inconclusive. A protocol receipt or stream-management
acknowledgement alone does not establish durable or replicated effects.

The exact-release run remains a later gate. A single-host local run does not
establish host-loss resilience, UI visibility, XMPP conformance, zero data loss
or production readiness.
