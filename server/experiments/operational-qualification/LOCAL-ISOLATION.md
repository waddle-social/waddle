# Local qualification environment plan

Status: offline declaration validator and proposal only for #2106/#2110, under
#2046 W1.8b. The owner selected a local isolated environment. No VM, cluster,
service, workload, fault or production deployment has been started by this plan.

## Boundary and phases

The proposed outer boundary is a dedicated KVM guest with no virtual NIC,
host mounts, container-runtime sockets, passthrough, vsock or TCP management
listener. Its unprivileged QEMU process must be confined to `/dev/kvm`, the
required runtime files and a private, run-owned disk/socket directory. Disable
implicit devices and user configuration; do not fall back to emulation. Keep
QMP separate from guest serial, accept only fixed management operations and
treat bounded serial output as untrusted data. Validate run-directory and socket
ownership before use. This boundary is a design target, not established evidence.

1. The **non-launching** declaration validator is implemented as the
   `local-isolation` command in this experiment. It validates the strict typed
   declaration and emits a deterministic structured proposal. Digest bytes,
   artifact closure, host confinement and isolation remain unverified, and
   `execution_authorization` is always `not_authorized`. This status means the
   validator cannot authorize execution; it does not request additional user
   approval. It starts no process, opens no artifact or host path, and contacts
   no service. Unknown/unsafe fields, out-of-bound resource requests, missing or
   duplicate roles, invalid digest syntax and unsafe identities are rejected.
   There are no arbitrary QEMU arguments, QMP commands or host-path operands.
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
   and individually scoped, identity-bound fault scenarios.

## Implemented declaration contract

`local-isolation` reads at most 64 KiB of JSON from stdin. All fields are
required, all object fields and enum variants are closed, and errors do not echo
input contents. Records must be JSON objects; positional arrays are rejected at
the root, resource, artifact, additional-artifact and digest-record boundaries.
Map-only deserialization passes original keys directly to the strict field
decoder, preserving duplicate-key rejection without a `Value` conversion.
The declaration contains schema version 1, a synthetic numeric run ID in
`1..=4294967295`, a purpose, an explicit `x86_64` or `aarch64` guest architecture,
resources, the required `artifacts` and an `additional_artifacts` list.

The purposes have distinct minimum inventories and inclusive resource bounds:

| Purpose | vCPUs | Memory (MiB) | Disk (GiB) | Required inventory |
| --- | --- | --- | --- | --- |
| `isolation_probe` | 1–2 | 128–1,024 | exactly 0 | `host_qemu_binary`, `guest_kernel`, `guest_initrd` |
| `qualification` | 1–16 | 1,024–49,152 | 8–128 | probe roles plus all qualification roles below |

The probe design is a diskless kernel/initrd guest with its entire tiny root
filesystem in memory; no service inventory, kind node or persistent guest disk
is needed or accepted. It remains a proposal, not a bootable configuration.
Qualification adds `guest_disk`, `kind_binary`, `kind_node`, `cni`,
`cnpg_operator`, `postgres`, `spicedb`, `object_store`, `colony`, `oidc`,
`telemetry_collector`, `telemetry_store`, `waddle` and `generator`. Each required
role occurs exactly once and must belong to the selected purpose. The reviewed
16-vCPU/48-GiB proposal fits within the qualification bounds. These bounds do not
allocate resources or establish capacity, host availability or bootability.

Required-role content types distinguish artifacts with a fixed representation
from tools and services that can be staged as a file or container image:

| Digest declaration type | Required roles |
| --- | --- |
| `file_bytes` only | `host_qemu_binary`, `guest_kernel`, `guest_initrd`, `guest_disk`, `kind_binary` |
| `oci_manifest` only | `kind_node`, `cnpg_operator` |
| Either type | `cni`, `postgres`, `spicedb`, `object_store`, `colony`, `oidc`, `telemetry_collector`, `telemetry_store`, `waddle`, `generator` |

`additional_artifacts` accepts 0–64 immutable declarations with unique synthetic
numeric `id` values in `1..=65535`. Additional roles can repeat; their numeric
IDs distinguish multiple artifacts without accepting arbitrary names or paths.
`host_runtime_file`, `guest_runtime_file`, `oci_blob` and
`offline_configuration` require `file_bytes`; `operator_image` and
`service_image` require `oci_manifest`. Probe declarations permit only additional
host/guest runtime files. Qualification can declare additional operator images,
shared libraries, configuration and OCI blobs. The `oci_blob` digest describes
the exact stored blob bytes, including compression when present. Every required
and additional artifact declares the same architecture as the guest, including
host QEMU/runtime files because this design forbids cross-architecture emulation.
That agreement is only a declaration: host compatibility and the real artifact
architecture remain unverified.

Each digest must use `sha256:` plus exactly 64 lowercase hexadecimal digits;
the all-zero digest sentinel is rejected. Rejecting that sentinel does not make
any other value trustworthy or prove content availability.
`file_bytes` declares a hash of file bytes; `oci_manifest` declares a hash of an
OCI image manifest, not a downloaded archive, tag or multi-architecture index.
The validator never reads or hashes those bytes, resolves an image, checks a
manifest's real type/platform, or authenticates provenance. A manifest digest
does not itself prove that its configuration, layers or other referenced content
is available. Operators, CNI, telemetry and services can require multiple images,
charts, configuration and further dependencies. The bounded additional inventory
allows those declarations, but it carries no dependency graph or proof that all
references have been enumerated. This is a declaration only and does **not**
verify the complete transitive offline closure. A later artifact-lock design must
verify exact content and complete dependency relationships; this format cannot
be used to stage or launch a guest.

The proposal sorts required artifacts by role and additional artifacts by numeric
ID. It always emits the same fixed external boundary
requirements: KVM with an unprivileged, independently confined QEMU process;
no emulation fallback, implicit devices, user configuration, NICs, host shares,
host runtime sockets, device passthrough or vsock. Management requires an owned
private local socket, ownership validation, a separate untrusted serial channel
with a 1 MiB capture limit, and no TCP listener. Only the abstract operations
`inspect_status`, `request_shutdown` and `force_stop` are listed. These are
requirements for a later reviewed controller, not QMP payloads or implemented
operations. No shell command, executable argument list, socket/disk location,
launcher configuration, confinement policy or applied isolation is produced.

`examples/isolation-probe.json` demonstrates the three-role diskless probe.
`examples/local-isolation.json` demonstrates a 16-vCPU/48-GiB qualification
proposal, a file-based generator and additional runtime/operator artifacts.
Both contain nonzero synthetic digest fixtures derived from label strings, not
verified VM or service artifacts. Their accepted syntax demonstrates this
contract only. Every proposal marks digest
verification, artifact closure, architecture compatibility, host confinement and
isolation `unverified`, execution `not_authorized` and qualification `not_assessed`.
The next step remains the independently reviewed confined launcher/controller
and a tiny uncredentialed boundary test described in phase 2.

A single physical host cannot establish host-loss resilience. Local results do
not establish UI visibility, XMPP conformance, replicated durability, zero data
loss or production readiness without the separate evidence described in #2106.
