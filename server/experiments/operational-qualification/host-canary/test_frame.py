import copy
import json
import unittest

from frame import FIXED, FRAME_BYTES, PREFIX, InvalidFrame, decode_frame


def fixture():
    report = copy.deepcopy(FIXED)
    report["observed"] = {
        "status": {
            "state": "observed",
            "uid": [0] * 4,
            "gid": [0] * 4,
            "caps": [0] * 5,
            "no_new_privileges": 1,
            "supplementary_groups": 0,
        },
        "uid_map": {"state": "observed", "inside": 0, "outside": 0, "length": 1},
        "gid_map": {"state": "observed", "inside": 0, "outside": 0, "length": 1},
        "namespaces": {"state": "observed", "inodes": list(range(100, 107))},
        "mounts": {"state": "observed", "count": 4},
        "descriptors": {
            "state": "observed",
            "count": 3,
            "stdin_device_major": 1,
            "stdin_device_minor": 3,
            "stdout_inode": 1000,
            "stderr_inode": 1001,
        },
        "cgroup_namespace_membership": {"state": "observed"},
        "limit_fsize": {"state": "observed", "soft": 65536, "hard": 65536},
    }
    return report


def frame_bytes(payload):
    if type(payload) is dict:
        payload = json.dumps(payload, separators=(",", ":")).encode("ascii")
    return PREFIX + str(len(payload)).encode("ascii") + b"\n" + payload + b"\n"


class FrameTests(unittest.TestCase):
    def invalid(self, data):
        with self.assertRaisesRegex(InvalidFrame, "^invalid probe frame$"):
            decode_frame(data)

    def test_observations_cannot_become_host_or_guest_isolation_pass(self):
        report = decode_frame(frame_bytes(fixture()))
        self.assertEqual(report["host_confinement"], "unverified")
        self.assertEqual(report["qemu_guest_isolation"], "unverified")
        self.assertEqual(report["controller"], "unavailable")
        for key in ("host_confinement", "qemu_guest_isolation", "controller"):
            value = fixture()
            value[key] = "passed"
            self.invalid(frame_bytes(value))

    def test_failed_and_unavailable_are_retained_without_success_defaults(self):
        for state in ("failed", "unavailable"):
            value = fixture()
            value["observed"]["status"]["state"] = state
            value["observed"]["status"]["caps"][0] = 1
            self.assertEqual(decode_frame(frame_bytes(value)), value)

    def test_group_count_is_bounded_and_does_not_authenticate_host_groups(self):
        for count in (0, 11, 64):
            value = fixture()
            value["observed"]["status"]["supplementary_groups"] = count
            report = decode_frame(frame_bytes(value))
            self.assertEqual(
                report["observed"]["status"]["supplementary_groups"], count
            )
            self.assertIn("external_group_identity", report["unavailable"])
            self.assertEqual(report["host_confinement"], "unverified")
        for bad in (65, -1, True, "11"):
            value = fixture()
            value["observed"]["status"]["supplementary_groups"] = bad
            self.invalid(frame_bytes(value))

    def test_unknown_missing_and_positional_objects_at_every_boundary(self):
        paths = [(), ("requested",), ("observed",)]
        paths += [("observed", key) for key in fixture()["observed"]]
        for path in paths:
            for mutation in ("extra", "missing", "array"):
                value = fixture()
                obj = value
                parent = None
                for key in path:
                    parent, obj = obj, obj[key]
                if mutation == "extra":
                    obj["command"] = "secret-marker"
                elif mutation == "missing":
                    del obj[next(iter(obj))]
                elif parent is None:
                    value = list(obj.values())
                else:
                    parent[path[-1]] = list(obj.values())
                self.invalid(frame_bytes(json.dumps(value).encode("ascii")))

    def test_duplicate_keys_are_rejected_before_values_can_be_overwritten(self):
        data = json.dumps(fixture(), separators=(",", ":")).encode("ascii")
        for needle, replacement in (
            (b'"schema_version":1', b'"schema_version":2,"schema_version":1'),
            (
                b'"controller":"unavailable"',
                b'"controller":"passed","controller":"unavailable"',
            ),
            (b'"state":"observed"', b'"state":"failed","state":"observed"'),
            (b'"inside":0', b'"inside":1000,"inside":0'),
        ):
            self.invalid(frame_bytes(data.replace(needle, replacement, 1)))

    def test_truncation_trailing_frames_wrong_lengths_and_oversize(self):
        data = frame_bytes(fixture())
        for end in range(len(data)):
            self.invalid(data[:end])
        for invalid in (
            data + data,
            data + b"x",
            data.replace(PREFIX, b"WADDLE_HOST_PROBE_V2 ", 1),
            data.replace(PREFIX, PREFIX + b"0", 1),
            b"x" * (FRAME_BYTES + 1),
            PREFIX + b"18446744073709551616\n{}\n",
            frame_bytes(b"{"),
            frame_bytes(b"\xff"),
            frame_bytes(b"[]"),
            frame_bytes(b"[" * 2000 + b"]" * 2000),
        ):
            self.invalid(invalid)

    def test_numeric_types_ranges_and_consistency_are_strict(self):
        mutations = [
            ("status", "no_new_privileges", 0),
            ("status", "supplementary_groups", 65),
            ("status", "uid", [0, 0, 1000, 0]),
            ("status", "caps", [0, 0, 0, 1, 0]),
            ("uid_map", "outside", 1000),
            ("gid_map", "length", 2),
            ("namespaces", "inodes", [0] * 7),
            ("namespaces", "inodes", [1] * 6),
            ("mounts", "count", 5),
            ("descriptors", "stdout_inode", 1001),
            ("descriptors", "stdin_device_minor", 5),
            ("limit_fsize", "soft", 65537),
            ("limit_fsize", "hard", 2**64 - 1),
        ]
        mutations += [("mounts", "count", v) for v in (True, "4", 4.0, -1, 2**64, None)]
        for group, key, bad in mutations:
            value = fixture()
            value["observed"][group][key] = bad
            self.invalid(frame_bytes(value))
        for bad in (1, "true", None):
            value = fixture()
            value["requested"]["no_new_privileges"] = bad
            self.invalid(frame_bytes(value))

    def test_unavailable_inventory_cannot_be_deleted_reordered_or_replaced(self):
        for bad in ([], ["none"], list(reversed(fixture()["unavailable"]))):
            value = fixture()
            value["unavailable"] = bad
            self.invalid(frame_bytes(value))


if __name__ == "__main__":
    unittest.main()
