# Host confinement canary plan

Status: static self-inspection probe implemented and tested with synthetic
fixtures only; no host canary or VM run. The controller now exists in
`host-controller/`, but manager operations remain unconditionally blocked pending
a bounded authenticated transport and final review.
This is a #2113/#2106 prerequisite for the local isolated environment, not evidence
of isolation or qualification. See [the probe and exact remaining blockers](host-canary/README.md).

## Plan

Create a small no-VM canary that tests whether the intended host boundary can be
established on this machine. It must use a uniquely owned, mode-0700 run directory,
fixed inputs and bounded output; no credentials, production destinations,
containers, QEMU launch or guest code. The canary may use a transient user service
and a mandatory Bubblewrap namespace boundary, but must fail rather than silently
fall back if the required controls are unavailable.

Verify the actual namespace, mount, descriptor and capability state, and read
back effective per-run CPU, memory, process and file-size limits. Use run-owned
host file/socket sentinels with host-side positive controls and sandbox-side
negative checks. Do not contact real LAN or production endpoints. Report each
check and every unavailable or inconclusive control separately; do not turn a
missing result into a success.

Bind cleanup to the recorded run directory and verified process/service identity.
Reject reused or replaced paths, stale PIDs and unexpected descriptors; do not
perform global pruning or name-only process termination. Preserve bounded evidence
and prove cleanup is safe to repeat.

## Gate

Review the design before implementation and the final code before any host
execution. A successful no-VM canary would establish only its observed host
controls; QEMU, guest device inventory, guest networking, artifact closure and
workload qualification remain separate unverified gates.

## Independent architecture gate

The canary must fail closed when a required control cannot be observed. It must
compare namespaces against a trusted baseline inside the transient service,
verify effective cgroup membership and values, clear the environment before any
dynamic launcher starts, and enumerate the exact mounts, descriptors, identity
maps and capabilities. A cgroup namespace alone does not enforce a limit.
The retained-groups profile requires the trusted service observer to bind and
compare the exact host group vectors; namespace-local group IDs can alias and
cannot authenticate that vector. This profile also requires reviewed syscall
filters and does not establish the QEMU boundary.

Use distinct file, pathname-socket and abstract-socket sentinels, with host
positive controls and a sandbox-local positive socket operation. A pathname
failure alone is not evidence of network isolation. Bound output in the supervisor;
file-size limits do not cap pipes or the journal.

Create unit/run names exclusively, record invocation/cgroup/process identities,
and use anchored, nonrecursive cleanup of an exact owned-file inventory. A
hostile process already running under the same user is outside this canary's
threat boundary; mode 0700 does not protect against that case. An observed canary
pass would not automatically validate the larger QEMU boundary.

## Implemented first component

`host-canary/` builds a static PIE inspection probe with no command/path operands.
It inspects only fixed self-procfs records, descriptors and `RLIMIT_FSIZE`, then
emits bounded framed evidence with separate requested/observed/failed/unavailable
states. Host and QEMU/guest isolation always remain unverified. Pure parser and
frame tests exclude the OS reader; static ELF checks inspect the binary without
executing it. No service, namespace, socket, mount, cleanup or real canary is
started by the build or tests.

The controller source now includes baseline, cgroup, sentinel and cleanup
components, but they have not established live host isolation. Every probe frame
still reports host and guest isolation as unverified. Authenticated manager
transport, safe interrupted-launch reconciliation and live controller validation
remain required before execution; this component does not satisfy that gate.
