"""Pure deterministic tests: no processes, sockets, filesystems or namespaces."""

import copy
import unittest

import policy
from observe import verify_environment, verify_fsize, verify_status

TOKEN = "a" * 32
ID = {"device": 7, "inode": 8, "mode": 0o100600, "uid": 1000}


class PolicyTests(unittest.TestCase):
    def rejected(self, function, *args):
        with self.assertRaises(policy.Rejected):
            function(*args)

    def test_tokens_reject_argv_and_path_injection(self):
        for value in (
            "",
            "a" * 31,
            "a" * 33,
            "A" * 32,
            "../" + "a" * 29,
            "--share-net",
            "\n" + "a" * 31,
            None,
            1,
        ):
            self.rejected(policy.token, value)
        self.assertEqual(policy.token(TOKEN), TOKEN)

    def test_unit_collision_has_unique_fixed_name(self):
        self.assertEqual(policy.unit(TOKEN), "waddle-host-" + TOKEN + ".service")
        self.rejected(policy.run_name, 0, TOKEN)

    def test_cleanup_replacement_and_unexpected_inventory(self):
        original = {
            name: dict(ID, inode=index + 1)
            for index, name in enumerate(sorted(policy.INVENTORY))
        }
        current = copy.deepcopy(original)
        self.assertEqual(
            set(policy.cleanup_inventory(original, current)), policy.TRANSIENT
        )
        for name in original:
            current = copy.deepcopy(original)
            current[name]["inode"] += 999
            self.rejected(policy.cleanup_inventory, original, current)
        current = copy.deepcopy(original)
        current["unrelated"] = ID
        self.rejected(policy.cleanup_inventory, original, current)

    def test_every_interrupted_cleanup_prefix_and_repeat(self):
        original = {name: ID for name in policy.INVENTORY}
        order = policy.cleanup_inventory(original, original)
        for count in range(len(order) + 1):
            current = {
                name: value
                for name, value in original.items()
                if name not in order[:count]
            }
            self.assertEqual(policy.cleanup_inventory(original, current), order[count:])
        retained = {name: ID for name in policy.RETAINED}
        self.assertEqual(policy.cleanup_inventory(original, retained), [])
        self.assertEqual(policy.cleanup_inventory(original, retained), [])

    def test_missing_receipt_never_passes(self):
        original = {name: ID for name in policy.INVENTORY}
        for missing in policy.RETAINED:
            self.rejected(
                policy.cleanup_inventory,
                original,
                {name: value for name, value in original.items() if name != missing},
            )

    def test_stale_replaced_service_and_process(self):
        process = {"pid": 17, "start": 100, "boot": "fixture"}
        saved = {
            "InvocationID": TOKEN,
            "ControlGroup": "/user.slice/owned.service",
            "MainPID": "17",
            "process": process,
        }
        policy.same_service(saved, saved, process)
        for key in ("InvocationID", "ControlGroup", "MainPID"):
            replaced = dict(saved, **{key: "different"})
            self.rejected(policy.same_service, saved, replaced, process)
        self.rejected(policy.same_service, saved, saved, dict(process, start=101))
        policy.same_service(saved, dict(saved, MainPID="0"))

    def test_result_file_cannot_override_failed_service_exit(self):
        success = {
            "Result": "success",
            "ExecMainCode": "1",
            "ExecMainStatus": "0",
            "MainPID": "0",
            "ActiveState": "active",
            "SubState": "exited",
        }
        policy.service_success(success)
        for key, values in {
            "Result": ("timeout", "oom-kill", "signal", "exit-code", ""),
            "ExecMainCode": ("0", "2", "3", ""),
            "ExecMainStatus": ("1", "9", "15", ""),
            "MainPID": ("42", ""),
            "ActiveState": ("failed", "inactive", "deactivating"),
            "SubState": ("running", "failed", "dead"),
        }.items():
            for value in values:
                self.rejected(policy.service_success, dict(success, **{key: value}))
            missing = dict(success)
            del missing[key]
            self.rejected(policy.service_success, missing)

    def test_every_partial_creation_prefix_requires_exact_known_identity(self):
        names = [
            "state.json",
            "launch.json",
            "service.json",
            "result.json",
            "controller.json",
            "host-file",
            "host.sock",
        ]
        for length in range(len(names) + 1):
            known = {
                name: dict(ID, inode=index + 1)
                for index, name in enumerate(names[:length])
            }
            self.assertEqual(
                set(policy.cleanup_inventory(known, known, True)),
                set(known) & policy.TRANSIENT,
            )
            for name, item in known.items():
                replaced = dict(known, **{name: dict(item, inode=999)})
                self.rejected(policy.cleanup_inventory, known, replaced, True)
            if length < len(names):
                extra = dict(known, **{names[length]: ID})
                self.rejected(policy.cleanup_inventory, known, extra, True)

    def test_every_namespace_must_be_observed_and_different(self):
        baseline = dict.fromkeys(policy.NAMESPACES, 11)
        observed = dict.fromkeys(policy.NAMESPACES, 12)
        policy.namespaces(baseline, observed)
        for name in policy.NAMESPACES:
            for value in (11, 0, True, "12"):
                self.rejected(
                    policy.namespaces, baseline, dict(observed, **{name: value})
                )
            missing = dict(observed)
            del missing[name]
            self.rejected(policy.namespaces, baseline, missing)

    def test_requested_limits_are_not_effective_observations(self):
        policy.limits(policy.LIMITS)
        self.rejected(policy.limits, policy.PROPERTIES)
        for key in policy.LIMITS:
            for value in ("max\n", "0\n", "", "50000 200000\n"):
                if value != policy.LIMITS[key]:
                    self.rejected(policy.limits, dict(policy.LIMITS, **{key: value}))

    def test_cgroup_paths_cannot_escape(self):
        policy.cgroup_path(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/owned.service"
        )
        for path in ("/", "relative", "/a/../b", "/a//b", "/a\n", "/a/./b"):
            self.rejected(policy.cgroup_path, path)

    def test_external_id_map_must_bind_service_owner(self):
        policy.id_map("0 1000 1\n", 1000)
        for value in ("0 0 1\n", "0 1001 1\n", "0 1000 2\n", "0 1000 1\n1 1001 1\n"):
            self.rejected(policy.id_map, value, 1000)

    def test_framing_truncation_duplicate_and_oversize(self):
        data = policy.encode({"observed": [1, 2, 3]})
        for length in range(len(data)):
            self.rejected(policy.decode, data[:length])
        self.rejected(policy.decode, b'{"a":1,"a":2}')
        self.rejected(policy.decode, b"x" * (policy.MAX_FILE + 1))

    def test_fixed_launch_has_no_fallback_and_bounds_journal(self):
        paths = {
            "bwrap": "/pinned/bwrap",
            "probe": "/pinned/probe",
            "bootstrap": "/pinned/bootstrap",
            "systemd-run": "/pinned/systemd-run",
        }
        args = policy.sandbox_argv(paths, TOKEN, 1000, 8, 9, 10)
        for namespace in ("user", "pid", "net", "ipc", "uts", "cgroup"):
            self.assertIn("--unshare-" + namespace, args)
        self.assertFalse(
            any(
                "try" in item or item in {"--share-net", "--unshare-all", "--dev"}
                for item in args
            )
        )
        self.assertEqual(args[-4:], ["/proc/self/fd/8", TOKEN, "1000", "9"])
        service = policy.service_argv(paths, TOKEN)
        for setting in (
            "StandardOutput=null",
            "StandardError=null",
            "RuntimeMaxSec=30s",
            "LimitFSIZE=65536",
            "KillMode=control-group",
            "Restart=no",
        ):
            self.assertIn("--property=" + setting, service)

    def test_kernel_status_capability_identity_and_fsize_checks(self):
        status = "Uid:\t1000 1000 1000 1000\nGid:\t1000 1000 1000 1000\nGroups:\t\nNoNewPrivs:\t1\n"
        status += "".join(
            name + ":\t0000000000000000\n"
            for name in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb")
        )
        verify_status(status, 1000, 1000, [])
        self.rejected(
            verify_status, status.replace("Groups:\t", "Groups:\t1000"), 1000, 1000, []
        )
        self.rejected(
            verify_status,
            status.replace("CapEff:\t0000000000000000", "CapEff:\t0000000000000001"),
            1000,
            1000,
            [],
        )
        verify_fsize(
            "Max file size             65536                65536                bytes\n"
        )
        self.rejected(
            verify_fsize,
            "Max file size             unlimited             65536                bytes\n",
        )

    def test_exact_initial_environment_for_each_image(self):
        verify_environment("PWD=/\0", "witness")
        verify_environment("", "probe")
        for data in ("", "PWD=/", "PWD=/tmp\0", "PWD=/\0PATH=/bin\0", "PWD=/\0PWD=/\0"):
            self.rejected(verify_environment, data, "witness")
        for data in ("PWD=/\0", "PATH=/bin\0", "\0"):
            self.rejected(verify_environment, data, "probe")
        self.rejected(verify_environment, "", "other")

    def test_exact_mount_inventory_flags_and_sources(self):
        data = (
            "10 1 0:1 /newroot / ro,nosuid,nodev,noexec - tmpfs tmpfs ro\n"
            "11 10 0:2 / /proc ro,nosuid,nodev,noexec - proc proc ro\n"
            "12 10 0:3 /null /dev/null rw,nosuid,noexec - devtmpfs devtmpfs rw\n"
            "13 10 8:1 /pinned/probe /probe ro,nosuid,nodev - ext4 source ro\n"
        )
        policy.mounts(data)
        for mutation in (
            data.replace("ro,nosuid,nodev,noexec", "rw,nosuid,nodev,noexec", 1),
            data + "14 10 8:1 /secrets /hidden ro - ext4 source ro\n",
            data.replace("11 10", "11 99"),
            data.replace(" /newroot / ro", " / / ro"),
            data.replace("noexec - tmpfs", "noexec shared:1 - tmpfs"),
        ):
            self.rejected(policy.mounts, mutation)


if __name__ == "__main__":
    unittest.main()
