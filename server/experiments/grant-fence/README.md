# Grant-fence and canonical leaf recovery experiments

This standalone, unpublished Linux test workspace is excluded from Waddle's
production workspace and images. It implements a disposable experiment, not a
production authority or worker. The prescience client is pinned to 0.1.0 with
the production TLS feature; the complete dependency resolution is locked here.

The CI task invokes `bash experiments/grant-fence/run-native.sh` from `server`.
From a clean committed checkout, the same complete command is:

```sh
bash server/experiments/grant-fence/run-native.sh
```

The runner enters a pure native Nix shell using the repository's `flake.lock`
inputs, PostgreSQL 17.11, SpiceDB 1.56.1, and the host Rust toolchain version and
components in `server/rust-toolchain.toml`. It runs format, full experiment tests,
and all-target Clippy with warnings denied. Database processes run on the
unprivileged Linux host outside Nix build sandboxing; Unix/loopback sockets and
`/proc` must be available. Package realization and initial Cargo downloads need
network/cache access. No container or production service is used.

For development without Nix, run from this directory with the repository's Rust toolchain and `protoc` on the
path (or set `PROTOC`). Install native PostgreSQL 17.11 (including `initdb`) and
SpiceDB 1.56.1, then set the following explicit paths; there is no host-specific
store-path fallback or binary lookup through inherited PATH:

```sh
export FENCE_POSTGRES_BIN=/absolute/postgresql-17.11/bin
export FENCE_SPICEDB_BIN=/absolute/spicedb-1.56.1/bin/spicedb
export FENCE_TEST_SHELL=/absolute/bash/bin/bash
export FENCE_TEST_SLEEP=/absolute/coreutils/bin/sleep
```

`src/binaries.rs` requires absolute existing paths and attests the exact versions
of `postgres`, `initdb`, and `spicedb` before starting a fixture. The process
cleanup regression also requires explicit shell/sleep executable paths; the Nix
runner supplies pinned Bash and coreutils paths. A missing path,
version mismatch, or unavailable prerequisite fails;
there are no ignored tests, container dependencies, inherited database URLs, or
mocked store acknowledgements. Loopback/Unix socket permission is required.
All database settings are generated internally; these binary-path variables do
not permit supplying a DSN, database directory, or credential.

```sh
cargo test --locked -- --nocapture
cargo clippy --all-targets --locked -- -D warnings
```

The explicit CI/formal-evidence invocation additionally rejects dirty or
untracked source:

```sh
FENCE_REQUIRE_PINNED=1 cargo test --locked -- --nocapture
```

Development runs record their exact Git HEAD, byte count and full SHA-256 digest of dirty/untracked status, and a
SHA-256 fingerprint of this workspace's manifest, lockfile, sources, schemas,
tests, README, and native runner. Status paths are not retained or printed.
Status stdout is drained while Git runs, with a five-second execution deadline
and a 16MiB input cap; larger output fails closed without a pipe-buffer deadlock. They label formal evidence false until the tree is committed
and clean. Missing provenance, source changes during a run, or cleanup failure
fail the run. A prototype pass does not establish a completed F0 gate.

