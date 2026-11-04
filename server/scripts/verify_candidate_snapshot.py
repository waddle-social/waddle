#!/usr/bin/env python3
"""Verify a materialized candidate against every entry in its Git tree."""
from collections import deque
import hashlib
import os
from pathlib import Path
import stat
import subprocess
import sys


def fail(message: str) -> None:
    raise ValueError(f"candidate source snapshot: {message}")


def validate_symlink(source: Path, path: Path) -> None:
    """Resolve each link component without ever leaving the store source."""
    relative = path.relative_to(source)
    pending = deque(relative.parts)
    resolved = []
    traversals = 0
    while pending:
        component = pending.popleft()
        if component in {"", "."}:
            continue
        if component == "..":
            if not resolved:
                fail(f"symlink escapes source: {relative}")
            resolved.pop()
            continue
        current = source.joinpath(*resolved, component)
        try:
            mode = current.lstat().st_mode
        except (FileNotFoundError, NotADirectoryError):
            resolved.append(component)
            continue
        if not stat.S_ISLNK(mode):
            resolved.append(component)
            continue
        traversals += 1
        if traversals > 40:
            fail(f"symlink loop or too many symlinks: {relative}")
        target = os.readlink(current)
        if os.path.isabs(target):
            fail(f"symlink escapes source: {relative}")
        pending.extendleft(reversed(Path(target).parts))


def verify(candidate: str, source: Path) -> None:
    git_environment = {**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"}
    object_format = subprocess.check_output(
        ["git", "rev-parse", "--show-object-format"], text=True, env=git_environment
    ).strip()
    tree = subprocess.check_output(
        ["git", "ls-tree", "-r", "-t", "-z", "--full-tree", candidate], env=git_environment
    )
    expected = {}
    for record in tree.split(b"\0"):
        if not record:
            continue
        metadata, path = record.split(b"\t", 1)
        mode, kind, object_id = metadata.split(b" ")
        if (mode, kind) not in {
            (b"040000", b"tree"), (b"100644", b"blob"),
            (b"100755", b"blob"), (b"120000", b"blob"),
        }:
            fail(f"unsupported Git tree entry: {os.fsdecode(path)}")
        expected[os.fsdecode(path)] = (mode, object_id.decode("ascii"))

    seen = set()

    def visit(directory: Path) -> None:
        with os.scandir(directory) as entries:
            for entry in entries:
                path = Path(entry.path)
                relative = path.relative_to(source).as_posix()
                if relative not in expected:
                    fail(f"unexpected entry: {relative}")
                mode, object_id = expected[relative]
                seen.add(relative)
                info = entry.stat(follow_symlinks=False)
                if mode == b"040000":
                    if not stat.S_ISDIR(info.st_mode):
                        fail(f"expected directory: {relative}")
                    visit(path)
                    continue
                if mode == b"120000":
                    if not stat.S_ISLNK(info.st_mode):
                        fail(f"expected symlink: {relative}")
                    validate_symlink(source, path)
                    content = os.fsencode(os.readlink(path))
                    digest = hashlib.new(object_format, b"blob " + str(len(content)).encode() + b"\0")
                    digest.update(content)
                else:
                    if not stat.S_ISREG(info.st_mode) or bool(info.st_mode & 0o111) != (mode == b"100755"):
                        fail(f"unexpected file type or executable mode: {relative}")
                    digest = hashlib.new(object_format, b"blob " + str(info.st_size).encode() + b"\0")
                    with path.open("rb") as content:
                        for chunk in iter(lambda: content.read(1024 * 1024), b""):
                            digest.update(chunk)
                if digest.hexdigest() != object_id:
                    fail(f"content differs from Git tree: {relative}")

    visit(source)
    missing = expected.keys() - seen
    if missing:
        fail(f"missing entry: {sorted(missing)[0]}")


if __name__ == "__main__":
    try:
        verify(sys.argv[1], Path(sys.argv[2]))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
