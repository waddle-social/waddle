"""Pure integration fakes; no real OS reader/launcher/socket is invoked."""

import unittest
from contextlib import ExitStack
from types import SimpleNamespace
from unittest.mock import Mock, patch

import controller
import policy
import runtime
from observe import groups, verify_status

TOKEN = "a" * 32
IDENTITY = {"device": 1, "inode": 2, "mode": 0o100600, "uid": 1000}
RECORD = {
    "InvocationID": TOKEN,
    "MainPID": "42",
    "ControlGroup": "/owned.service",
    "process": {"pid": 42, "start": 100, "boot": "fixture"},
    "cgroup_identity": IDENTITY,
}


class MemoryRoot:
    def __init__(self, *_args, **_kwargs):
        self.path = "/fixture/run"
        self.identity = IDENTITY
        self.parents = dict.fromkeys(
            ("/", "/run", "/run/user", "/run/user/1000"), IDENTITY
        )
        self.files = {}
        self.cleaned = 0
        self.owned = dict.fromkeys(policy.INVENTORY, IDENTITY)

    def create_file(self, name, data=b""):
        if name in self.files:
            raise FileExistsError
        self.files[name] = data

    def inventory(self):
        return dict.fromkeys(policy.INVENTORY, IDENTITY)

    def read(self, name):
        if name == "service.json":
            return policy.encode(RECORD)
        if name == "result.json":
            return policy.encode(
                {"state": "observed", "nonce": TOKEN, "invocation": TOKEN}
            )
        return self.files[name]

    def write(self, name, value, _identity):
        self.files[name] = policy.encode(value)

    def cleanup(self, inventory, partial=False):
        policy.cleanup_inventory(
            inventory,
            inventory
            if self.cleaned == 0
            else {name: IDENTITY for name in policy.RETAINED},
            partial,
        )
        self.cleaned += 1

    def close(self):
        pass


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        override = patch.object(controller, "manager_transport_required")
        override.start()
        self.addCleanup(override.stop)
        for target, name, value in (
            (controller.os, "getuid", 1000),
            (controller.os, "getgid", 1000),
            (controller.secrets, "token_hex", TOKEN),
        ):
            override = patch.object(target, name, return_value=value)
            override.start()
            self.addCleanup(override.stop)

    def test_run_order_requires_positive_before_and_after_and_repeat_cleanup(self):
        root = MemoryRoot()
        sentinel = Mock()
        events = []
        sentinel.positive.side_effect = lambda: events.append("positive")
        sentinel.negative_queue.side_effect = lambda: events.append("negative")
        with ExitStack() as stack:
            stack.enter_context(patch.object(controller, "RunRoot", return_value=root))
            stack.enter_context(
                patch.object(controller, "Sentinels", return_value=sentinel)
            )
            stack.enter_context(
                patch.object(controller.secrets, "token_hex", return_value=TOKEN)
            )
            stack.enter_context(
                patch.object(controller.os, "getuid", return_value=1000)
            )
            stack.enter_context(
                patch.object(controller.os, "getgid", return_value=1000)
            )
            stack.enter_context(
                patch.object(
                    controller,
                    "show",
                    side_effect=[
                        {"LoadState": "not-found"},
                        dict(
                            RECORD,
                            ActiveState="active",
                            SubState="exited",
                            MainPID="0",
                            Result="success",
                            ExecMainCode="1",
                            ExecMainStatus="0",
                        ),
                    ],
                )
            )
            launch = stack.enter_context(
                patch.object(
                    controller,
                    "command",
                    side_effect=lambda _a: events.append("launch"),
                )
            )
            stack.enter_context(
                patch.object(
                    controller,
                    "stop_owned",
                    side_effect=lambda *_a: events.append("stop"),
                )
            )
            result = controller.run(
                {"systemd-run": "/fixed/systemd-run", "bootstrap": "/fixed/bootstrap"}
            )
        self.assertEqual(events[:4], ["positive", "launch", "negative", "positive"])
        self.assertEqual(result["state"], "observed")
        self.assertEqual(root.cleaned, 3)
        launch.assert_called_once()

    def test_observed_result_cannot_hide_service_failure(self):
        root = MemoryRoot()
        actual = dict(
            RECORD,
            MainPID="0",
            ActiveState="failed",
            SubState="failed",
            Result="timeout",
            ExecMainCode="2",
            ExecMainStatus="9",
        )
        with (
            patch.object(controller, "RunRoot", return_value=root),
            patch.object(controller, "Sentinels", return_value=Mock()) as sentinels,
            patch.object(
                controller, "show", side_effect=[{"LoadState": "not-found"}, actual]
            ),
            patch.object(controller, "command"),
            patch.object(controller, "stop_owned"),
            self.assertRaisesRegex(policy.Rejected, "unsuccessful-service-exit"),
        ):
            controller.run(
                {"systemd-run": "/fixed/run", "bootstrap": "/fixed/bootstrap"}
            )
        sentinels.return_value.negative_queue.assert_not_called()
        self.assertEqual(root.cleaned, 1)

    def test_result_failure_before_or_after_stop_rejects_success(self):
        good = dict(
            RECORD,
            LoadState="loaded",
            MainPID="0",
            ActiveState="active",
            SubState="exited",
            Result="success",
            ExecMainCode="1",
            ExecMainStatus="0",
        )
        group = Mock(identity=IDENTITY)
        group.members.return_value = set()
        for before_stop in (True, False):
            bad = dict(good, Result="timeout", ActiveState="failed")
            observations = [good, bad] if before_stop else [good, good, bad]
            with (
                patch.object(controller, "show", side_effect=observations),
                patch.object(controller, "Cgroup", return_value=group),
                patch.object(controller, "command") as stop,
                self.assertRaisesRegex(policy.Rejected, "unsuccessful-service-exit"),
            ):
                controller.stop_owned(
                    {"systemctl": "/fixed/systemctl"}, TOKEN, RECORD, True
                )
            self.assertEqual(stop.call_count, 0 if before_stop else 1)

    def test_run_root_collision_never_starts_service(self):
        with (
            patch.object(controller, "RunRoot", side_effect=FileExistsError),
            patch.object(controller, "command") as command,
            self.assertRaises(FileExistsError),
        ):
            controller.run({})
        command.assert_not_called()

    def test_unit_collision_never_starts_or_stops_existing_service(self):
        with (
            patch.object(controller, "RunRoot", return_value=MemoryRoot()),
            patch.object(controller, "Sentinels"),
            patch.object(controller, "show", return_value={"LoadState": "loaded"}),
            patch.object(controller, "command") as command,
            patch.object(controller, "stop_owned") as stop,
            self.assertRaisesRegex(policy.Rejected, "unit-collision"),
        ):
            controller.run({})
        command.assert_not_called()
        stop.assert_not_called()

    def test_replaced_service_or_stale_pid_never_signalled(self):
        for actual, process in (
            (
                dict(RECORD, LoadState="loaded", InvocationID="b" * 32),
                RECORD["process"],
            ),
            (dict(RECORD, LoadState="loaded"), dict(RECORD["process"], start=101)),
        ):
            with (
                patch.object(controller, "show", return_value=actual),
                patch.object(controller, "process_identity", return_value=process),
                patch.object(controller, "command") as command,
                self.assertRaises(policy.Rejected),
            ):
                controller.stop_owned({}, TOKEN, RECORD)
            command.assert_not_called()

    def test_garbage_collected_unit_requires_absent_cgroup_no_signal(self):
        with (
            patch.object(controller, "show", return_value={"LoadState": "not-found"}),
            patch.object(controller, "Cgroup", side_effect=FileNotFoundError),
            patch.object(controller, "command") as command,
        ):
            controller.stop_owned({}, TOKEN, RECORD)
            controller.stop_owned({}, TOKEN, RECORD)
        command.assert_not_called()
        with (
            patch.object(controller, "show", return_value={"LoadState": "not-found"}),
            patch.object(controller, "Cgroup", return_value=Mock()),
            patch.object(controller, "command") as command,
            self.assertRaisesRegex(policy.Rejected, "orphaned-cgroup"),
        ):
            controller.stop_owned({}, TOKEN, RECORD)
        command.assert_not_called()

    def test_replaced_cgroup_prevents_stop(self):
        group = Mock(identity=dict(IDENTITY, inode=999))
        actual = dict(RECORD, LoadState="loaded", ActiveState="active")
        with (
            patch.object(controller, "show", return_value=actual),
            patch.object(
                controller, "process_identity", return_value=RECORD["process"]
            ),
            patch.object(controller, "Cgroup", return_value=group),
            patch.object(controller, "command") as command,
            self.assertRaisesRegex(policy.Rejected, "replaced-cgroup"),
        ):
            controller.stop_owned({}, TOKEN, RECORD)
        command.assert_not_called()

    def test_actual_cleanup_anchors_and_rejects_replacement_before_unlink(self):
        root = object.__new__(runtime.RunRoot)
        root.fd = 10
        root.check = Mock()
        original = dict.fromkeys(policy.INVENTORY, IDENTITY)
        root.inventory = Mock(return_value=original)
        root.parent_for = Mock(return_value=(11, "host-file"))
        replacement = SimpleNamespace(
            st_dev=1, st_ino=999, st_mode=0o100600, st_uid=1000
        )
        with (
            patch.object(runtime.os, "stat", return_value=replacement),
            patch.object(runtime.os, "close"),
            patch.object(runtime.os, "unlink") as unlink,
            self.assertRaisesRegex(policy.Rejected, "replaced-entry"),
        ):
            root.cleanup(original)
        unlink.assert_not_called()

    def test_host_sentinel_positive_wrong_nonce_or_peer_fails(self):
        sentinel = object.__new__(runtime.Sentinels)
        sentinel.root = Mock()
        sentinel.nonce = b"a" * 32
        sentinel.listeners = []
        sentinel.root.read.return_value = b"wrong"
        with self.assertRaisesRegex(policy.Rejected, "file-positive"):
            sentinel.positive()
        sentinel.root.read.return_value = sentinel.nonce
        sentinel.positive()
        listener = Mock()
        sentinel.listeners = [listener]
        client = Mock()
        client.getsockopt.return_value = b"\0" * 12
        fake_socket = Mock()
        fake_socket.__enter__ = Mock(return_value=client)
        fake_socket.__exit__ = Mock(return_value=False)
        with (
            patch.object(runtime.socket, "socket", return_value=fake_socket),
            patch.object(runtime.os, "getpid", return_value=123),
            self.assertRaisesRegex(policy.Rejected, "sentinel-peer"),
        ):
            sentinel.positive()

    def test_unexpected_host_sentinel_connection_fails(self):
        sentinel = object.__new__(runtime.Sentinels)
        sentinel.root = Mock()
        listener = Mock()
        listener.accept.return_value = (Mock(), None)
        sentinel.listeners = [listener]
        with self.assertRaisesRegex(policy.Rejected, "sentinel-contacted"):
            sentinel.negative_queue()
        listener.accept.side_effect = BlockingIOError
        sentinel.negative_queue()

    def test_group_vector_is_external_exact_including_order_and_count(self):
        base = "Uid: 1000 1000 1000 1000\nGid: 1000 1000 1000 1000\nGroups: 1 7 1000\nNoNewPrivs: 1\n"
        base += "".join(
            name + ": 0000000000000000\n"
            for name in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb")
        )
        verify_status(base, 1000, 1000, [1, 7, 1000])
        for expected in ([1, 7], [1, 7, 1000, 1001], [7, 1, 1000], [65534, 65534, 0]):
            with self.assertRaises(policy.Rejected):
                verify_status(base, 1000, 1000, expected)
        with self.assertRaises(policy.Rejected):
            groups("Groups: " + "1 " * 65)


