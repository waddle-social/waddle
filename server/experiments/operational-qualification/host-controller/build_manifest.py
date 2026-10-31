"""Derivation-internal manifest generator; no path-taking command-line interface."""

import hashlib
import json
import os
from pathlib import Path

SOURCES = (
    "controller.py",
    "runtime.py",
    "policy.py",
    "observe.py",
    "bootstrap.c",
    "witness.c",
    "filter.c",
    "filter.h",
)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    paths = json.loads(Path("build/runtime-paths.json").read_text())
    destination = Path(os.environ["out"])
    runtime = destination / "libexec/host-controller"
    if not destination.is_absolute() or not str(destination).startswith("/nix/store/"):
        raise ValueError("Nix derivation output required")
    # Nix-generated references, not caller-selected runtime command operands.
    paths = {name: str(Path(path).resolve(strict=True)) for name, path in paths.items()}
    paths.update(
        {
            "bootstrap": str(destination / "bin/host-confinement-canary"),
            "witness": str(runtime / "witness"),
        }
    )
    if runtime.exists():
        config = {
            "paths": paths,
            "digests": {name: digest(path) for name, path in paths.items()},
            "sources": {name: digest(runtime / name) for name in SOURCES},
        }
        (runtime / "config.json").write_text(
            json.dumps(config, sort_keys=True, indent=2) + "\n"
        )
    else:
        Path("build/paths.h").write_text(
            "#define PYTHON_PATH " + json.dumps(paths["python"]) + "\n"
            "#define CONTROLLER_PATH "
            + json.dumps(str(runtime / "controller.py"))
            + "\n"
        )


if __name__ == "__main__":
    main()
