# Bounded no-VM host canary controller

This directory implements the controller for #2106/#2046. It is **not yet live
evidence**: only builds, pure models, a pure cBPF interpreter and integration
fakes have run. The combined controller and controlled inspection probe require
independent final review before their first execution. That is an external work
gate, not a mutable approval flag or a claim made by this program.

Manager operations are **unconditionally blocked in code** before connecting,
creating a run directory or starting a service. A bounded transport that binds the
actual D-Bus connection to the observed manager has not been implemented. Pinned
`systemctl`/`systemd-run` binaries do not authenticate their private socket peer's
process identity. This is a partial implementation, not an executable canary.
[The transport design and remaining blockers](MANAGER-TRANSPORT.md) describe the
required follow-up. The host, system bus and system/user managers are external
trusted dependencies; their executable authentication remains **unverified**.
The installed client version does not authenticate the running manager's image.

The scope is one fixed static inspection payload and fixed AF_UNIX sentinel
operations. No VM, QEMU, guest image, IP socket, production destination, credential
or host data workload is accepted. A malicious process with the same host UID,
the host administrator and a compromised kernel are outside this boundary.
Mode 0700 does not protect against the same UID. This profile does not establish
isolation for arbitrary programs or QEMU.

## Components and lifecycle

`bootstrap.c` is static PIE. It clears the environment before starting pinned
dynamic Python with `-I -S`, closes descriptors above 2, sets `no_new_privs`, uses
umask 0077 and refuses root. The only entry modes are `run`, `service <nonce>` and
`cleanup <nonce>`. Generated nonces have exactly 32 lowercase hexadecimal digits;
no executable, command, mount, endpoint or extra argument can be supplied at run
time. Dynamic Python, Bubblewrap and systemd client tools are resolved immutable Nix
store artifacts; the running user manager is a separate external dependency. Local source and static binary hashes
are checked against the frozen build manifest; this assumes the trusted checkout
and manifest have not been deliberately replaced by their same-UID owner.

The controller exclusively creates `/dev/shm/waddle-host-<uid>-<nonce>` with mode
0700. It creates an exact seven-entry inventory: five bounded JSON evidence files,
`host-file`, and `host.sock`. It also binds a distinct run-named abstract UNIX
listener. It refuses directory, file, socket and service collisions. It never
unlinks an existing path to make a bind succeed. Host positive controls verify
the nonce and listener `SO_PEERCRED` before and after the sandbox.

The fixed transient user service requests and reads back Type=exec, Restart=no,
control-group termination, 5-second start/2-second stop/30-second runtime limits,
65,536-byte soft and hard FSIZE, null stdin/stdout/stderr and no new privileges.
The service reads its actual cgroup v2 files and membership before and after the
payload: cpu.max must be `50000 100000`, memory.max 134217728, memory.swap.max 0,
and pids.max 16. Missing controllers, unsupported properties, unexpected members,
unlimited values and partial evidence fail closed. Service output cannot reach
the journal. Controller subprocess pipes each have an independent 8-KiB cap and
3-second deadline; sandbox stdout/stderr/info limits are 8192/512/2048 bytes with
one 20-second deadline. Each witness gate has a five-second poll bound. The witness
is namespace PID 1: a bare default-action SIGALRM does not supply a termination
guarantee and has been removed. The observer's pidfd SIGKILL and independently
enforced service deadline remain mandatory.
Every run-owned evidence file is capped at 64 KiB. No raw launcher error is
forwarded or saved.

Before Bubblewrap starts, the service records all seven namespace identities,
the invocation ID, PID/start/boot identity, cgroup path and inode, effective limits,
and user-manager identity. The service must have Seccomp=0 and zero inherited
filters; opaque inherited policy is rejected. Its supplementary group vector
must exactly equal the authenticated external user manager's observed vector (at most
64 entries). This is a **distinct inherited-groups profile**, not an empty-groups
claim. Sandbox vectors are read by the service in its own user namespace and
must match exactly, including order, additions and removals. Overflow IDs printed
inside the sandbox are never treated as host identity evidence.

## Fixed boundary and observation gates

Bubblewrap is mandatory and always requests user, PID, mount, network, IPC, UTS
and cgroup namespaces, with no `-try` option or fallback. Its empty tmpfs root
contains only `/proc`, `/dev/null` and `/probe`. A static witness is initially
executed through an explicitly inherited executable FD; that FD is closed before
READY. The helper's executable and mappings are authenticated setup artifacts.
Only the gate FD3 and null/pipe/pipe standard descriptors remain at the gates.
For pinned Bubblewrap 0.12.0 the tmpfs root's mountinfo source root is exactly
`/newroot`; both the trusted observer and self-probe reject `/` or other roots.
Bubblewrap's initial witness environment is exactly `PWD=/` plus its terminating
NUL. `clearenv()` does not erase `/proc/PID/environ`; the observer checks that exact
initial environment for the witness and the truly empty exec environment for the
subsequent probe.