The fixture creates only generated 0700 directories under `/tmp`. PostgreSQL
listens on a private Unix socket; application and SpiceDB databases have separate
non-superuser owners and deny each other's connections. SpiceDB binds gRPC only
to loopback and disables other listeners and outbound telemetry. Generated
credentials travel through anonymous stdin pipes or the SpiceDB child's
inherited environment, never files, arguments, or evidence. PostgreSQL statement
logging and native process diagnostics are disabled. Only PostgreSQL's password
verifiers exist in the disposable database. The fixture terminates worker and SpiceDB process groups before reaping their
leaders, avoiding signals to reused PGIDs. PostgreSQL uses bounded fast shutdown:
the postmaster must exit normally after coordinating and reaping its separately
grouped backends. There is no SIGKILL fallback for PostgreSQL. Any unverified
shutdown permanently retains that private directory, including on later Drop.
The first cleanup failure is retained as bounded typed service/phase/errno/status
metadata in a shared fixture-owned latch and evidence; workers, initdb,
migration, services and fixture version probes attach it before postspawn
fallible work or stdin. Explicit stop and Drop record into the same sticky
latch; later successful service cleanup cannot clear it, and mutex poison is
uncertainty. At a PostgreSQL deadline, a separate diagnostic scan observes only
the owned postmaster and direct children: up to 64 PID/PPID/start-tick/state and
validated kernel wait-symbol records, with a cooperative 100ms scan deadline checked between bounded file reads
(individual kernel reads are not a hard real-time guarantee). Truncation/read uncertainty is explicit and never changes the original
failed shutdown. The normal PostgreSQL shutdown budget is 30 seconds: engineering margin after
three measured single-workload stops of 3570/3651/3740ms and a separate parallel
shutdown observation blocked in filesystem synchronization beyond five seconds
with eventual valid shutdown. This is not a proven upper bound or an explanation
of the older failure whose first cause was lost. Forced 200ms timeout tests and
the diagnostic-only five-second threshold remain unchanged. No process arguments, environment or output
are captured. Linux ESRCH from a proc stat read means the task exited between
open and read, and is treated as vanished. A real opened-FD regression covers
that race; unrelated I/O failures and EACCES/EPERM still fail closed as typed
uncertainty. The runner therefore requires process-stat visibility sufficient
to verify cleanup; inaccessible tasks are never assumed to have disappeared.
Before reporting success, the fixture scans stopped data for exact plaintext
credential bytes and removes it. Cleanup regressions cover an exited worker
leader with a live descendant, injected supervisor failure followed by Drop,
and a real stopped PostgreSQL backend in a separate process group.

Each proof contender is a separate instance of the test-only worker executable.
Nonsecret barrier files determine schedules; `pg_blocking_pids` establishes exact
lock dependencies rather than timing. Final admission holds `FOR SHARE` through
an admission-ledger insert; revoke holds `FOR UPDATE` through closing and
advancing the guard. Real, fully consistent SpiceDB checks happen outside those
locks. The single-edge projection identity includes scope, fresh incarnation,
and generation. The fixture's trusted setup advances closed generations and
uses fresh incarnations; it is not a production lifecycle API.

Reports are retained in generated 0700 `/tmp/waddle-fence-evidence-*` directories.
A bounded typed summary is also printed to CI stdout (at most 64KiB), retaining
the exact revision/source digest, source cleanliness, versions, cases, pending
work, cleanup result, and static passed/failed outcome after the runner ends.
The summary excludes dirty-status text and dynamic error chains, and both saved
evidence and the printed summary are rejected if any exact generated credential
bytes occur. They contain nonsecret barrier observations and the following explicit scope:

| Evidence | First-stage status |
| --- | --- |
| Real check-then-revoke negative control | Implemented; unsafe admission is the expected control |
| Guarded check then revoke | Implemented |
| Admission-first and revoke-first row-lock schedules | Implemented, exact PostgreSQL blocker observations |
| Old projection publish after regrant | Implemented, old graph really allowed but old guard denied |
| Same generation with fresh incarnation | Implemented bounded ABA schedule; full lifecycle closure pending |
| Lock-holder process death and fresh contender | Implemented; uncommitted effect rolls back |
| Independent scope during a held lock | Implemented |
| PostgreSQL lock timeout | Implemented, explicit SQLSTATE and no effect |
| SpiceDB outage and same-database process restart | Implemented; checks fail closed, graph persistence checked |
| Bounded canonical leaf P/S comparison | Implemented in #2051; full production authority selection/recovery remains **PENDING** |
| Lost external write response | Bounded leaf caller-deadline scenarios below; production unknown-write recovery remains **PENDING** |
| PostgreSQL restart, restore, full graph loss/rebuild | **PENDING**; controlled closed leaf/marker removal is covered separately |
| Moves, all lifecycle writers, schema/dependency drift | **PENDING** |
| Multi-edge graphs, actor/claim fences, codec migration | **PENDING** |
| Frozen room-child acceptance and sink/byte drain | **PENDING** |
| Production closure and end-to-end federation | **PENDING** |

