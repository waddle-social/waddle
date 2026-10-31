# Host confinement canary plan

Status: proposed, no canary or VM run yet. This is a #2106 prerequisite for the
local isolated environment, not evidence of isolation or qualification.

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
