"""Build and inspect only. This module never executes init or starts a guest."""

import hashlib
import json
import shutil
from pathlib import Path

from archive import archive
from inspect_artifact import inspect_archive

REQUIRED_CONFIG = (
    "BLK_DEV_INITRD",
    "RD_GZIP",
    "KVM_GUEST",
    "INET",
    "IPV6",
    "DEVTMPFS",
    "SERIAL_8250",
    "SERIAL_8250_CONSOLE",
)


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def closure_metadata(registration):
    lines = registration.read_text().splitlines()
    result = []
    while lines:
        if len(lines) < 5:
            raise ValueError("invalid toolchain closure registration")
        path, nar_hash, size, deriver, count = lines[:5]
        count = int(count)
        if (
            deriver
            or count < 0
            or len(lines) < 5 + count
            or not path.startswith("/nix/store/")
        ):
            raise ValueError("invalid toolchain closure record")
        result.append(
            {
                "path": path,
                "nar_hash": nar_hash,
                "nar_bytes": int(size),
                "references": sorted(lines[5 : 5 + count]),
            }
        )
        lines = lines[5 + count :]
    if not result or len({record["path"] for record in result}) != len(result):
        raise ValueError("empty or duplicated toolchain closure")
    return sorted(result, key=lambda record: record["path"])


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def build_artifact(
    *,
    init,
    compiler,
    compiler_flags,
    kernel,
    kernel_config,
    kernel_source,
    toolchain_registration,
    repository_lock,
    output,
):
    """Build from inputs supplied by the locked Nix derivation, not a CLI."""
    source = Path(__file__).resolve().parent
    lock = json.loads(repository_lock.read_text())
    pin = lock["nodes"]["nixpkgs"]["locked"]
    config = kernel_config.read_text()
    if any(f"CONFIG_{name}=y\n" not in config for name in REQUIRED_CONFIG):
        raise ValueError("kernel lacks a required built-in feature")
    output.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(init, output / "init")
    compressed = archive({"dev": b"", "init": (output / "init").read_bytes()})
    inspection = inspect_archive(compressed)
    (output / "initramfs.cpio.gz").write_bytes(compressed)
    shutil.copyfile(kernel, output / "bzImage")
    shutil.copyfile(kernel_config, output / "kernel.config")
    write_json(output / "inspection.json", inspection)
    write_json(
        output / "toolchain-closure.json",
        closure_metadata(toolchain_registration),
    )
    source_names = sorted(
        path.name
        for path in source.iterdir()
        if path.suffix in {".c", ".h", ".py", ".nix", ".json"}
    )
    manifest = {
        "schema_version": 1,
        "nixpkgs": {"revision": pin["rev"], "nar_hash": pin["narHash"]},
        "source_sha256": {name: sha256(source / name) for name in source_names},
        "repository_lock_sha256": sha256(repository_lock),
        "compiler": {
            "path": str(compiler),
            "sha256": sha256(compiler),
            "flags": compiler_flags,
        },
        "kernel_source": {
            "path": str(kernel_source),
            "sha256": sha256(kernel_source),
        },
        "files_sha256": {
            name: sha256(output / name)
            for name in (
                "init",
                "initramfs.cpio.gz",
                "bzImage",
                "kernel.config",
                "inspection.json",
                "toolchain-closure.json",
            )
        },
        "guest_execution": "not_performed",
        "host_isolation": "unverified",
        "qualification": "not_assessed",
        "reproducibility": "not_inferred_from_single_build",
    }
    write_json(output / "manifest.json", manifest)


if __name__ == "__main__":
    raise SystemExit("Run the pinned Nix derivation; direct invocation is unsupported")