Admission is an exact committed ledger record in this experiment. A pre-fence
admitted attempt may finish after acknowledgement. Nothing here proves that no
bytes are emitted afterward, nor does it alter Waddle's existing frozen-child
acceptance contract. SpiceDB process persistence is not PostgreSQL restart or
unknown-write recovery. The pinned native SpiceDB version is not automatically
equivalent to the deployment operator's `stable` channel.

For a separate shutdown timing diagnosis, run inside the pinned native shell:

```sh
FENCE_MEASURE_POSTGRES_SHUTDOWN=1 cargo test --test postgres_guard --locked -- --nocapture
```

This opt-in mode runs the actual proof workload, verifies `fsync`,
`synchronous_commit`, and `full_page_writes` remain enabled in fresh application
and SpiceDB-role sessions, and records only the numeric filesystem kind.
Connection establishment and the combined durability query/close each have an
explicit two-second client deadline; a connected SIGSTOP backend regression
verifies that the query cannot wedge shutdown. A
dedicated controller uses a separate five-second PostgreSQL diagnostic threshold
exactly once (the normal cleanup budget is thirty seconds),
recording time immediately before its successful SIGINT and at observed normal
exit before reap. If that deadline expires it remains a failed, retained run;
the controller only observes afterward, up to thirty seconds total from the
first signal. It sends no further signals during observation and never clears
the failure latch. Normal cleanup/Drop semantics outside that controller are
unchanged. Healthy fixtures are removed only after verified process stops and
credential scans; failed/unverified data stays private.

The separate `shutdown-measurement.json` and bounded stdout timing describe
observed elapsed time, not crash-recovery correctness or formal proof. Kernel
wait symbols and whitelisted numeric owned-process IO counters are diagnostic
observations. These measurements do not retroactively identify the cause of
any earlier failure whose first cause was not recorded.

## Canonical authority recovery comparison (#2051)

The runner preserves the original nine PostgreSQL guard scenarios and adds 23
bounded recovery scenarios. This compares two leaf models; it does not select
a production authority. Evidence independently names AdmissionClosed,
CanonicalMutationCommitted, and ProjectionVerified, each grouped under the full
candidate/scope/incarnation/generation/operation/digest identity. Facts describe
what was observed, including historical commitment before deliberate corruption;
they do not assert that corrupted canonical data remains present. Verification
is separate from exact SQL activation.

The PostgreSQL candidate stores the complete subject, relation, and grant/revoke
polarity in its authoritative SQL journal. A fresh recovery process loads that
exact current journal record before rebuilding. The SpiceDB candidate stores no
canonical payload in SQL, and its fresh Recover request carries only identity,
digest and pause control. It obtains payload from real SpiceDB. Both the marker
and grant or positive revoke tombstone are committed in one acknowledged atomic
write. Verification reads the whole bounded object in one FullyConsistent RPC,
requires equal read_at revisions for every row, validates the two exact
relationships and reconstructed digest, and never infers revoke from absence.
The subsequent permission check uses that same exact snapshot.

| Additional scenario | Evidence and boundary |
| --- | --- |
| Grant and revoke during SpiceDB outage, both candidates | Both close SQL admission; only PostgreSQL commits canonical intent during outage. No verification or success is inferred from failure. |
| Closed marker loss and closed tuple/tombstone loss independently | PostgreSQL rebuilds from its exact current SQL journal; SpiceDB remains closed with canonical state missing. |
| Worker death after acknowledged write, grant and revoke | Fresh process verifies committed marker+leaf. This is explicitly **not** a lost RPC-response test. |
| Stale activation before and after newer revoke commit | Exact PostgreSQL blocker observation or postcommit CAS rejection; a pending closed operation can be superseded. |
| Activation completes before revoke begins | It may activate first; the subsequent newer revoke closes it. |
| Healthy independent scope while mutation lock held | Actual mutation and admission complete despite another scope's held exclusive lock. |
| ACTIVE canonical deletion after allowed check | Expected unsafe negative control: SQL guard alone permits admission when canonical state is deleted without closing it. |