class PartialRoot:
    """Owned-inode model; errors can occur immediately after each creation."""

    def __init__(self, fail_after=None):
        self.path = "/fixture/run"
        self.identity = IDENTITY
        self.parents = dict.fromkeys(
            ("/", "/run", "/run/user", "/run/user/1000"), IDENTITY
        )
        self.owned = {}
        self.current = {}
        self.files = {}
        self.fail_after = fail_after
        self.closed = False

    def create_file(self, name, data=b""):
        if name in self.files:
            raise FileExistsError
        item = dict(IDENTITY, inode=len(self.owned) + 10)
        self.owned[name] = item
        self.current[name] = item
        self.files[name] = data
        if len(self.owned) == self.fail_after:
            raise OSError("synthetic interrupted creation")

    def inventory(self):
        return dict(self.current)

    def read(self, name):
        return self.files[name]

    def write(self, name, data, _identity):
        self.files[name] = policy.encode(data)

    def cleanup(self, recorded, partial=False):
        for name in policy.cleanup_inventory(recorded, self.current, partial):
            del self.current[name]
            del self.files[name]

    def close(self):
        self.closed = True


class PartialCleanupTests(unittest.TestCase):
    def setUp(self):
        for target, name, value in (
            (controller.os, "getuid", 1000),
            (controller.os, "getgid", 1000),
            (controller.secrets, "token_hex", TOKEN),
            (controller, "manager_transport_required", None),
        ):
            override = patch.object(target, name, return_value=value)
            override.start()
            self.addCleanup(override.stop)

    def prepared(self):
        root = PartialRoot()
        for name in sorted(policy.INVENTORY):
            root.create_file(name)
        root.write(
            "state.json",
            {
                "version": 3,
                "nonce": TOKEN,
                "uid": 1000,
                "gid": 1000,
                "parents": root.parents,
                "root": root.identity,
                "inventory": root.inventory(),
            },
            None,
        )
        return root

    def sentinels(self, root, _nonce):
        root.create_file("host-file")
        root.create_file("host.sock")
        return Mock()

    def test_all_prelaunch_creation_prefixes_clean_owned_transients(self):
        for length in range(1, len(policy.INVENTORY) + 1):
            with self.subTest(length=length):
                root = PartialRoot(fail_after=length)
                with (
                    patch.object(controller, "RunRoot", return_value=root),
                    patch.object(controller, "Sentinels", side_effect=self.sentinels),
                    patch.object(controller, "command") as launch,
                    patch.object(controller, "stop_owned") as stop,
                    self.assertRaises(OSError),
                ):
                    controller.run({})
                self.assertFalse(set(root.current) & policy.TRANSIENT)
                self.assertTrue(root.closed)
                launch.assert_not_called()
                stop.assert_not_called()

    def test_initial_positive_failure_cleans_files_and_persisted_cleanup_repeats(self):
        root = PartialRoot()

        def failed_positive(root, nonce):
            sentinel = self.sentinels(root, nonce)
            sentinel.positive.side_effect = policy.Rejected("synthetic-positive")
            return sentinel

        with (
            patch.object(controller, "RunRoot", return_value=root),
            patch.object(controller, "Sentinels", side_effect=failed_positive),
            patch.object(controller, "show", return_value={"LoadState": "not-found"}),
            patch.object(controller, "command") as launch,
            patch.object(controller, "stop_owned") as stop,
        ):
            with self.assertRaisesRegex(policy.Rejected, "synthetic-positive"):
                controller.run({})
            self.assertEqual(controller.cleanup({}, TOKEN)["service"], "not_attempted")
            self.assertEqual(controller.cleanup({}, TOKEN)["service"], "not_attempted")
        self.assertFalse(set(root.current) & policy.TRANSIENT)
        launch.assert_not_called()
        stop.assert_not_called()

    def test_empty_or_torn_service_record_cleans_files_without_signalling(self):
        for record in (b"", b"{", b'{"InvocationID":'):
            root = self.prepared()
            root.files["launch.json"] = policy.encode(
                {"nonce": TOKEN, "state": "attempted"}
            )
            root.files["service.json"] = record
            with (
                patch.object(controller, "RunRoot", return_value=root),
                patch.object(controller, "stop_owned") as stop,
            ):
                for _ in range(2):
                    with self.assertRaises(policy.Rejected):
                        controller.cleanup({}, TOKEN)
                stop.assert_not_called()
            self.assertFalse(set(root.current) & policy.TRANSIENT)

    def test_unknown_launch_outcome_not_retried_and_files_cleaned(self):
        root = PartialRoot()
        with (
            patch.object(controller, "RunRoot", return_value=root),
            patch.object(controller, "Sentinels", side_effect=self.sentinels),
            patch.object(controller, "show", return_value={"LoadState": "not-found"}),
            patch.object(
                controller, "command", side_effect=policy.Rejected("unknown")
            ) as launch,
            patch.object(controller, "stop_owned") as stop,
            self.assertRaisesRegex(policy.Rejected, "unknown"),
        ):
            controller.run(
                {"systemd-run": "/fixed/run", "bootstrap": "/fixed/bootstrap"}
            )
        self.assertEqual(policy.decode(root.files["launch.json"])["state"], "attempted")
        self.assertFalse(set(root.current) & policy.TRANSIENT)
        launch.assert_called_once()
        stop.assert_not_called()

    def test_replaced_root_or_entry_or_unrecorded_entry_never_deleted(self):
        for mutation in ("root", "known", "unknown"):
            root = self.prepared()
            if mutation == "root":
                root.identity = dict(IDENTITY, inode=999)
            elif mutation == "known":
                root.current["host-file"] = dict(IDENTITY, inode=999)
            else:
                root.current["unknown"] = IDENTITY
            before = dict(root.current)
            with (
                patch.object(controller, "RunRoot", return_value=root),
                patch.object(controller, "stop_owned") as stop,
                self.assertRaises(policy.Rejected),
            ):
                controller.cleanup({}, TOKEN)
            self.assertEqual(root.current, before)
            stop.assert_not_called()

    def test_empty_or_torn_state_cannot_authenticate_names(self):
        for state in (b"", b"{"):
            root = self.prepared()
            root.files["state.json"] = state
            before = dict(root.current)
            with (
                patch.object(controller, "RunRoot", return_value=root),
                patch.object(controller, "stop_owned") as stop,
                self.assertRaises(policy.Rejected),
            ):
                controller.cleanup({}, TOKEN)
            self.assertEqual(root.current, before)
            stop.assert_not_called()


