# Gated external reader for one fixed benign observation

`resource_reader.py` is a separate candidate for capturing a future, separately
authorized single benign observation. Its unconditional activation gate is enabled;
`ARTIFACT` and `ARTIFACT_SHA256` are unset. The guard's separate unconditional
launch gate is unchanged. Building or reviewing this reader authorizes no attempt.
It changes neither #2115's published build-only scope nor the existing controller.

A later exact activation review must bind the reader to one reviewed immutable
`/nix/store/<hash>-waddle-benign-resource-inspection-1/bin/benign-resource-inspection`
path and that entrypoint's SHA-256, including review of that package's complete
source/dependency closure. There is no command, path, endpoint, environment or
runtime approval argument. The reader rejects arguments. Hashing opens a regular,
read-only, size-bounded file with O_NOFOLLOW and retains its FD. The child executes
that same FD with fixed argv/environment/cwd; it never reopens a supplied path.
The host OS, immutable Nix store and user manager remain trusted; hostile same-UID
processes remain excluded. A byte hash alone does not authenticate the manager or
prove the artifact's dependency closure. The existing guard limitations apply.

## Required invocation environment

Even the Nix Bash launcher can honor inherited BASH_ENV before it reaches
Python's `-I -S`. A future invocation must therefore clear the environment
**before** entering that launcher: `env -i LC_ALL=C <exact-reader>`, using the
reviewed immutable env executable and reader paths recorded with activation.
Do not add BASH_ENV, ENV, PATH or other inherited variables. The reader needs a
soft RLIMIT_FSIZE allowance of at least 16384 bytes (or unlimited), independently
of the guard's unchanged zero allowance. Its precreated current directory must
be owned, mode 0700 and reserved for this one deliberate future attempt. No file
redirection or automatic retry is permitted. The concrete paths, source/closure
hashes and invocation require a separate independent review before execution.

## One child and bounded capture

The reader requires a single Python thread, default SIGCHLD disposition, open
standard descriptors and FD exec support. Closed reader standard descriptors
are refused before fork so new pipes cannot alias the dup2 targets. It creates fresh anonymous stdout/stderr pipes, plus a private
start pipe. The forked child resets interrupt handlers, connects only the fresh
report pipes, and waits for one start byte. Before sending that byte, the reader
opens a pidfd for its own unreaped child. There is no wait/poll/reap before that
pin, and no artifact execution before release. Default SIGCHLD and ownership of
an unreaped direct child prevent PID reuse during this short pin interval.
After release, the immutable bootstrap itself closes unrelated inherited FDs.

Both report read ends are held and drained concurrently with nonblocking reads
of at most 1024 bytes. Memory is capped at 4096 bytes per channel and 8192 total;
excess bytes cause failure and are discarded during bounded termination. Capture
has a 15-second monotonic budget. Overflow, timeout or reader interruption begins
at most three seconds for SIGTERM cleanup, then at most one second after SIGKILL.
Only `pidfd_send_signal` targets that original parent; reaping uses only
`waitid(P_PIDFD, ..., WNOHANG)`. No PID, process group or unit is inferred, and no
manager command is present. There is exactly one child attempt per invocation;
there is no retry or durable global exactly-once journal.

A pump/setup error after pinning also gets bounded termination of the pinned
child. If pidfd acquisition fails, the start pipe closes without a start byte:
no artifact executes. No guessed signal or numeric-PID reap is attempted. That
exceptional child can remain unreaped until reader exit reparents it; the result
explicitly records `child-pin-failed-unreaped`, `child-exit-unverified`, and no
proven parent death. Offline fixture teardown alone reaps this fake child.

SIGTERM/SIGHUP/SIGINT set an interruption flag; subsequent signals do not reset
the cleanup clock. SIGKILL, reader death, kernel stalls or power loss can prevent
capture, termination or evidence persistence. Time bounds assume kernel calls
return and the reader is scheduled; SIGKILL cannot guarantee prompt exit of a
kernel-blocked task. There is no durable recovery or automatic manager cleanup.

## Reports and failure evidence

