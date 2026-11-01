# Fixed benign resource inspection candidate

This is a prerequisite investigation for #2106, not a Nix or application runner.
The gated implementation through `7c5f9c4b1dddc69981e64be8f31cf947c6037ce5`
completed independent review for one fixed benign guard under the stated TCB.
Local activation commit `b4ff628025c19e3cc9dd48609bec90b6f97acf74` removes
only its unconditional placeholder gate, with corresponding offline fixture
updates; all actual preflight, budgets, identities and cleanup remain intact.
That activation was reviewed and used once for a failed benign observation;
the saved task evidence is summarized below. The unconditional launch gate was
restored first in `d532f9a072fdf9503dbf39970d837d91015c789f`, before the v262
and reporting fixes. A separately reviewed local activation later observed the
configured limits for one fixed benign guard; kernel reclamation remains
unverified. The implementation in this tree remains non-launching and requires
its own exact activation review. There is no runtime approval flag. The existing
[host controller transport blocker](MANAGER-TRANSPORT.md) is unchanged.

The explicit trusted computing base is the host OS and user systemd manager.
The fixed installed systemd CLI does **not** authenticate the actual manager
socket peer or running manager image. Hostile same-UID processes are excluded.
This narrower trust model is only appropriate for the fixed benign guard.

The no-argument static bootstrap starts the pinned Python interpreter with
`-I -S`, fixed `LC_ALL=C`, no other environment or inherited descriptors above 2,
no new privileges, no core files and no file writes. The installed immutable
configuration selects exactly the guard and two systemd CLI paths. The guard is
static PIE, takes no arguments, closes inherited descriptors above 2 and
reexecutes only its fixed own path with an empty exec environment when needed.
It then raises SIGSTOP. The only operation after SIGCONT is `_exit(0)`; it has no
fork, clone, cgroup write, socket, namespace, Nix or application operation.

The parent generates one fresh 128-bit unit nonce. Pinned systemd-run's
StartTransientUnit mode is `fail`; no collision is replaced or retried. Its fixed
Type=exec unit requests 50% CPU at a 100 ms period, 64 MiB memory, zero swap,
eight tasks, an eight-second runtime, two-second start and one-second stop bound,
Restart=no, KillMode=control-group, SIGKILL termination, no delegation and null
stdio. Actual readback must match, including finite timeout and runtime values.
InvocationID support is required; missing support fails closed.
The cgroup-v2 profile does not request deprecated CPUAccounting: manager v262
omits it from GetAll. CPUQuota/CPUQuotaPeriod requests and strict readback remain,
as do the required cpu controller and actual `cpu.max=50000 100000` check.
Completeness is still mandatory for every requested property; no missing-field
fallback is allowed. The sanitized real v262 transcript in `fixtures/` preserves
the failed timeout state and explicitly replaces its run identities.

Before launch, the parent queries the **current manager's UnitPath**, rather than
using a prior host snapshot or a hard-coded search list. Every reported root,
including runtime, control and early/ordinary/late generator roots, is inspected
for `service.d`, dashed-prefix and exact random-unit `.d`, `.wants`, `.requires`
and `.upholds` paths, plus the exact unit file or alias. Existing matches are
rejected even when empty, dangling or not directories. Resolvable root symlinks
are checked at their canonical target; dangling/looping symlinks, unreadable or
non-directory roots, escaped/ambiguous manager output and excessive paths fail.
A missing ordinary root is accepted only as observed absence below a readable
existing ancestor. The snapshot records traversed directory identities.
The named root-count bound is 64 (the reviewed host reports 37); the independent
8 KiB response bound still applies, so excessive total path bytes are rejected.