class ManagerBlockTests(unittest.TestCase):
    def test_manager_operations_fail_before_any_process_root_or_connection(self):
        for operation in (
            lambda: runtime.show({}, TOKEN),
            lambda: controller.run({}),
            lambda: controller.service({}, {}, None, TOKEN),
            lambda: controller.stop_owned({}, TOKEN, RECORD),
        ):
            with (
                patch.object(runtime, "command") as command,
                patch.object(controller, "RunRoot") as root,
                patch.object(runtime.socket, "socket") as connection,
                self.assertRaisesRegex(
                    policy.Rejected, "authenticated-manager-transport-unimplemented"
                ),
            ):
                operation()
            command.assert_not_called()
            root.assert_not_called()
            connection.assert_not_called()


class FakeSelector:
    def __init__(self):
        self.entries = {}

    def register(self, fd, _event, name):
        self.entries[fd] = SimpleNamespace(fd=fd, data=name)

    def unregister(self, fd):
        del self.entries[fd]

    def get_map(self):
        return self.entries

    def select(self, _timeout):
        return [(entry, 1) for entry in self.entries.values()]

    def close(self):
        pass


class StreamTests(unittest.TestCase):
    def stream(self, blocks, cap, ticks):
        process = Mock(pid=45)
        process.poll.return_value = None
        process.wait.return_value = 0
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(runtime.selectors, "DefaultSelector", FakeSelector)
            )
            stack.enter_context(patch.object(runtime.os, "set_blocking"))
            stack.enter_context(patch.object(runtime.os, "pidfd_open", return_value=90))
            stack.enter_context(patch.object(runtime.os, "close"))
            stack.enter_context(patch.object(runtime.os, "read", side_effect=blocks))
            stack.enter_context(
                patch.object(runtime.time, "monotonic", side_effect=ticks)
            )
            kill = stack.enter_context(
                patch.object(runtime.signal, "pidfd_send_signal")
            )
            try:
                return runtime.pump(process, {"out": (10, cap)}, 5), kill
            except policy.Rejected:
                kill.assert_called_once_with(90, signal_kill())
                raise

    def test_exact_cap_then_eof(self):
        output, _ = self.stream([b"abc", b""], 3, [0, 1, 2, 3, 4])
        self.assertEqual(output, {"out": b"abc"})

    def test_one_excess_byte_kills_owned_pidfd(self):
        with self.assertRaisesRegex(policy.Rejected, "pipe-output-limit"):
            self.stream([b"abcd"], 3, [0, 1])

    def test_deadline_without_output_kills_owned_pidfd(self):
        with self.assertRaisesRegex(policy.Rejected, "pipe-deadline"):
            self.stream([], 3, [0, 6])


