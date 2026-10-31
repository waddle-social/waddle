# Diskless isolation-probe artifact

Status: proposed build for #2106. This document does not launch a VM or establish
host confinement. The user selected a local isolated qualification environment;
no guest, service, workload or fault has run.

## Plan

Build a reproducible, uncredentialed initial guest from the repository-pinned
Linux kernel and a small statically linked PID 1 probe. The initramfs contains
only the reviewed probe and minimal empty mount directories: no shell, host
configuration, credentials, package manager, disks, user accounts or services.
Pin and record source/toolchain, kernel, probe and initramfs hashes. Verify the
archive inventory and static executable; do not infer transitive closure merely
from a declared digest.

The guest probe's fixed behavior is to report bounded machine-readable evidence
about its interfaces, addresses and routes, exercise its own loopback as a
positive control, and report no-route results for synthetic documentation-range
addresses. It accepts no command, host path, URL or arbitrary network target.
Its output is untrusted data and is never executed or rendered as a terminal.
Host-side sentinels and their positive controls are separate later work.

## Gates

This slice builds and inspects artifacts only. Before the first boot, a separate
implementation must prove the host confinement canary, effective CPU/memory/PID
limits, exact owned process and management identities, no NIC or shared host
resources, bounded serial handling and cleanup. It must be independently reviewed
and fail closed on missing evidence. A successful guest build alone proves none
of those properties, workload qualification, or production readiness.
