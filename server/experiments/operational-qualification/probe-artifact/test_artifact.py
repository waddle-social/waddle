import os
import runpy
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import patch

from archive import archive
from inspect_artifact import MAX_ARCHIVE, inspect_archive, inspect_elf


def elf():
    data = bytearray(256)
    struct.pack_into(
        "<16sHHIQQQIHHHHHH",
        data,
        0,
        b"\x7fELF\x02\x01\x01" + b"\0" * 9,
        2,
        62,
        1,
        0x400078,
        64,
        128,
        0,
        64,
        56,
        1,
        64,
        1,
        0,
    )
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, 0x400000, 0x400000, 256, 256, 4096)
    return data


def compress(raw):
    compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
    return (
        b"\x1f\x8b\x08\0\0\0\0\0\x02\xff"
        + compressor.compress(raw)
        + compressor.flush()
        + struct.pack("<II", zlib.crc32(raw), len(raw))
    )


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.good = archive({"dev": b"", "init": bytes(elf())})
        self.raw = zlib.decompress(self.good, 31)

    def test_build_script_rejects_direct_cli_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "unwanted-output"
            with (
                patch.object(sys, "argv", ["build.py", "--out", str(output)]),
                self.assertRaises(SystemExit),
            ):
                runpy.run_path(
                    str(Path(__file__).parent / "build.py"), run_name="__main__"
                )
            self.assertFalse(output.exists())

    def reject(self, value):
        with self.assertRaises(ValueError):
            inspect_archive(value)

    def test_inventory_and_static_structure(self):
        report = inspect_archive(self.good)
        self.assertEqual(report["inventory"], ["dev/", "init"])
        self.assertFalse(report["init"]["pt_interp"])
        self.assertFalse(report["init"]["dt_needed"])
        self.assertEqual(report["guest_execution"], "not_performed")

    def test_gzip_second_member_trailing_bytes_truncation_crc_and_header(self):
        for bad in (
            self.good + self.good,
            self.good + b"\0",
            self.good[:-1],
            self.good[:-8] + b"\0" * 8,
            b"bad" + self.good[3:],
        ):
            with self.subTest(size=len(bad)):
                self.reject(bad)

    def test_archive_after_trailer_is_rejected(self):
        self.reject(compress(self.raw + self.raw))
        self.reject(compress(self.raw + b"\0" * 512))

    def test_duplicate_traversal_absolute_and_extra_paths(self):
        self.reject(compress(self.raw[:116] + self.raw))
        for name in (b"dev\0", b"../x\0", b"/ini\0", b"evil\0"):
            raw = self.raw.replace(b"init\0", name, 1)
            self.reject(compress(raw))

    def test_symlink_hardlink_wrong_owner_time_mode_inode_device_checksum(self):
        # The init header starts immediately after the 116-byte dev record.
        for field, value in (
            (0, 3),
            (1, 0o120777),
            (1, 0o100755),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 1),
            (7, 1),
            (8, 1),
            (9, 1),
            (10, 1),
            (12, 1),
        ):
            raw = bytearray(self.raw)
            start = 116 + 6 + field * 8
            raw[start : start + 8] = f"{value:08x}".encode()
            with self.subTest(field=field, value=value):
                self.reject(compress(raw))

    def test_malformed_and_oversized_archive(self):
        for raw in (
            self.raw[:-4],
            b"070702" + self.raw[6:],
            b"070701g" + self.raw[7:],
            self.raw[:113] + b"X" + self.raw[114:],
        ):
            self.reject(compress(raw))
        self.reject(compress(b"x" * (MAX_ARCHIVE + 1)))
        self.reject(self.good + b"x" * MAX_ARCHIVE)

    def test_trailer_metadata_and_missing_entries(self):
        self.reject(compress(self.raw[116:]))
        self.reject(compress(self.raw.replace(b"TRAILER!!!", b"TRAILER!!?")))
        self.reject(compress(self.raw[:116]))

    def test_elf_machine_type_entry_interpreter_and_needed(self):
        for offset, format_string, value in (
            (18, "<H", 183),
            (16, "<H", 3),
            (24, "<Q", 0),
            (24, "<Q", 0x900000),
            (64, "<I", 3),
        ):
            binary = elf()
            struct.pack_into(format_string, binary, offset, value)
            self.reject(archive({"dev": b"", "init": bytes(binary)}))
        binary = elf()
        struct.pack_into("<H", binary, 56, 2)
        struct.pack_into("<Q", binary, 40, 176)
        struct.pack_into("<IIQQQQQQ", binary, 120, 2, 4, 240, 0, 0, 16, 16, 8)
        struct.pack_into("<qQ", binary, 240, 1, 0)
        self.reject(archive({"dev": b"", "init": bytes(binary)}))

    def test_elf_malformed_bounds_and_executable_memory(self):
        for offset, format_string, value in (
            (54, "<H", 55),
            (56, "<H", 0),
            (32, "<Q", 300),
            (40, "<Q", 300),
            (68, "<I", 7),
            (104, "<Q", 8),
        ):
            binary = elf()
            struct.pack_into(format_string, binary, offset, value)
            self.reject(archive({"dev": b"", "init": bytes(binary)}))

    def test_archive_repacking_order_timestamp_umask_path_independence(self):
        with tempfile.TemporaryDirectory() as temporary:
            for mask, timestamp, reverse in (
                (0o022, 1, False),
                (0o077, 900000000, True),
            ):
                directory = Path(temporary) / str(mask)
                directory.mkdir()
                previous = os.umask(mask)
                try:
                    path = directory / "different-name"
                    path.write_bytes(elf())
                    os.utime(path, (timestamp, timestamp))
                    entries = [("dev", b""), ("init", path.read_bytes())]
                    if reverse:
                        entries.reverse()
                    with patch.dict(os.environ, {"SOURCE_DATE_EPOCH": str(timestamp)}):
                        self.assertEqual(archive(dict(entries)), self.good)
                finally:
                    os.umask(previous)

    def test_elf_load_virtual_address_ranges_and_uint64_overflow(self):
        # Fixtures stay in memory; neither an ELF loader nor its entry point runs.
        for virtual, memory_bytes in (
            (0x400000, 0xFFFFFFFFFFFFFFFF),
            (0xFFFFFFFFFFFFF000, 8192),
            (0x00007FFFFFFFF000, 256),
            (0x00007FFFFFFFE000, 4097),
            (0x0000800000000000, 256),
            (0xFFFF800000000000, 256),
            (0x0100000000000000, 256),
            (0, 256),
            (0xF000, 256),
        ):
            binary = elf()
            struct.pack_into("<Q", binary, 80, virtual)
            struct.pack_into("<Q", binary, 104, memory_bytes)
            with self.subTest(virtual=hex(virtual), memory_bytes=memory_bytes):
                with self.assertRaisesRegex(
                    ValueError, "load segment virtual address range"
                ):
                    inspect_elf(binary)
                self.reject(archive({"dev": b"", "init": bytes(binary)}))
        # The last complete page below Linux's four-level guard page is valid.
        binary = elf()
        struct.pack_into("<Q", binary, 24, 0x00007FFFFFFFE078)
        struct.pack_into("<Q", binary, 80, 0x00007FFFFFFFE000)
        struct.pack_into("<Q", binary, 104, 4096)
        inspect_archive(archive({"dev": b"", "init": bytes(binary)}))

    def test_elf_entry_must_use_supported_user_address_range(self):
        for entry in (
            0xFFFF,
            0x00007FFFFFFFF000,
            0x0000800000000000,
            0xFFFFFFFFFFFFFFFF,
        ):
            binary = elf()
            struct.pack_into("<Q", binary, 24, entry)
            with self.subTest(entry=hex(entry)):
                with self.assertRaisesRegex(ValueError, "ELF header or architecture"):
                    inspect_elf(binary)
                self.reject(archive({"dev": b"", "init": bytes(binary)}))
        binary = elf()
        struct.pack_into("<Q", binary, 24, 0x10078)
        struct.pack_into("<Q", binary, 80, 0x10000)
        inspect_archive(archive({"dev": b"", "init": bytes(binary)}))

    def test_writer_refuses_additional_paths(self):
        with self.assertRaises(ValueError):
            archive({"dev": b"", "init": bytes(elf()), "extra": b""})

    @unittest.skipUnless(
        os.environ.get("PROBE_TEST_BINARY_A") and os.environ.get("PROBE_TEST_BINARY_B"),
        "independent Nix-built binaries required",
    )
    def test_independent_probe_compiles_and_archive_rebuilds(self):
        # Nix compiled these with different paths, creation order, timestamps and umasks.
        binaries = [
            Path(os.environ[name]).read_bytes()
            for name in ("PROBE_TEST_BINARY_A", "PROBE_TEST_BINARY_B")
        ]
        archives = []
        for data in binaries:
            inspect_elf(data)
            packed = archive({"init": data, "dev": b""})
            inspect_archive(packed)
            archives.append(packed)
        self.assertEqual(binaries[0], binaries[1])
        self.assertEqual(archives[0], archives[1])


if __name__ == "__main__":
    unittest.main()