class BarrierTests(unittest.TestCase):
    def pipeline(self, mutation=None, observer_error=False, replaced=False):
        process = Mock(pid=50, returncode=2)
        process.poll.return_value = 2
        process.stdout.fileno.return_value = 10
        process.stderr.fileno.return_value = 11
        group = Mock()
        group.members.side_effect = [{100, 50, 60}, {100}]
        group.limits.return_value = policy.LIMITS
        observed = {"process": {"pid": 60, "start": 42}, "namespaces": {"user": 7}}
        observations = [observed, observed, observed]
        if replaced:
            observations[1] = dict(observed, process={"pid": 60, "start": 43})
        writes = []
        closed = []

        def fake_pump(_process, _caps, _timeout, hook):
            stages = [b"READY\n", controller.WITNESS_RESULT, controller.PROBE_READY]
            for index, stage in enumerate(stages):
                data = {
                    "out": b"",
                    "err": stage,
                    "info": policy.encode({"child-pid": 60}),
                }
                if mutation:
                    mutation(index, data)
                hook(data)
            return {
                "out": b"synthetic-frame",
                "err": controller.PROBE_READY,
                "info": b"{}",
            }

        with ExitStack() as stack:
            stack.enter_context(patch.object(controller.os, "open", return_value=20))
            stack.enter_context(
                patch.object(controller.os, "pipe2", side_effect=[(21, 22), (23, 24)])
            )
            stack.enter_context(
                patch.object(
                    controller.os,
                    "fstat",
                    side_effect=lambda fd: SimpleNamespace(st_ino=fd + 100),
                )
            )
            stack.enter_context(
                patch.object(controller.os, "close", side_effect=closed.append)
            )
            stack.enter_context(
                patch.object(
                    controller.os,
                    "write",
                    side_effect=lambda fd, data: writes.append((fd, data)) or len(data),
                )
            )
            stack.enter_context(
                patch.object(controller.os, "pidfd_open", return_value=30)
            )
            stack.enter_context(patch.object(controller.os, "getpid", return_value=100))
            stack.enter_context(
                patch.object(controller.os, "getuid", return_value=1000)
            )
            stack.enter_context(
                patch.object(controller.subprocess, "Popen", return_value=process)
            )
            stack.enter_context(patch.object(controller, "pump", side_effect=fake_pump))
            observer = stack.enter_context(
                patch.object(
                    controller,
                    "sandbox",
                    side_effect=PermissionError if observer_error else observations,
                )
            )
            stack.enter_context(patch.object(controller, "match_probe"))
            try:
                result = controller.launch_sandbox(
                    {
                        "witness": "/fixed/witness",
                        "bwrap": "/fixed/bwrap",
                        "probe": "/fixed/probe",
                    },
                    {},
                    TOKEN,
                    {},
                    group,
                    lambda _: {},
                )
            except (policy.Rejected, PermissionError):
                self.assertNotIn((22, b"I"), writes)
                raise
        self.assertEqual(writes, [(22, b"G"), (22, b"P"), (22, b"I")])
        self.assertEqual(closed.count(22), 1)
        self.assertEqual(observer.call_args_list[2].args[-1], "probe")
        self.assertEqual(observer.call_count, 3)
        return result

    def test_three_barriers_and_post_exec_identity_before_i_and_eof(self):
        result = self.pipeline()
        self.assertIn("trusted_post_exec_observation", result)

    def test_unavailable_kernel_observer_never_releases(self):
        with self.assertRaises(PermissionError):
            self.pipeline(observer_error=True)

    def test_process_replacement_between_barriers_rejects(self):
        with self.assertRaisesRegex(policy.Rejected, "barrier-process-change"):
            self.pipeline(replaced=True)

    def test_premature_probe_output_rejects(self):
        with self.assertRaisesRegex(policy.Rejected, "premature-probe-output"):
            self.pipeline(
                mutation=lambda index, data: (
                    data.update(out=b"forged") if index == 0 else None
                )
            )

    def test_wrong_sentinel_errno_never_releases_probe(self):
        with self.assertRaisesRegex(policy.Rejected, "invalid-witness-progress"):
            self.pipeline(
                mutation=lambda index, data: (
                    data.update(err=data["err"].replace(b"ENOENT", b"EACCES"))
                    if index == 1
                    else None
                )
            )


def signal_kill():
    return runtime.signal.SIGKILL


if __name__ == "__main__":
    unittest.main()
