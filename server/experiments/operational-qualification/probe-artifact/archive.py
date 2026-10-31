"""Canonical archive writer. Inspection is independently implemented in inspect_artifact.py."""

import struct
import zlib


def archive(entries):
    """Metadata is fixed; caller order, filesystem paths, clocks and umasks are irrelevant."""
    if set(entries) != {"dev", "init"} or entries["dev"] != b"":
        raise ValueError("unexpected archive input")
    result = bytearray()
    for inode, name in enumerate(sorted(entries), 1):
        content = entries[name]
        directory = name == "dev"
        fields = [
            inode,
            0o40755 if directory else 0o100555,
            0,
            0,
            2 if directory else 1,
            0,
            len(content),
            0,
            0,
            0,
            0,
            len(name) + 1,
            0,
        ]
        _entry(result, name, content, fields)
    _entry(result, "TRAILER!!!", b"", [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0])
    compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
    body = compressor.compress(bytes(result)) + compressor.flush()
    # Fixed mtime, no optional fields, maximum compression, OS unknown.
    return (
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff"
        + body
        + struct.pack("<II", zlib.crc32(result), len(result))
    )


def _entry(result, name, content, fields):
    result.extend(
        b"070701" + "".join(f"{field:08x}" for field in fields).encode("ascii")
    )
    result.extend(name.encode("ascii") + b"\0")
    result.extend(b"\0" * (-len(result) % 4))
    result.extend(content)
    result.extend(b"\0" * (-len(result) % 4))