Loss injection always observes an exact committed closed fence first, except
the explicitly labeled ACTIVE negative control. The test does not establish
arbitrary active-corruption detection, production lost-response recovery, multi-edge
closure, schema immutability, restore safety, production writer/actor closure,
accepted room-child behavior, final byte draining or federation readiness.
The accepted-child contract needs separate tests through actual ingress/replay
code. Global SpiceDB outage denies protected reads for both candidates;
independent-scope progress is demonstrated only with healthy SpiceDB.


## Lost write response and caller deadline

The same native runner adds twelve leaf scenarios and one inverted-order negative control to the original 32. Each uses
an owned, supervised proxy process listening only on an ephemeral loopback port.
It accepts one authenticated `WriteRelationships` request, forwards its exact
bounded protobuf bytes, and lets tonic validate the actual backend unary body
and final successful gRPC status. Only then does the proxy record
`BackendAcknowledged`; it withholds that success from the mutation worker.
The control records `RequestNotForwarded` without invoking the backend write.
An independent real read verifies presence or absence of the exact marker/leaf.
Unauthenticated, unsupported-method and oversized requests are rejected before
the one accepted write; all twelve positive schedules exercise those receiving boundaries.

The writer captures an absolute two-second `CLOCK_MONOTONIC` deadline before
starting the write and arms that exact deadline using Linux timerfd with
`TFD_TIMER_ABSTIME`; recording a barrier never restarts the duration. The
prescience endpoint timeout is ten seconds. Clock reads, arithmetic and timer
setup fail closed. Evidence requires
`CallerDeadlineExceeded`, not merely an unavailable service or a worker killed
after acknowledgement. This is a local elapsed deadline, **not** a claimed
remote gRPC `DeadlineExceeded` status: pinned tonic 0.12.3 maps its own transport
timeout to `Cancelled`. Unexpected gRPC statuses are recorded separately and
fail these scenarios. The controller releases the proxy only after the worker
has exited with that local deadline and without acknowledgement, verification
or activation. The proxy's eventual error is not the caller's observed result.
The independently timestamped proxy fault observation must fall between the
writer's armed time and its exact absolute deadline, strictly before expiry.
The writer records actual timerfd expiration on the same shared monotonic clock.
Checking absence of a completion file is not accepted as ordering evidence. The proxy's own waits,
wire messages (8KiB), readbacks and native process cleanup are bounded; raw
requests, metadata, credentials and responses never enter files or reports.
It uses the existing stdin-only credential transfer, process-group ownership,
cleanup latch and report secret scan. This is a fault fixture, not a production
proxy or an external-write recovery service.

Both candidates are tested for grant and revoke with committed-write response
loss and never-forwarded controls. The SQL guard stays closed throughout the
uncertainty. A separate healthy scope mutates and admits while the successful
response remains withheld. Fresh recovery processes receive only identity and
digest. PostgreSQL reconstructs from its canonical SQL intent in either control;
SpiceDB recovers committed marker/leaf data after response loss and stays closed
when the request never arrived. Neither candidate derives success from timeout.

Four additional schedules commit a newer revoke while the old reply is withheld,
then deliberately republish the exact older marker/leaf and run fresh recovery.
The old operation remains stale and cannot admit; the newer revoked guard stays
closed. This demonstrates bounded operation identity and activation fencing,
not a production retry protocol. Reports distinguish proxy evidence, caller
failure, historical canonical facts, recovery outcome and the newer identity.
No production writer closure, arbitrary corruption recovery, PostgreSQL restart,
restore safety, inherited/multi-edge policy, accepted-child contract change,
byte draining or federation readiness is established by these cases.


The inverted-order control deliberately pauses a real worker after its local
absolute deadline but before its `done` observation. Only then is the proxy's
one owned request task allowed to forward the write. The real backend commits,
the marker/leaf is independently read back, and `done` is still absent, yet the
monotonic ordering check must reject that schedule. The controller later releases
the worker and recovers the actual committed state in a fresh process. The report
labels this `AfterCallerDeadline` with `ordering_accepted: false`; it is not a
passing pre-deadline response-loss observation. The bounded upstream task belongs
to the supervised proxy process and survives downstream cancellation for this
control. Explicit process-group stop and the sticky cleanup latch cover that
runtime and all its tasks; no detached task can outlive verified process cleanup.