Success requires the exact child reaped, EOF on both pipes, exactly one complete
ASCII JSON line on stdout (at most 2048 bytes), empty stderr and the matching
exit status: 0 for success, 1 for a delivered guard failure. Missing/truncated or
multiple reports, duplicate keys, malformed JSON, unknown/incorrect fields,
contradictory status and exit 74 all fail. Successful reports must carry the fixed
limit values, unit/cgroup shape and narrowly permitted quiescence/reclamation
fields. This parser checks the trusted artifact's report contract; it does not
replace the guard's external observation of actual kernel/manager identities.

Reader `primary_failure` and `cleanup_failure` are separate. A valid failed guard
report is retained intact under `guard_report`, preserving its own primary and
cleanup failures. Raw channel bytes are stored only as bounded base64, never
forwarded to the terminal. `possible_unfinished_manager_operation` is true after
release unless a valid report attests userspace quiescence and reader cleanup
completed. Killing or reaping the parent alone cannot establish unit cleanup:
it may interrupt a manager request after the manager has accepted it, or leave
an already-created unit. The reader issues no manager queries or inferred cleanup.
Its conservative flag is uncertainty evidence, not proof an operation is pending.

`parent_pidfd_dead` and `parent_reaped` describe only the original direct child.
Neither that status nor a guard `events-populated-0` report proves aggregate
kernel resource reclamation. `kernel_reclamation` is always `unverified`; the
removed-cgroup evidence remains separately classified by the guard. This reader
provides no Nix/workload, descendant-migration or aggregate-accounting assurance.

## Evidence persistence

Before any child attempt, the reader pins an owned current-directory FD and
checks that it is not group/world writable; it retains that anchor throughout
capture. Only after capture does the reader create
`benign-resource-capture-<128-bit-random-nonce>/evidence.json` in its working
directory. It requires the anchored current directory to be owned by its UID
and not group/world writable. Directory creation refuses collisions; the new
directory must be owned mode 0700. Its record is an anchored O_EXCL/O_NOFOLLOW
regular, one-link mode-0600 file, at most 16 KiB. File/directory identities are
checked after writes and fsync. No uncertain path is deleted or reused. Neither
child report pipe is ever redirected to an evidence file. The reader stays outside
the guard's FSIZE=0 and requires its own file-size allowance to fit the record.

An unsafe destination, collision, disk/full/write/fsync error, or path drift fails
persistence. A partial directory/file is retained without replacement or deletion;
there is no fallback write of child bytes to stdout/stderr. The current directory
is the evidence destination only, not an execution-path selector. No evidence
location is printed. Exit 0 means a valid successful report was captured and
persisted; 1 means failed observation evidence was persisted; 78 means a gate,
preflight or persistence failure. Missing or partial evidence is never success.
Filesystem operations are subject to ordinary OS/filesystem blocking and crash
semantics, independently of the finite child-capture clock.

## Offline verification

`resource-reader.nix` is a separate build recipe pinned to the repository nixpkgs
lock. Its checks fork Python fake children only; they execute no guard image,
systemd tool, service, network or application. Tests cover success, pin-before-
progress/reaping, unrelated-child ownership, timeout/SIGTERM/SIGKILL, overflow,
concurrent streams, early/closed channels, missing/malformed/multiple/truncated/
status-inconsistent reports, schema/reclamation claims, reader interruption,
primary/cleanup errors, pin failure and collision/symlink-safe evidence writes.
The source pin is tested using explicit non-executable fixture bytes only.

Independent review of the exact reader diff and a separate artifact/activation
review are required before any live attempt. No reader or guard activation is
part of this implementation.

Local implementation checkpoint `8adbe71f12a1f785ef0bce7220ec4db134da964b`
and deterministic full-pipe regression `c5223a96` passed 87 combined offline
tests (60 guard + 27 reader), Ruff check/format and `git diff --check`.
The final reader package built with
`nix build --offline --no-link --json --option builders '' --file resource-reader.nix`.
Derivation: `/nix/store/y8lw9fpiiq4hn9xmacp6abrdj830k347-waddle-benign-resource-reader-1.drv`.
Output: `/nix/store/s9skrzfv4rd9gydhqyh80k4spa6dl8s7-waddle-benign-resource-reader-1`.
Its log confirms all 27 fake-child/evidence tests and Ruff checks passed. Installed
reader source matches the checkout byte-for-byte. No installed reader, guard,
manager command, service or probe was run. Both activation gates remain enabled,
and the reader's immutable artifact pin remains unset. The guard source and
`resource-guard.nix` are byte-for-byte unchanged from gated `3e5ef7d0`.
