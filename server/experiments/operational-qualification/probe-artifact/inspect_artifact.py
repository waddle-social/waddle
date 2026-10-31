"""Strict byte-level inspector: no extraction, loading, ldd or guest execution."""

import argparse
import hashlib
import json
import struct
import zlib
from pathlib import Path

MAX_INIT = 1024 * 1024
MAX_ARCHIVE = MAX_INIT + 1024
# Pinned Linux config: 4-KiB pages and DEFAULT/LSM_MMAP_MIN_ADDR=65536.
# Use page_64_types.h's DEFAULT_MAP_WINDOW, independent of guest LA57 support.
X86_64_USER_MIN = 65536
X86_64_USER_LIMIT = (1 << 47) - 4096


def fail(message):
    raise ValueError(message)


def inspect_elf(data):
    if len(data) < 64 or len(data) > MAX_INIT:
        fail("ELF size")
    values = struct.unpack_from("<16sHHIQQQIHHHHHH", data)
    (
        ident,
        kind,
        machine,
        version,
        entry,
        phoff,
        shoff,
        flags,
        ehsize,
        phsize,
        phnum,
        shsize,
        shnum,
        shstr,
    ) = values
    if (
        ident != b"\x7fELF\x02\x01\x01" + b"\0" * 9
        or kind != 2
        or machine != 62
        or version != 1
        or flags != 0
        or ehsize != 64
        or not X86_64_USER_MIN <= entry < X86_64_USER_LIMIT
        or phsize != 56
        or not 1 <= phnum <= 32
        or phoff < 64
        or phoff + phsize * phnum > len(data)
    ):
        fail("ELF header or architecture")
    executable_entry = False
    for index in range(phnum):
        kind, flags, offset, virtual, _physical, filesz, memsz, align = (
            struct.unpack_from("<IIQQQQQQ", data, phoff + index * phsize)
        )
        if offset + filesz > len(data) or filesz > memsz:
            fail("ELF segment bounds")
        if kind == 3:
            fail("PT_INTERP is forbidden")
        if kind == 2:
            _dynamic(data[offset : offset + filesz])
        if kind == 1:
            # Subtraction bounds both the full memory mapping (including BSS)
            # and uint64 addition, without ever accepting a wrapped endpoint.
            if (
                not X86_64_USER_MIN <= virtual < X86_64_USER_LIMIT
                or memsz > X86_64_USER_LIMIT - virtual
            ):
                fail("ELF load segment virtual address range")
            if align not in (0, 1) and (
                align & (align - 1) or virtual % align != offset % align
            ):
                fail("ELF segment alignment")
            if flags & 3 == 3:
                fail("writable executable ELF segment")
            if flags & 1 and virtual <= entry < virtual + filesz:
                executable_entry = True
        if kind == 0x6474E551 and flags & 1:
            fail("executable stack")
    if not executable_entry:
        fail("ELF entry outside executable file segment")
    if (
        shoff == 0
        or shsize != 64
        or not 1 <= shnum <= 128
        or shstr >= shnum
        or shoff + shsize * shnum > len(data)
    ):
        fail("ELF section table")
    for index in range(shnum):
        _name, kind, _flags, _address, offset, size, _link, _info, _align, _entsize = (
            struct.unpack_from("<IIQQQQIIQQ", data, shoff + index * shsize)
        )
        if kind != 8 and offset + size > len(data):
            fail("ELF section bounds")
        if kind == 6:
            _dynamic(data[offset : offset + size])
    return {
        "format": "elf64-little-endian",
        "machine": "x86_64",
        "type": "ET_EXEC",
        "entry": entry,
        "pt_interp": False,
        "dt_needed": False,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
    }


def _dynamic(data):
    if not data or len(data) % 16:
        fail("ELF dynamic bounds")
    terminated = False
    for tag, value in struct.iter_unpack("<qQ", data):
        if tag == 1:
            fail("DT_NEEDED is forbidden")
        if terminated and (tag or value):
            fail("ELF data after DT_NULL")
        if tag == 0:
            terminated = True
    if not terminated:
        fail("ELF unterminated dynamic table")


def inspect_archive(compressed):
    if not 18 <= len(compressed) <= MAX_ARCHIVE:
        fail("gzip size")
    if compressed[:10] != b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff":
        fail("noncanonical gzip header")
    inflater = zlib.decompressobj(31)
    try:
        raw = inflater.decompress(compressed, MAX_ARCHIVE + 1)
    except zlib.error:
        fail("malformed gzip")
    if (
        len(raw) > MAX_ARCHIVE
        or not inflater.eof
        or inflater.unused_data
        or inflater.unconsumed_tail
    ):
        fail("oversized, truncated or concatenated gzip")
    cursor, names, probe = 0, [], None
    expected = {
        "dev": (1, 0o40755, 2),
        "init": (2, 0o100555, 1),
        "TRAILER!!!": (0, 0, 1),
    }
    while cursor < len(raw):
        if len(raw) - cursor < 110 or raw[cursor : cursor + 6] != b"070701":
            fail("newc header")
        header = raw[cursor + 6 : cursor + 110]
        if any(byte not in b"0123456789abcdef" for byte in header):
            fail("newc field encoding")
        fields = [int(header[index : index + 8], 16) for index in range(0, 104, 8)]
        (
            inode,
            mode,
            uid,
            gid,
            links,
            mtime,
            size,
            devmajor,
            devminor,
            rmajor,
            rminor,
            namesize,
            checksum,
        ) = fields
        if not 1 <= namesize <= 32 or size > MAX_INIT:
            fail("newc entry bounds")
        cursor += 110
        end_name = cursor + namesize
        if (
            end_name > len(raw)
            or raw[end_name - 1] != 0
            or b"\0" in raw[cursor : end_name - 1]
        ):
            fail("newc filename")
        try:
            name = raw[cursor : end_name - 1].decode("ascii")
        except UnicodeDecodeError:
            fail("newc filename encoding")
        if name not in expected or name in names:
            fail("unexpected or duplicate newc path")
        if (inode, mode, links) != expected[name] or any(
            (uid, gid, mtime, devmajor, devminor, rmajor, rminor, checksum)
        ):
            fail("newc metadata, link or type")
        cursor = _padding(raw, end_name)
        if (
            cursor + size > len(raw)
            or (name != "init" and size != 0)
            or (name == "init" and size == 0)
        ):
            fail("newc content bounds")
        if name == "init":
            probe = inspect_elf(raw[cursor : cursor + size])
        cursor = _padding(raw, cursor + size)
        names.append(name)
        if name == "TRAILER!!!":
            if cursor != len(raw):
                fail("data or second archive after newc trailer")
            break
    if names != ["dev", "init", "TRAILER!!!"] or probe is None:
        fail("newc inventory or order")
    return {
        "schema_version": 1,
        "inventory": ["dev/", "init"],
        "init": probe,
        "initramfs_sha256": hashlib.sha256(compressed).hexdigest(),
        "compressed_bytes": len(compressed),
        "uncompressed_bytes": len(raw),
        "guest_execution": "not_performed",
        "host_isolation": "unverified",
    }


def _padding(data, cursor):
    aligned = (cursor + 3) & ~3
    if aligned > len(data) or any(data[cursor:aligned]):
        fail("newc padding")
    return aligned


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    arguments = parser.parse_args()
    with arguments.archive.open("rb") as stream:
        data = stream.read(MAX_ARCHIVE + 1)
    try:
        result = inspect_archive(data)
    except ValueError as error:
        parser.exit(1, f"artifact rejected: {error}\n")
    print(json.dumps(result, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
