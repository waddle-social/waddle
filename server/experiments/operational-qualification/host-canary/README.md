# Static self-inspection probe

This is the self-inspection component of #2113/#2106/#2115, **not an executable host
canary**. It builds two static Linux probe variants but supplies no launcher, service,
sentinel listener, cleanup operation or run command. The probe has not been run
on the host or inside a sandbox. Tests exercise synthetic data only.

`build/probe` accepts no arguments and never reads stdin. It clears its environment
and reads only fixed procfs paths about itself, descriptor metadata and
`RLIMIT_FSIZE`. It does not execute commands, create namespaces, mount filesystems,
open network sockets, signal other processes, read credentials or delete files.
Its only output is a bounded frame to a validated anonymous stdout pipe; the
standalone variant never writes to stderr. `build/probe-controlled` adds the
fixed post-exec identity barrier described below. Both variants leave host and
QEMU/guest isolation `unverified`.

## Build and offline checks

Dependencies: GNU Make, a Linux musl GCC supporting static PIE, Python 3.10 or
later, GNU binutils `readelf`, and `rg`. Formatting/lint additionally uses
clang-format 21 and Ruff. No package download, production service or repository
workspace build is needed. Set `STATIC_CC` to the **absolute compiler executable**
from the reviewed, pinned toolchain; there is no compiler fallback in the
Makefile. A compiler path by itself is not a provenance guarantee.

From this directory:

```sh
make STATIC_CC=/absolute/path/to/x86_64-unknown-linux-musl-gcc all check check-static check-analysis
clang-format --dry-run --Werror *.c *.h
ruff format --check frame.py check_frame.py test_frame.py
ruff check frame.py check_frame.py test_frame.py
```

`all` compiles both probe variants; it does not run them. `check` compiles and runs
two separate fixture binaries. The report fixture links **only** `parse.c` and
`frame.c`; the barrier fixture links **only** `barrier.c` and `parse.c`. The OS
readers (`inspect.c`, `barrier_io.c`) and probe entry point are absent from both.
The report fixture's output is a synthetic frame written under `build/` and
independently decoded by the Python checker. Python
tests make no subprocess, filesystem, socket or mount calls. `check-static`
inspects both probes as ELF data: static PIE, no interpreter, no dynamic library or
loader search dependency, RELRO, immediate binding and a non-executable stack.
Static PIE legitimately contains a dynamic relocation section.
`check-analysis` separately compiles every C file with GCC's analyzer and
warnings-as-errors; it does not execute the compiled objects.

The implementation was checked with musl GCC 15.3.0, 562 report/parser fixture
assertions, 1,720 barrier fixture assertions and eight Python test methods
(including rejection at every truncated byte offset).
The barrier fixtures cover partial readiness writes; wrong, duplicate and extra
release bytes; empty EOF; byte-then-EOF; `POLLIN|POLLHUP`; invalid poll flags;
interruptions, would-block results, retry exhaustion, clock failure and deadlines;
and anonymous-pipe type, access, link/inode and alias classification.
The standalone `default.nix` uses the repository lock to build both probes and
run the same fixture, static-ELF and analyzer checks in a non-publishing CI job.
It never executes either probe. These checks establish parsing, framing and build
properties. They do not establish that the OS collection paths, write deadlines
or proposed mount profile work in a real Bubblewrap sandbox. This non-publishing
workflow does not build or publish the production server image.

## Controlled post-exec barrier

`build/probe-controlled` accepts no command operands or environment configuration.
After `clearenv()`, it validates FD3 as a read-only anonymous pipe and FD2 as a
distinct write-only anonymous pipe. It checks the descriptor types, access flags,
device/inode identities and matching `pipe:[inode]` procfs links. `O_PATH`, named
FIFOs, wrong access modes, aliases, inaccessible procfs and non-procfs descriptor
directories fail closed. The validation directory is closed before any marker.
These local checks do not authenticate either peer or the executable.

The probe makes FD2 and FD3 nonblocking, ignores SIGPIPE for this exchange, and
emits exactly the twelve ASCII bytes `PROBE_READY\n` on FD2. It then requires
exactly the byte `I` followed by EOF on FD3; receiving the byte without EOF never
releases it. `POLLIN|POLLHUP` is accepted so buffered data followed by writer close
is read correctly. Empty EOF, wrong/duplicate/extra bytes, poll errors and invalid
flags fail closed. Readiness output and release input share one 5,000 ms monotonic
deadline and at most 128 poll/I/O iterations, including EINTR/EAGAIN retries.
Clock errors, backward time and expired deadlines fail closed. Procfs/syscall
stalls and total runtime still need the controller's external service deadline.

