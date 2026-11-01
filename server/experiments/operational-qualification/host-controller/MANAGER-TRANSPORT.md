# Authenticated manager transport: incomplete, execution blocked

The current controller is not execution-ready. `manager_transport_required()`
unconditionally rejects run, service, baseline observation and show/stop paths
before any manager connection. This is an implementation blocker, not a mutable
review flag or a request for another user approval. Removing that rejection alone
would be unsafe. The legacy systemd client calls below it are not an accepted
transport and cannot be reached without changing the code.

The host kernel, root administrator, system bus and system/user managers are
external trusted dependencies. The fixed Nix clients do not authenticate their
running images or versions. In particular, the observed host manager version 262
and configured ExecStart do not establish its actual executable inode, whose
procfs lookup is unavailable. There is no claim of manager image authentication.
A malicious same-UID client remains outside the threat model.

## Connection design for a separately reviewed implementation

A fixed helper can connect to `/run/dbus/system_bus_socket`, authenticate that
actual connection's root peer and retain its FD. Over that same bus connection it
must resolve `org.freedesktop.systemd1` to a unique owner, verify the owner's UID0
and PID1, address that owner explicitly, and validate reply sender and owner
stability. It must query `user@<uid>.service`, including MainPID, InvocationID,
active state and ControlGroup. Bind these to externally read boot ID, process
start time, UID, group vector and anchored cgroup identity. Do not attempt the
root-only `/run/systemd/private`, elevate privileges or change socket permissions.

Then connect once to the fixed `/run/user/<uid>/systemd/private`. The actual
connected socket's SO_PEERCRED must match the authenticated user-manager PID and
UID. Hold its FD and a pidfd. Give that exact FD to `sd_bus_set_fd`, in direct-peer
mode with no bus Hello, automatic reconnect, endpoint substitution or fallback.
The helper, not another subprocess, must perform all launch/show/stop calls on
that connection. Revalidate the system-manager unit identity and process tuple
before and after each operation. A separate credential preflight does not bind a
later command's connection. Failure, disconnect, restart, owner change or uncertain
identity must abort, without retrying a launch request or signalling by guessed
PID/unit name.

Use typed, bounded calls: StartTransientUnit `sa(sv)a(sa(sv))` with mode `fail` and
only the compiled fixed unit properties/entrypoint; bounded GetAll `a{sv}`;
InvocationID `ay` exactly 16 bytes, PID `u`, and time properties `t` in microseconds.
Reject missing, duplicate, wrong-type or oversized data and all incoming FDs.
Authenticate and bound errors as well as successes. Per-call and total monotonic
deadlines cover connect, authentication, flush and receive. A returned job path is
not service completion: verify final MainPID/state, Result, ExecMainCode/Status,
InvocationID and the authenticated cgroup's emptiness. Preserve ownership records
across every operation, including uncertain launch outcomes.

## Blocking implementation questions

* **Receive bounds before allocation.** Pinned systemd v261.2's public sd-bus API
  has no per-connection receive-size setter. Its socket reader accepts declared
  messages below 128 MiB and allocates the declared size before application-level
  parsing; authentication permits 64 KiB. A thin helper using those defaults does
  not meet this canary's small independent evidence limits. A fixed bounded
  library override or independently reviewed wire transport is required. Merely
  bounding helper stdout or checking parsed message size is insufficient.
* **Manager reexec.** pidfd, boot ID and start time survive exec, and executable
  inspection is unavailable here. Establish an authenticated generation marker or
  prove from the pinned/host protocol contract that reexec always invalidates the
  held private connection. Do not treat a stable PID/start tuple as reexec proof.
* **Interrupted ownership publication.** The current state-v3 model fsyncs a
  launch-attempt marker before sending, never retries an unknown result, and
  removes only identity-recorded sentinels. It does not safely recover every
  hard-kill prefix before inode identities are persisted, nor reconcile an
  uncertain manager request. That requires a durable publication and authenticated
  reconciliation design; no signal is authorized by an incomplete record.
* **Private protocol and external versions.** The client library is pinned, but
  the running manager is external. Direct-peer behavior, property types and
  closure across reexec need a supported contract plus host verification. No
  version-string equality is a substitute.

No helper or connection experiment ran. Current fake tests prove that blocked
manager entrypoints do not construct a socket, run a command or create a run root;
they do not prove endpoint authentication, owner-change handling or restart
reconciliation. Those protocol fakes and final independent review belong to the
transport implementation. Existing service/PID/cgroup replacement fixtures remain
necessary but are not an alternative to testing the actual transport.

References: [sd_bus_set_fd ownership contract](https://www.freedesktop.org/software/systemd/man/latest/sd_bus_set_fd.html),
[pinned v261.2 public API](https://github.com/systemd/systemd/blob/v261.2/src/systemd/sd-bus.h),
[receive constants](https://github.com/systemd/systemd/blob/v261.2/src/libsystemd/sd-bus/bus-internal.h#L302-L321),
and [receive allocation](https://github.com/systemd/systemd/blob/v261.2/src/libsystemd/sd-bus/bus-socket.c#L1191-L1317).