Bubblewrap's ordinary tmpfs root does not add noexec, and this host's /dev/shm
does not supply it. Therefore a fixed trusted setup finisher has only
namespace-scoped CAP_SYS_ADMIN, CAP_SETFCAP and CAP_SETPCAP during setup. It sets
`user.max_user_namespaces=1`, creates a nested user and mount namespace together,
writes exact `0 0 1` UID/GID maps through a held proc directory FD, then closes
that FD. It uses additive per-mount `mount_setattr` with no cleared attributes:
root and proc gain ro/nosuid/nodev/noexec; null gains nosuid/noexec; probe gains
ro/nosuid/nodev. It drops every bounding capability (SETPCAP last), clears all
other capability sets, sets NNP and verifies another user namespace cannot be
created. Any failure aborts before the first gate. It never remounts a host
superblock or changes a host namespace.

The trusted service requires exactly four mounts and their parent relationships,
the specified security flags (only optional relatime is tolerated), an exact
root/device inventory, the mounted probe's source device/inode, namespace
separation from the service baseline, external UID/GID maps to the service owner,
zero capabilities, one thread, FSIZE, exact anonymous pipe inodes/access flags and
unchanged process identity. It requires readable procfs observation; a ptrace,
LSM or namespace permission denial is a failure, never replaced by self-report.

There are three fixed gates, all on the same pidfd-bound process:

1. `READY\n`: witness filter installed; trusted observation precedes byte `G`.
2. Sentinel results plus `INSPECTOR_READY\n`: the stricter inspector filter is
   stacked; trusted observation precedes byte `P`.
3. The controlled probe's `PROBE_READY\n`: **after exec**, the observer requires
   the actual probe executable inode, its exclusive file-backed mappings, the
   same process/namespaces, both filters and exact FD0..3. Only then it writes
   `I` and closes the writer. The controlled probe requires `I` followed by EOF,
   closes FD3 and performs its ordinary fixed inspection with FD0..2.

No stdout is accepted before the third release. The controlled probe build from
`../host-canary` is required; the ordinary probe lacks this gate and cannot pass.
Actual kernel observations, verified artifact/source hashes, the pinned witness's
sentinel observations and `untrusted_probe_output` are stored separately. A
matching self-report alone never satisfies a trusted observation.

## Syscall and sentinel scope

`filter.c` constructs fixed x86_64 LP64 cBPF instructions. Other architectures and
x32 are killed; all unlisted calls return EPERM. The witness filter is installed
after setup and before the first READY. It permits only AF_UNIX stream sockets
with CLOEXEC and protocol zero, plus connect/bind/listen/accept4 and read/write/
close. It does not permit socketpair, sendmsg, recvmsg or SCM_RIGHTS. The second
filter denies all socket operations. Both deny keyctl/add_key/request_key,
io_uring, ioctl, ptrace, process_vm, BPF/perf, process creation, namespace/mount
changes and SysV/POSIX IPC APIs. Opens are read-only; PROT_EXEC memory changes and
memfd creation are denied. These restrictions are necessary for this inherited-
groups profile: a network namespace alone does not confine AF_VSOCK, and key
permissions can depend on supplementary groups.

Both layers deliberately retain execve for the controlled probe transition.
They do **not** claim to deny execve afterward. The observed post-exec closure is
the static `/probe` inode and its procfs aliases, no other executable file or
mapping, readonly/noexec root, no writable or executable-memory creation, and
FD0..2 after gate closure. Procfs includes non-PID interfaces; this controller is
qualified only for the fixed inspected code and fixed paths, not arbitrary
programs with inherited groups. Seccomp mode/count observations do not identify
filter instructions by themselves: provenance also depends on the verified
static image, pinned filter source, deterministic filter construction, blocked
single-threaded control flow and the absence of inputs that choose a policy.

The sandbox file and pathname socket must fail with ENOENT, the host abstract
socket with ECONNREFUSED, and a sandbox-local abstract UNIX round trip must pass.
The host rejects any unexpected queued sentinel connection. No IP socket is
created. These run-owned tests do not prove group-only DAC isolation or probe
access to any real credential. No synthetic key is created by this implementation;
the key API denial is covered by pure BPF tests, with live negative syscall
qualification still required after review.

## Cleanup and offline checks

