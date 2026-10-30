# PostgreSQL grant-fence first-stage experiment

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
| SpiceDB-canonical variant and P/S comparison | **PENDING** |
| Unknown accepted external writes and durable recovery | **PENDING** |
| PostgreSQL restart, restore, graph loss/rebuild | **PENDING** |
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
