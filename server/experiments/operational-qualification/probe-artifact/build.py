"""Build and inspect only. This module never executes init or starts a guest."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

from archive import archive
from inspect_artifact import inspect_archive

FLAGS = [
    "-std=c11",
    "-O2",
    "-Wall",
    "-Wextra",
    "-Wpedantic",
    "-Werror",
    "-static",
    "-no-pie",
    "-fno-pie",
    "-fno-ident",
    "-fstack-protector-strong",
    "-D_FORTIFY_SOURCE=2",
    "-frandom-seed=waddle-isolation-probe-v1",
    "-s",
    "-Wl,--build-id=none,-z,noexecstack,-z,relro,-z,now",
]
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


def compile_probe(compiler, source, output):
    # Clear ambient compiler/linker injection; wrapper defaults are in its pinned closure.
    environment = {
        "PATH": os.environ["PATH"],
        "LC_ALL": "C",
        "TZ": "UTC",
        "SOURCE_DATE_EPOCH": "0",
        "NIX_HARDENING_ENABLE": "",
    }
    subprocess.run(
        [
            str(compiler),
            *FLAGS,
            f"-ffile-prefix-map={source}=.",
            f"-fdebug-prefix-map={source}=.",
            str(source / "init.c"),
            str(source / "model.c"),
            "-o",
            str(output),
        ],
        env=environment,
        check=True,
        timeout=120,
    )


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "cc",
        "kernel",
        "kernel-config",
        "kernel-source",
        "toolchain-registration",
        "repository-lock",
        "out",
    ):
        parser.add_argument(f"--{name}", type=Path, required=True)
    arguments = parser.parse_args()
    source = Path(__file__).resolve().parent
    lock = json.loads(arguments.repository_lock.read_text())
    pin = lock["nodes"]["nixpkgs"]["locked"]
    config = arguments.kernel_config.read_text()
    if any(f"CONFIG_{name}=y\n" not in config for name in REQUIRED_CONFIG):
        raise ValueError("kernel lacks a required built-in feature")
    output = arguments.out
    output.mkdir(parents=True, exist_ok=False)
    compile_probe(arguments.cc, source, output / "init")
    compressed = archive({"dev": b"", "init": (output / "init").read_bytes()})
    inspection = inspect_archive(compressed)
    (output / "initramfs.cpio.gz").write_bytes(compressed)
    shutil.copyfile(arguments.kernel, output / "bzImage")
    shutil.copyfile(arguments.kernel_config, output / "kernel.config")
    write_json(output / "inspection.json", inspection)
    write_json(
        output / "toolchain-closure.json",
        closure_metadata(arguments.toolchain_registration),
    )
    source_names = sorted(
        path.name
        for path in source.iterdir()
        if path.suffix in {".c", ".h", ".py", ".nix"}
    )
    manifest = {
        "schema_version": 1,
        "nixpkgs": {"revision": pin["rev"], "nar_hash": pin["narHash"]},
        "source_sha256": {name: sha256(source / name) for name in source_names},
        "repository_lock_sha256": sha256(arguments.repository_lock),
        "compiler": {
            "path": str(arguments.cc),
            "sha256": sha256(arguments.cc),
            "flags": FLAGS,
        },
        "kernel_source": {
            "path": str(arguments.kernel_source),
            "sha256": sha256(arguments.kernel_source),
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
    main()