Cleanup revalidates run-directory device/inode, every retained/transient entry,
service invocation/cgroup/process identity and cgroup emptiness. It unlinks only
the two exact transient file/socket names through anchored directory FDs. It
never recursively deletes or prunes, and never signals an unrelated reused PID.
The five JSON evidence files and their mode-0700 directory are intentionally
retained. Cleanup can repeat after any prefix of its deletions; replacements or
unexpected entries abort before deletion. A garbage-collected unit is acceptable
only when its recorded cgroup is absent, and no signal is sent on that path.
Before sending a launch request, `launch.json` records the attempt and is fsynced;
an unknown outcome is never retried. The success path also requires Result=success,
ExecMainCode=CLD_EXITED, ExecMainStatus=0 and the exact exited service state, checked
again before stop. A result file cannot override timeout, signal, OOM or exit failure.

Ordinary exceptions after an object's identity is recorded use that identity to
clean transient files. Persisted cleanup validates the complete run inventory
and removes its sentinels even if service.json is empty or torn.
An empty launch record means no request was sent; an attempted/unknown launch
without a complete service identity remains a failure and never authorizes a
signal. Repeated cleanup is safe. The model fixtures cover these paths.

A hard interruption, or an ordinary error before an object's identity is recorded,
can leave a partial inventory that a later process cannot authenticate; cleanup
then refuses deletion. This includes the socket bind-to-identity-record window and
the period before state.json durably records the complete inventory. No service
request has been sent in that phase.
A durable identity publication scheme and manager reconciliation remain required
before live execution; cleanup does not guess ownership from path or UID alone.

The runtime is built as one derivation with Python, systemd client tools,
Bubblewrap, static compiler and controlled probe fixed by the repository lock:

```sh
nix-build --no-out-link default.nix
```

This command performs offline fixture/static/analyzer checks as part of the build;
it never invokes the resulting canary. There is no path-taking configuration or
compiler CLI. The derivation generates the static bootstrap's fixed Python/script
paths and an immutable manifest of artifact/source hashes. Runtime configuration
is accepted only from that installed Nix output. Standalone `make` remains useful
for compiler/model checks but cannot produce an accepted runtime installation.

The checked lock resolves Python 3.14.7, systemd client 261.2 and musl GCC 15.3.0.
Bubblewrap is explicitly overridden to v0.12.0 with fetchFromGitHub hash
`sha256-VnhJ5bej3/GTHcU8+AkyR7f3J0KKDuoc94SFxo4grhk=`; the vulnerable lock-default
0.11.2 is not the controller dependency. Relevant 0.12.0 source paths were checked: `write_uid_gid_map` for the
initial single-ID mapping, `set_ambient_capabilities` before exec, child FD
pass-through with `--as-pid-1`, optional-only `seccomp_programs_apply`, and
`bind_mount` adding restrictions while preserving existing mount flags. The
controller's finisher establishes and observes its own final flags and filters.

The current locked fix-tranche build passed 1,567 pure BPF assertions, 44 Python
fake/model tests, static PIE/RELRO/NOW/non-executable-stack checks, GCC analyzer
with warnings as errors, and Ruff. Its changed probe dependency separately passed
562 parser/frame C assertions, 1,720 pure barrier assertions and eight Python tests.
Build output: `/nix/store/6ifa8w31x1qscxk2hn9kmb4wc3lq6f2i-waddle-host-confinement-canary-1`.
Its frozen manifest resolves Bubblewrap to
`/nix/store/v6a6s4aqw7l6n74x72wblvl3994bksw3-bubblewrap-0.12.0/bin/bwrap` and contains
no manager-image artifact. These are offline results; no canary executable ran.

`check` runs only `filter_test.c` (pure cBPF interpretation; no filter installation)
and Python fake/model tests. The bootstrap, witness, probe, systemd, Bubblewrap,
namespaces and sockets are never executed by builds or tests. The intended runtime entrypoint
is the output package's `bin/host-confinement-canary run`, currently blocked in code;
interrupted cleanup is `bin/host-confinement-canary cleanup <nonce>`. No extra
approval file is required. Rebuild the locked derivation after source changes. No current test result establishes the real host's
namespace support, delegated cgroup controls, mount behavior, procfs access or
end-to-end cleanup; those remain the first live qualification's fail-closed gates.

Design references: [Bubblewrap v0.12.0 source](https://github.com/containers/bubblewrap/blob/v0.12.0/bubblewrap.c),
[PID namespace signal rules](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html),
[Linux user namespace mappings](https://man7.org/linux/man-pages/man7/user_namespaces.7.html),
[systemd resource controls](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html)
and [execution settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html).