Before release, the single-threaded probe performs only the channel validation
and this bounded exchange; it collects no report and emits no other output.
After the byte and EOF, it closes FD3 and restores the prior SIGPIPE disposition
before the existing exact FD0/1/2 self-inspection and frame path. The standalone
variant has no barrier and retains its existing entry/output behavior. Either
variant returns 74 for a barrier/channel failure, 64 for an operand or failed
environment clearing, and 2 after a complete report; none is a canary pass.

The controller must receive the complete marker, bind the exact FD0..3 and pipe
identities, pinned `/proc/<pid>/exe` device/inode, namespace identities and maps,
exact host supplementary-group vector, capabilities and seccomp state to the
same live pidfd/process-start identity, then revalidate before sending `I` and
closing its sole release writer. FD0 must be the fixed read-only null device;
FD1/FD2 must be distinct report/readiness pipes and FD3 the release pipe. It must
enforce a pinned immutable probe artifact and remain within the barrier budget.
A seccomp filter count alone does not authenticate a filter's policy; the
controller also needs a trusted pinned launcher/filter installation chain.
This probe does not implement or claim any of that external proof. A forged
marker/frame, release byte, successful build or synthetic fixture cannot prove
the controller, process binding, confinement, or host-canary success.

## Requested and observed state

The fixed profile requests namespaces in this order: user, PID, mount, network,
IPC, UTS and cgroup. The probe records their procfs inode numbers. **Their presence
does not prove separation from the service or host.** A later trusted observer
must compare them with a baseline collected inside the transient service before
Bubblewrap. With `--unshare-user --uid 0 --gid 0 --disable-userns`, Bubblewrap
0.12.0 creates a second user namespace. In the probe's own `/proc/self/uid_map`
and `gid_map`, the candidate shape is one mapping from zero to zero in its
parent namespace, of length one. This self-map does not prove the host identity:
the trusted service observer must read the child's maps from the service user
namespace and independently verify they map to its nonzero UID and GID. See
the [pinned Bubblewrap source](https://github.com/containers/bubblewrap/blob/v0.12.0/bubblewrap.c)
and [Linux user namespace mapping semantics](https://man7.org/linux/man-pages/man7/user_namespaces.7.html).
All four observed UIDs/GIDs must be zero inside that user namespace, all five
capability masks zero and `NoNewPrivs` one. The probe records only a bounded
count of up to 64 supplementary groups; namespace-local IDs can alias to an
overflow ID. A trusted external observer must independently compare the exact
service and child host-group vectors. Retaining groups is a distinct profile,
not proof that group-derived authority is absent; the controller must enforce
and verify the required mount, descriptor and syscall boundaries.

The mount parser accepts exactly four mount targets, unique mount IDs, no
propagation/optional mountinfo fields and only the listed mount options plus
one optional `relatime` per mount. Duplicate `relatime`, every other extra option
and missing security flags are rejected. Non-root mounts must have the root
mount as their parent. The root mount's parent must
differ from its own ID. This is a deliberately narrow **candidate shape** whose
compatibility with real Bubblewrap remains untested:

| Target | Required mount options; optional `relatime` | Filesystem / root constraints |
| --- | --- | --- |
| `/` | `ro,nosuid,nodev,noexec` | tmpfs; root `/` |
| `/proc` | `ro,nosuid,nodev,noexec` | proc; root `/` |
| `/dev/null` | `rw,nosuid,noexec` | devtmpfs or tmpfs; root `/null` |
| `/probe` | `ro,nosuid,nodev` | tmpfs, ext4, btrfs or xfs; absolute unescaped root |

The mount source and filesystem-root strings are never emitted. A matching shape
does not authenticate the probe's source inode, prove absence of mounted sentinel
aliases, inventory ordinary files/device nodes under these mounts, or establish
the full runtime artifact closure. Those results remain explicitly unavailable.
In particular, enumerating mount targets is not a device inventory.

The exact descriptor inventory is FD 0 (read-only character device 1:3), FD 1 and
FD 2 (distinct write-only anonymous pipes). All other inherited descriptors,
terminal/socket/named-FIFO outputs, alternate devices, read-write descriptors and
`O_PATH` descriptors are rejected. Access classification excludes `O_PATH` before
checking access-mode bits, because its zero access-mode bits do not permit reads.
The procfs directory descriptor used for enumeration is the only
temporary exclusion; proc-file readers have already closed their descriptors.
The probe is single-threaded. This does not authenticate the controller at the
other end of either pipe.

`/proc/self/cgroup` must be exactly `0::/` with a newline. That observation only
describes namespace-relative membership. It does **not** establish effective
CPU/memory/swap/task limits, actual host cgroup identity or process membership.
The actual soft and hard `RLIMIT_FSIZE` must both be 65,536 bytes. That limit does
not bound pipes or journal storage.

## Framing, resource bounds and failure behavior

The probe reads at most 16 KiB of status, 32 KiB of mountinfo, 256 bytes per ID map,
1 KiB of cgroup membership and 64 bytes per namespace link. It reads one excess
byte to detect overflow; no truncation is accepted. Proc-file reads have at most
16 attempts, descriptor enumeration eight. Fixed path readers require regular
procfs files. Parser errors never echo raw input, usernames, paths or mount sources.

A report is `WADDLE_HOST_PROBE_V1 <decimal-payload-byte-count>\n`, followed by one
ASCII JSON object and a final newline. The entire frame is at most 8 KiB. Each
observation is separately `observed`, `failed` or `unavailable`, beside the fixed
`requested` profile. Numeric fields on failed/unavailable records are incomplete
and cannot be treated as observed results. All frames identify their origin as
`untrusted_probe_output`; a forged matching record cannot establish isolation.
The decoder rejects unknown/missing/duplicate fields, positional records,
noncanonical headers, trailing data, invalid numeric types, oversized/truncated
records and inconsistent claimed observations.

Output uses nonblocking writes with a single monotonic one-second deadline and a
maximum of 8 KiB. The probe ignores SIGPIPE for its own output operation only.
No record is emitted unless all standard descriptors match the profile. A partial
write is an invalid frame. The decoder accepts a complete bounded in-memory byte
string; `check_frame.py` reads a local fixture and is **not** a live stream
supervisor. The controller must independently bound bytes and time on
stdout/stderr and all launcher output, and disable or bound journal use. Procfs
syscall stalls and total process runtime still require enforced service deadlines.

Exit codes are 2 after a complete report (never canary success), 64 for any command
operand or failed environment clearing, and 74 for an unsafe output channel or
framing/output failure. Missing output is not a pass. `clearenv()` here only
clears this static probe's environment: it does not sanitize a future dynamic
Bubblewrap launcher. Static ELF verification prevents loader injection into this
component but supplies no assurance about other executables.

## Remaining execution blockers

No host execution is safe to infer from this slice. The combined controller now
implements the following candidate controls, which still need complete independent
review and live qualification after the transport blocker is resolved:

1. A pinned trusted static pre-launch helper that clears the environment before
   invoking dynamic Bubblewrap; exact fixed executable, argument, mount, FD and
   device inventories. Mandatory user/PID/mount/network/IPC/UTS/cgroup namespaces,
   no `-try` switches or permissive fallback, capability removal, `NoNewPrivs` and
   prevention of further user namespaces. Reject all argument injection paths.
2. An exclusively created mode-0700 run root and collision-refusing transient
   `Type=exec` user service. Require finite `CPUQuota`, `MemoryMax`, `MemorySwapMax`,
   `TasksMax`, `LimitFSIZE`, start/runtime/stop deadlines, `Restart=no` and
   control-group termination. Observe actual kernel cgroup values and member
   processes from the trusted pre-Bubblewrap service baseline; do not infer
   enforcement from requested properties or namespace-relative paths.
3. Three separate run-owned file, pathname-UNIX-socket and abstract-UNIX-socket
   sentinels. Use unique nonces/listener identity, host positive controls before
   and after, a sandbox-local positive socket and exact expected negative
   outcomes/errno. Verify no mounted alias. A missing pathname alone cannot prove
   network isolation. Never contact production or LAN destinations.
4. Record and revalidate service InvocationID, cgroup identity, process start
   identity/pidfd and directory device/inode. Use anchored directory descriptors,
   exact inventory and nonrecursive deletion. Reject replacements, unexpected
   entries, unit collisions and stale identities; demonstrate interruption-safe,
   repeatable cleanup without ever targeting unrelated processes.
5. Independent byte/time bounds for every pipe and journal path, authenticated
   evidence binding, abort paths, and live tests of the complete controller.

Sentinel/errno, permissive-launcher-injection, unit collision, stale PID/InvocationID,
path replacement and interrupted/repeated-cleanup tests now exist in
`../host-controller/` and run only against isolated fakes. The controller's manager
operations are unconditionally blocked pending bounded authenticated D-Bus
transport. Those offline results do not establish live cleanup or host isolation.
This probe's own pure tests cover namespace syntax, maps, capabilities, exact
mount shape (including Bubblewrap 0.12.0 root `/newroot`), FDs, namespace-relative
cgroup text and malformed/framed evidence. See the controller transport document
for the remaining endpoint-authentication and interrupted-publication blockers.

A malicious process already running with the same host UID is outside the threat
boundary; mode 0700 and these observations do not protect against it. Even an
eventual successful host canary will not establish QEMU/guest isolation, guest
device inventory, networking, artifact closure or workload qualification.

The controller requirements should be checked against upstream
[Bubblewrap options](https://github.com/containers/bubblewrap/blob/main/bwrap.xml),
[systemd resource controls](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html)
and [execution settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)
during final combined review; this directory supplies no runnable
Bubblewrap or systemd command template.
