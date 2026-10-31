# Diskless isolation-probe artifact

Status: build-only artifact for #2106/#2112. This document does not launch a VM or
establish host confinement. The user selected a local isolated qualification
environment; no guest, service, workload or fault has run.

## Artifact and build

The uncredentialed initial guest uses the repository-pinned Linux kernel and a
small statically linked PID 1 probe. Its initramfs contains only `init` and an
empty `dev/` mount directory: no shell, host configuration, credentials, package
manager, disks, user accounts or services.

The standalone [default.nix](probe-artifact/default.nix) imports the exact nixpkgs
revision and NAR hash from the unchanged root `flake.lock`. Its Linux 6.18 kernel,
configuration, static musl GCC compiler, Python and zlib all come from that pin.
Only x86_64 Linux is implemented. No host kernel configuration or initrd is read.
From the repository root, build without creating a launcher or booting anything:

```sh
nix-build server/experiments/operational-qualification/probe-artifact/default.nix --no-out-link
```

The output has `bzImage`, `kernel.config`, `init`, `initramfs.cpio.gz`,
`inspection.json`, `toolchain-closure.json` and `manifest.json`. The manifest
records actual source-file, repository-lock, compiler-wrapper, kernel-source,
kernel/configuration, probe/archive and inspection hashes. The toolchain closure
lists store paths, NAR hashes, sizes and references for the compiler and archive
runtime. These are build inputs and provenance; only the two allowlisted entries
enter the initramfs. Neither a QEMU binary nor a launcher is built here.

Archive construction fixes path order, uid/gid, modes, link counts, inode numbers,
mtime, device fields and padding. It uses a single newc archive, one gzip member,
no gzip filename or timestamp, and a fixed compression level. Nix fixups are
disabled after hashing so the manifest describes the final bytes.

## Future guest behavior

`init.c` checks `getpid() == 1` before any mount, descriptor mutation or socket
operation; host tests never execute that entry point, including its refusal path.
As PID 1 it installs a 20-second watchdog, explicitly mounts a fresh devtmpfs,
opens only `/dev/console` for nonblocking output and disconnects stdin. It needs
neither procfs nor sysfs. Mount/console setup failure halts without a success
report. It accepts no arguments, environment instructions, kernel-command-line
instructions, command, host path, URL, DNS name or arbitrary network target.

Fixed rtnetlink requests inventory links, addresses and routes before and after
enabling `lo`. Each dump is limited to 8,192 bytes and eight receive attempts,
each with a 250-ms timeout. Exact request payloads use their respective
`ifinfomsg`, `ifaddrmsg` and `rtmsg` layouts. Wrong/truncated/interrupted messages,
unexpected interfaces/addresses/routes, gateways and unhandled routing forms
prevent socket checks. The post-change inventory also requires an up loopback
and both IPv4 and IPv6 addresses/routes. Unknown routing attributes can cause a
conservative rejection; this parser is intentionally narrow and is not a general
network inventory tool.

Only after both inventories meet that contract does the probe send and receive
one fixed UDP payload through each of `127.0.0.1` and `::1`. Both families use the
same protocol, port and payload. A family's successful positive control permits a
UDP `connect` route lookup to its fixed documentation address (`192.0.2.1` or
`2001:db8::1`). There is no send to either documentation address. Only an exact
`ENETUNREACH` result is expected; timeouts, permission errors, other errno values,
success, missing IPv6 and incomplete inventory cannot pass.

One versioned JSON record is bounded to 102,400 bytes. Each inventory retains
the exact native little-endian rtnetlink records as hex, including every kernel
attribute and numeric flags, plus its classification. Hex encoding prevents raw
interface names or other kernel bytes becoming terminal instructions. Any
partial/failed output produces an inconclusive failure record and halts. Normal
completion powers off; if that fails it halts, then exits PID 1 as a kernel
fail-stop. It never loops waiting for commands or returns after its report.
Every report leaves host isolation `unverified` and qualification `not_assessed`.
Serial output remains untrusted data for the future bounded reader, never code
or terminal content. Host sentinels and their positive controls are separate work.

## Verification and limits

The independent inspector parses exact gzip/newc bytes without extraction. It
rejects extra gzip members, extra archives/trailing bytes, duplicate/traversal/
absolute/unknown paths, symlinks/hardlinks, wrong ownership/modes/times/devices,
nonzero padding, malformed entries and oversized decompression. It structurally
checks ELF class/endianness/machine/type, table and segment bounds, executable
entry point, stack/segment permissions, `PT_INTERP` and `DT_NEEDED`. Entry points
and complete `PT_LOAD` memory ranges (including BSS) must stay in
`[65536, (1 << 47) - 4096)`, with a segment endpoint allowed exactly at the upper
bound. This conservatively follows the pinned 4-KiB-page kernel's configured
minimum mapping address and four-level `DEFAULT_MAP_WINDOW`; it does not rely on
LA57. Subtraction-based endpoint validation rejects uint64 overflow and mappings
into the noncanonical hole or kernel half. It never uses
`ldd`, loads the ELF or executes it.

Pure C fixtures link only `model.c` and cover request layout, complete IPv4/IPv6
inventory, missing IPv6, wrong errno, truncation/interruption, unknown interfaces,
inconclusive classification and output overflow. Python tests exercise adversarial
archive/ELF fixtures, deterministic repacks across order/timestamps/umasks/paths,
and two separate compiler/linker invocations from perturbed source directories.
Those comparisons establish probe/initramfs rebuild equality for one fixed
toolchain. They do not independently rebuild Linux, the compiler or its closure,
or establish full kernel/toolchain reproducibility. [Local build evidence](probe-artifact-evidence/verification.json),
[exact hashes](probe-artifact-evidence/manifest.json) and [byte inspection](probe-artifact-evidence/inspection.json)
are recorded alongside this document; runtime bootability and socket behavior remain
unverified until the separately reviewed first-boot phase.

## Gates

This slice builds and inspects artifacts only. Before the first boot, a separate
implementation must prove the host confinement canary, effective CPU/memory/PID
limits, exact owned process and management identities, no NIC or shared host
resources, bounded serial handling and cleanup. It must be independently reviewed
and fail closed on missing evidence. A successful guest build alone proves none
of those properties, workload qualification, or production readiness.
