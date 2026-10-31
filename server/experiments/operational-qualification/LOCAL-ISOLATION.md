# Local qualification environment plan

Status: design only for #2106, under #2046 W1.8b. The owner selected a local
isolated environment. No VM, cluster, service, workload, fault or production
deployment has been started by this plan.

## Boundary and phases

The proposed outer boundary is a dedicated KVM guest with no virtual NIC,
host mounts, container-runtime sockets, passthrough, vsock or TCP management
listener. Its unprivileged QEMU process must be confined to `/dev/kvm`, the
required runtime files and a private, run-owned disk/socket directory. Disable
implicit devices and user configuration; do not fall back to emulation. Keep
QMP separate from guest serial, accept only fixed management operations and
treat bounded serial output as untrusted data. Validate run-directory and socket
ownership before use. This boundary is a design target, not established evidence.

1. Build a **non-launching** artifact-lock and environment-plan validator. Its
   output must explicitly say that artifact closure, host confinement and
   isolation are unverified; it must not start a process or contact a service.
   Reject unknown/unsafe fields, unbounded resource requests, missing required
   artifact roles, invalid content digests and unsafe identities. Generate only
   a deterministic structured proposal with no arbitrary QEMU or QMP commands.
2. Independently review a confined launcher and controller. Validate the real
   device list, owned paths, fixed management operations, resource limits,
   bounded serial handling and identity-safe cleanup. First boot only a tiny,
   uncredentialed guest to test this boundary.
3. Stage the complete pinned offline guest, kernel, kind/CNI, CNPG/PostgreSQL,
   SpiceDB, shared object store, Colony/OIDC, telemetry, Waddle and generator
   artifacts. Verify hashes and import required images into each kind node;
   missing content must fail closed. No production GitOps manifests,
   credentials, host configuration or user data may enter the guest.
4. Prove isolation using positive guest-local and host-sentinel controls and
   negative probes to run-owned host sentinels and controlled external test
   destinations. Reject production endpoints and credentials statically; do not
   actively probe production. Repeat checks after network changes. Inner CNI
   policy enforcement requires its own canaries and is not the outer boundary.
5. Only then bootstrap stable local identities, real local OIDC/SpiceDB, shared
   persistence and telemetry, followed by two-replica workload qualification
   and individually authorized, identity-bound fault scenarios.

A single physical host cannot establish host-loss resilience. Local results do
not establish UI visibility, XMPP conformance, replicated durability, zero data
loss or production readiness without the separate evidence described in #2106.
