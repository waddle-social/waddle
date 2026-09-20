#!/usr/bin/env python3
"""Balance whole nextest binaries by ELF size and replicate explicitly shared ones.

Usage: plan_nextest_binary_shards.py FULL_INVENTORY OUTPUT_DIRECTORY [--count 4]
                                    [--shared-binary BINARY_ID ...]

The input is an unfiltered `cargo nextest list --message-format json` inventory.
Its binary paths must still exist. The output directory receives plan.json and
partition-N.filter for each archive's owned and shared binaries, and
partition-N.whole.filter for only its owned binaries. shared.filter selects the
shared binaries, whose execution uses nextest --partition hash:N/COUNT.
Nonshared binaries are assigned exactly once, including empty and ignored-only
binaries. plan.json records unique source bytes, replicated shared bytes, and
each archive's members and bytes. Check the actual whole and shared inventories
with check_nextest_shards.py, passing the same --shared-binary options.

Inventory and output paths must remain within the current working directory or
the Nix build's explicit `out` directory. ELF paths must remain within
`CARGO_TARGET_DIR` (default: ./target). These roots come from the build caller,
not from inventory contents or CLI path arguments.
"""

import argparse
import json
import os
from pathlib import Path
import stat
import tempfile


def confined_path(value, roots):
    """Resolve traversal and symlinks before checking trusted caller roots."""
    path = Path(value).resolve()
    if not any(path.is_relative_to(root) for root in roots):
        raise ValueError("path is outside the permitted build directories")
    return path


def build_file_roots():
    roots = [Path.cwd().resolve()]
    if os.environ.get("out"):
        roots.append(Path(os.environ["out"]).resolve())
    return roots


def binary_filter(binary_ids):
    """Use nextest's exact matcher and documented Unicode escapes, never globs."""
    predicates = []
    for binary_id in sorted(binary_ids):
        if not isinstance(binary_id, str) or not binary_id:
            raise ValueError("binary IDs must be nonempty strings")
        encoded = []
        for character in binary_id:
            codepoint = ord(character)
            if 0xD800 <= codepoint <= 0xDFFF:
                raise ValueError("binary IDs must contain valid Unicode scalars")
            if character.isascii() and (character.isalnum() or character in "_-.:"):
                encoded.append(character)
            else:
                encoded.append(f"\\u{{{codepoint:x}}}")
        predicates.append(f"binary_id(={''.join(encoded)})")
    if not predicates:
        raise ValueError("cannot create a filter for an empty shard")
    return " | ".join(predicates)


def plan(inventory_path, count=4, shared_binary_ids=()):
    if count < 1:
        raise ValueError("shard count must be positive")
    inventory_path = confined_path(inventory_path, build_file_roots())
    if not inventory_path.is_file():
        raise ValueError("inventory must be a regular file")
    document = json.loads(inventory_path.read_text())
    target_directory = Path(os.environ.get("CARGO_TARGET_DIR", "target")).resolve()
    binaries = []
    for binary_id, suite in document["rust-suites"].items():
        if suite["binary-id"] != binary_id:
            raise ValueError(f"{binary_id}: binary-id differs from its inventory key")
        if suite["status"] != "listed":
            raise ValueError(f"{binary_id}: the full inventory must list every binary")
        binary_filter([binary_id])
        path = confined_path(suite["binary-path"], [target_directory])
        metadata = path.stat()
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"{binary_id}: binary path is not a regular file: {path}")
        with path.open("rb") as binary:
            if binary.read(4) != b"\x7fELF":
                raise ValueError(f"{binary_id}: binary path is not an ELF file: {path}")
        binaries.append({"binary_id": binary_id, "bytes": metadata.st_size})
    shared_ids = set(shared_binary_ids)
    unknown = shared_ids - {binary["binary_id"] for binary in binaries}
    if unknown:
        raise ValueError(f"unknown shared binary IDs: {sorted(unknown)}")
    shared = sorted(
        (binary for binary in binaries if binary["binary_id"] in shared_ids),
        key=lambda item: item["binary_id"],
    )
    owned = [binary for binary in binaries if binary["binary_id"] not in shared_ids]
    shared_bytes = sum(binary["bytes"] for binary in shared)
    if len(owned) < count:
        raise ValueError(f"cannot fill {count} whole-binary shards with only {len(owned)} nonshared binaries")

    shards = [
        {"index": index, "bytes": 0, "binaries": []}
        for index in range(1, count + 1)
    ]
    for binary in sorted(owned, key=lambda item: (-item["bytes"], item["binary_id"])):
        shard = min(shards, key=lambda item: (item["bytes"], item["index"]))
        shard["binaries"].append(binary)
        shard["bytes"] += binary["bytes"]
    for shard in shards:
        shard["binaries"].sort(key=lambda item: item["binary_id"])
        shard["whole_binaries"] = shard["binaries"]
        shard["whole_bytes"] = shard["bytes"]
        shard["whole_filter"] = binary_filter(item["binary_id"] for item in shard["whole_binaries"])
        shard["binaries"] = sorted(shard["whole_binaries"] + shared, key=lambda item: item["binary_id"])
        shard["bytes"] += shared_bytes
        shard["filter"] = binary_filter(item["binary_id"] for item in shard["binaries"])
    return {
        "schema_version": 2,
        "algorithm": "greedy-largest-binary-first",
        "shard_count": count,
        "binary_count": len(binaries),
        "bytes": sum(binary["bytes"] for binary in binaries),
        "shared_binaries": shared,
        "shared_bytes": shared_bytes,
        "shared_filter": binary_filter(shared_ids) if shared_ids else "none()",
        "shards": shards,
    }


def write_plan(result, directory):
    directory = confined_path(directory, build_file_roots())
    directory.mkdir(parents=True, exist_ok=True)
    files = {"shared.filter": result["shared_filter"] + "\n",
             "plan.json": json.dumps(result, indent=2) + "\n"}
    for shard in result["shards"]:
        files[f"partition-{shard['index']}.filter"] = shard["filter"] + "\n"
        files[f"partition-{shard['index']}.whole.filter"] = shard["whole_filter"] + "\n"
    # Validate every destination before writing any files. Atomic replacement
    # also prevents an existing hard link from overwriting a file elsewhere.
    paths = {}
    for name in files:
        path = directory / name
        if path.is_symlink():
            raise ValueError("plan output files must not be symlinks")
        paths[name] = confined_path(path, [directory])
    for name, contents in files.items():
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(mode="w", dir=directory, delete=False) as stream:
                temporary = Path(stream.name)
                stream.write(contents)
            temporary.replace(paths[name])
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory", type=Path)
    parser.add_argument("output_directory", type=Path)
    parser.add_argument("--count", type=int, default=4)
    parser.add_argument("--shared-binary", action="append", default=[])
    args = parser.parse_args()
    try:
        result = plan(args.inventory, args.count, args.shared_binary)
        write_plan(result, args.output_directory)
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.exit(1, f"cannot plan nextest binary shards: {error}\n")


if __name__ == "__main__":
    main()