The expected user transient directory is `/run/user/<uid>/systemd/transient`.
The supported host profile assumes that the user manager uses
`XDG_RUNTIME_DIR=/run/user/<uid>`, with no temporary/root override. This follows
[systemd v261.2's user transient-directory derivation](https://github.com/systemd/systemd/blob/v261.2/src/libsystemd/sd-path/path-lookup.c#L238-L251),
not a nonexistent manager TransientDirectory property. The program requires that
exact directory in current UnitPath, already present, owned by the user and
openable through a path with no symlinks; otherwise it rejects before launch.
It retains its FD/inode and reopens its path for every subsequent check.

After a known successful launch, the sole configuration exception is the
nonce unit's manager-reported FragmentPath directly below that pinned directory.
Transient must be yes, SourcePath/DropInPaths empty and InvocationID stable. The
fragment must be a same-UID regular non-symlink file with one link, at most 8 KiB,
and unchanged held inode, metadata and SHA-256 content identity. Every drop-in,
dependency and additional match remains forbidden. This accommodates
[systemd's own fragment creation](https://github.com/systemd/systemd/blob/v261.2/src/core/unit.c#L4450-L4488)
without treating a pre-existing or arbitrary file as run-owned.

The fragment may remain after the original service stops. Its deletion occurs
at [unit free/GC](https://github.com/systemd/systemd/blob/v261.2/src/core/unit.c#L720-L731),
not necessarily cgroup pruning. Disappearance is accepted only after the original
stop is confirmed, the original pidfd is dead and the narrow quiescence check
passes. The report distinguishes retention from absence after stop. The parent
never deletes a fragment, including on uncertain launch/cleanup outcomes.

The manager paths and filesystem snapshot are queried again immediately before
launch, before release and before cleanup; observed path or inode drift fails.
This filesystem preflight is **not atomic with launch**. This single fixed benign
run assumes stable manager configuration throughout, in addition to excluding
hostile same-UID processes. It cannot protect a workload from concurrent changes
to manager configuration and does not establish a general service launcher.

Before SIGCONT the parent checks InvocationID, PID/start ticks/pidfd, executable
device/inode, stopped state, UID, empty exec environment, NNP and null FD0..2.
It anchors the cgroup and its original parent directory by retained FDs and
device/inode. Before accepting a leaf identity or absence, it also reopens the
saved parent path through the cgroup hierarchy and requires the original parent
identity; an old parent FD alone cannot detect deletion and recreation.
It checks cgroup v2 membership, domain type, CPU/memory/PID
controllers and actual cpu.max/memory.max/memory.swap.max/pids.max files.
Missing, malformed, unlimited, replaced or unreadable observations fail closed.
No self-report substitutes for these observations.

## Quiescence evidence and unresolved reclamation

RemainAfterExit retains unit metadata, but **does not retain its empty cgroup**:
[systemd v261.2 service.c lines 1428-1431](https://github.com/systemd/systemd/blob/v261.2/src/core/service.c#L1428-L1431)
prunes it in SERVICE_EXITED. An open cgroup.events FD is not assumed readable
after that removal.

Direct `cgroup.events` populated=0 and removal are reported separately as
`events-populated-0` and `removed-guard-dead-reclamation-unverified`. The latter requires the
original leaf to be absent through the retained original parent FD, unchanged
retained and freshly reopened parent identities and a dead original pidfd. A present leaf with
another inode, a live guard, unreadable events in a still-present leaf, or any
identity uncertainty fails. Repeated final observations reject observed reuse.

The earlier v6.18 equivalence argument was wrong for the actual booted kernel.
An independent review identified booted Zen 7.2.11-zen1, source commit
[`793e6d748c1cc142f66350d98eb492686d646192`](https://github.com/zen-kernel/zen-kernel/commit/793e6d748c1cc142f66350d98eb492686d646192).
Its [removal path](https://github.com/zen-kernel/zen-kernel/blob/793e6d748c1cc142f66350d98eb492686d646192/kernel/cgroup/cgroup.c#L6213-L6287)
can skip exiting tasks whose signal live count is zero; its
[`cgroup.events` population check](https://github.com/zen-kernel/zen-kernel/blob/793e6d748c1cc142f66350d98eb492686d646192/kernel/cgroup/cgroup.c#L3814-L3821)
is separate. Removal plus pidfd readiness therefore does **not** prove
populated=0 or complete kernel work/resource reclamation.

The reviewer matched booted vmlinux build ID
`4fac26e2d12ed4afeddad483c4768e7b6774211f` to `/sys/kernel/notes`, matched
configuration to `/proc/config.gz`, and verified relevant Nix outputs and
disassembly. This implementation did not repeat those checks. The external local
handoff is `nix-sandbox-evidence/kernel-review.md` in the task workspace; neither
runtime behavior nor repository checks depend on that external file. This is
build-ID/configuration, signed store-output and recorded derivation evidence, not a full source NAR
rehash/rebuild or hardware attestation.

For this fixed single-threaded, childless guard only, the independent reviewer
accepted the narrower conclusion: original group removed and original pidfd
dead, no further userspace execution, kernel work/reclamation unverified.
Both evidence paths report `userspace_quiesced` and `quiescence_evidence`, with
`kernel_reclamation=unverified`. There is no generic or aggregate
`cleanup_verified` field. Even directly reading populated=0 does not prove all
dying cgroup charges or kernel resources have been reclaimed.

On timeout or interruption after ownership is pinned, the parent verifies the
unit/cgroup/PID identities, kills the **entire** control group, requires the
original pidfd dead plus the narrowly classified quiescence evidence, stops the
unit and checks completion and quiescence again. A dead pidfd can precede systemd
publishing MainPID=0/exited: unchanged-identity status is retried within the same
deadline, while real PID/InvocationID/cgroup drift is rejected immediately.
The preflight and inspection have six seconds and an independent
three-second cleanup budget; each CLI has at most one second and 8 KiB combined
stdout+stderr. Each preflight/inspection/cleanup phase has a separate 32 KiB
aggregate output cap, allowing repeated full UnitPath and property readbacks
while retaining an independent per-call limit. Its one fixed report is at most 2 KiB; raw CLI errors
are neither printed nor retained. SIGTERM/SIGHUP request ordinary cleanup.

An ambiguous launch result, collision or failure before identities are pinned
never authorizes signalling by guessed PID or unit name. It returns failure and
does not retry; the fixed guard cannot pass SIGSTOP and the trusted manager's
runtime/stop limits remain its independent backstop. SIGKILL/power loss can
prevent the parent from recording or verifying cleanup. No success or recovery
claim is made for those prefixes, and there is no durable recovery journal.
The bounded report preserves `primary_failure` separately from `cleanup_failure`.
Its summary `check=cleanup-unverified` cannot erase a parser/identity failure that
caused cleanup to become necessary. OS/CLI diagnostics are never forwarded.

## Pipe-only report transport

RLIMIT_FSIZE remains zero in the static bootstrap and guard. Before any
configuration read or manager contact, the Python entrypoint validates **both**
stdout and stderr as writable pipes/FIFOs, requires PIPE_BUF at least 2048 bytes
and initial poll readiness without error/hangup, and sets O_NONBLOCK. Regular
files, read-only descriptors, closed readers and initially full pipes fail before
main runs. Descriptor identity is checked again before writing.

The report is one atomic write of at most 2 KiB. There is no blocking wait,
partial-write retry, or fallback after a reporting attempt fails. A closed/full
pipe, identity drift, encoding/size failure or short/failed write returns exit 74.
When initial validation rejects stdout, an independently validated stderr pipe
may receive the single fixed preflight-error report. No regular FD is written.
If the pipe becomes unavailable after validation, no fallback is attempted.
The entrypoint uses `os._exit` to avoid an interpreter-shutdown buffer flush to a
broken FD. Exit 74 without a complete report is failed evidence, never success.

A future independently reviewed observation must use an **external bounded pipe
reader**, not `>result.json 2>stderr.txt`. Its required contract is:

1. Keep the reader outside the guard's FSIZE limit, with a reader RLIMIT_FSIZE
   soft allowance of at least 16384 bytes (or unlimited). Invoke its Bash launcher
   only through `env -i LC_ALL=C <exact-reader>` so inherited BASH_ENV cannot run
   before Python starts. Use a precreated private owned mode-0700 current
   directory. The reader forks one child with fresh anonymous stdout/stderr
   pipes and a private start barrier; it pins the child's pidfd before sending
   the start byte or reaping. Only then does the child execute the exact reviewed
   artifact through its retained FD, with stdin /dev/null and fixed environment/
   cwd. Do not use regular files as child stdio or automatically retry.
2. Hold both read ends through completion and multiplex them nonblockingly with
   selectors. Read at most 1024 bytes at once; cap each channel at 4096 bytes and
   both channels at 8192 bytes. Keep an independent 15-second monotonic deadline.
3. On overflow/deadline, request termination only through the owned parent
   pidfd, allow at most three seconds for its cleanup, then SIGKILL only that
   same pidfd and wait at most one further second. Record timeout/overflow or
   uncertain exit explicitly. Never infer a manager unit or signal by name.
4. Require child exit, EOF on both pipes, a complete bounded single JSON report
   and the expected exit status. Missing/truncated reports and exit 74 are
   reporting failures. Preserve both primary/cleanup failures without retrying.
5. Only the external reader may create a new mode-0600 evidence file with
   O_EXCL/O_NOFOLLOW. Store bounded/base64 channel data plus status/deadline
   metadata in a capped record (at most 16 KiB). Treat channel data as untrusted;
   no terminal forwarding or automatic manager cleanup/retry is permitted.

The separate candidate `resource_reader.py` now implements this protocol with
its own unconditional activation gate and unset artifact path/hash. See
[the reader contract](RESOURCE-READER.md) for exact bounds, failure semantics and
evidence persistence. Only fake Python children execute in its offline tests;
a future live invocation and its external capture still need their own review. The program does not claim that a
reader will remain alive after validation; report delivery can fail safely.

The host user's ancestor `cgroup.procs` files may be writable by the same UID.
Removal does not prove population zero or reclamation, and would not prove an
untrusted payload's descendants had not migrated elsewhere. This guard's source
does not create children or write cgroup files. Its results do not qualify a Nix
builder tree or establish an adversarial resource boundary. Such a runner still
needs a separately reviewed cgroup/namespace delegation boundary, group/keyring,
KVM/AF_VSOCK and filesystem/tmpfs accounting controls.

## Offline verification and packaging

`resource-guard.nix` is a separate fixed package recipe beside the existing
controller, using the repository's exact nixpkgs lock. The parent independently
built the exact initial `a9c4fd72` recipe and its 22 offline tests; no guard or
service ran. The recipe compiles both C modes as static
PIE, analyzes them, checks ELF hardening and runs only the fake Python tests.
It installs source/artifact SHA-256 sums. It never executes either C image,
systemd CLI or any service during checks.

Current local verification: 60 offline fake/pure tests passed, including mandatory
preflight rejection, malformed/missing/unlimited fields, collision/ambiguous launch,
PID/start/unit/cgroup/parent replacement, stopped barrier, missing controllers,
environment/output cap, deadlines, whole-group cleanup, removal/read races,
leaf reuse and interruption, current UnitPath categories and configuration/path
drift, a parent actually deleted/recreated while its old FD remains open, and
manager exit-publication races. The initial C modes compiled with the already installed
pinned musl GCC 15.3.0 and passed GCC `-fanalyzer` with warnings as errors.
Both images passed static PIE, no interpreter/NEEDED library, RELRO/NOW and
non-executable stack inspection. Ruff check/format passed.
The existing restrictive umask is expressed as `S_IRWXG | S_IRWXO` with identical
permissions. Its line-level `NOSONAR` comment suppresses Sonar's
capability-setting false positive. Hosted reanalysis must pass before merge.
The added fixtures cover 37/64 accepted roots and 65 rejected, known transient
fragment creation, allowed and premature disappearance, retained fragments,
replacement/mutation/owner/symlink/content bounds, spoofed properties and injected
configuration. Full 37- and 64-root lifecycle transcripts use the real bounded
command-output pump with fake CLI streams and remain within each phase cap;
separate fixtures enforce both the per-call and aggregate phase limits.

The revised source at checkpoint `c02c2b6261db30df50829703616eb2e3eaae8dca`
also built successfully with
`nix build --offline --no-link --json --option builders '' --file resource-guard.nix`.
Derivation: `/nix/store/awsf1b2fc5l9bx9igd39d47hb0dyyhbb-waddle-benign-resource-inspection-1.drv`.
Output: `/nix/store/ghsd3az4a3y70ix5bkrlk8cb47bd8nzj-waddle-benign-resource-inspection-1`.
Its build log confirms all 34 tests, Ruff check/format, both C compilation/analyzer
modes and ELF hardening checks passed. Installed Python source matched the
checkout byte-for-byte. The installed binary/guard SHA-256 values are respectively
`1458e29553d3f6ddb6c7cc8aa6d9a867318d3940791da46d9a2c0aa72757108b` and
`83b1079e525737f8f84c5e6d1813f5207eeffdc813c2914eb164e86c61240d2a`.
Neither image was executed. Documentation-only updates do not enter the package
source filter.

The transient-fragment/root-limit fixes at
`bef2a6162de80a46132345e48fab4daef2f3ca60` passed the same pinned offline build.
Derivation: `/nix/store/4nfn85lnzl2652qz5hs5m2iwkfrnjiw1-waddle-benign-resource-inspection-1.drv`.
Output: `/nix/store/2hqb3p71l4kvbnsxzgpy85fa0qh1y3jg-waddle-benign-resource-inspection-1`.
The log confirms 46 tests, Ruff check/format, both static C compilation/analyzer
modes and ELF hardening checks passed. Installed Python source matched the
checkpoint byte-for-byte. The entrypoint/guard SHA-256 values are respectively
`d61cd5af2bb84f2efec4ef1c44396c295a97f535390c9dcae2ad0a0c92174d50` and
`97c300aef0f42d55bad5624ff3d9f89ecba900cc23c521273062ac879e96c30b`.
Neither image was executed, and that checkpoint retained its unconditional gate.
Runtime manager-version/path assumptions, real fragment
timing and host execution still require validation; these fake transcripts are
not live evidence. The stable-configuration, trusted-manager and fixed-guard
limitations remain necessary.

The gated test checkpoint `7c5f9c4b1dddc69981e64be8f31cf947c6037ce5` adds a
real Host lifecycle fixture: a known successful fake launch followed by failure
before fragment or process ownership is pinned. It proves no SIGCONT or guessed
signal, `ok=false`, `userspace_quiesced=false`, `cleanup-unverified`, and no
deletion of the uncertain fragment. Its pinned offline package is
`/nix/store/c9027nqb920dq6831cvimbmdc49q7w9m-waddle-benign-resource-inspection-1`
(derivation `fhzz8d8f3iqilnd42na9awphaqmbmilj`).

The separately identifiable local activation commit `b4ff628025c19e3cc9dd48609bec90b6f97acf74`
also passed all 47 offline tests, Ruff, C compilation/analyzer and ELF checks.
Its package is `/nix/store/gv6wq661jhbj3kyf87z3w3dy1s6q2kbp-waddle-benign-resource-inspection-1`
(derivation `b8f37smqb8icy6826c9xi3zb0lnpp6ph`), and installed Python source
matched the commit byte-for-byte. The production activation diff deletes exactly
the five-line unconditional blocker function and its single launch call. Other
changes only remove obsolete gate-patch fixtures and replace the blocker test
with mandatory malformed-UnitPath rejection. Those were offline build results;
the activation image was later invoked once and failed as described above.

After gate restoration `d532f9a072fdf9503dbf39970d837d91015c789f`, the v262,
failure-preservation and pipe-report fixes are `8b33e052b074b6b151da99c012ae0a2e94c820c0`,
with final unavailable-channel regressions in `a351a41b919f1f59e2084c41e2582f3016f481b9`.
The pinned offline build passed all 60 tests, Ruff check/format, both static C
compilation/analyzer modes and ELF hardening checks. Derivation:
`/nix/store/bv2i3fjgm326xb9wnrnwavjxq22fpk04-waddle-benign-resource-inspection-1.drv`.
Output: `/nix/store/hn1yi0ihyzzzvb2j94p9ic523c7mq9qk-waddle-benign-resource-inspection-1`.
Its launch gate remains unconditional. Neither package image was run during
these fixes; manager cleanup and any new activation are outside this work.

No guard, launch/configuration-query manager command, transient service, namespace,
VM, Nix workload, network test, synthetic fault or application was launched during
these fixes. One earlier benign attempt failed: saved read-only evidence reports
v262 omitting CPUAccounting and regular-file capture failing under FSIZE=0.
It does not prove a successful observation or primary cleanup. Independent
manager metadata cleanup already completed separately and was not touched here.
The later fixed benign observation and its limits are recorded on #2106;
runtime behavior and tests do not depend on external workspace paths. It
observed the configured cgroup values and process identity for that one guard,
but did not establish enforcement under pressure, aggregate cleanup, kernel
reclamation or Nix workload isolation. Offline Nix package builds are separate
build evidence.
