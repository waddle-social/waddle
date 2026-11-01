"""Offline fake-child tests only: no guard, manager tool or service runs."""

import base64
import errno
import hashlib
import json
import os
import signal
import stat
import tempfile
import time
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

import resource_guard
import resource_reader as r


def success():
    unit = f"waddle-resource-{os.getuid()}-" + "a" * 32 + ".service"
    evidence = "events-populated-0"
    return {
        "ok": True,
        "check": "limits-observed",
        "unit": unit,
        "kernel_reclamation": "unverified",
        "userspace_quiesced": True,
        "quiescence_evidence": evidence,
        "observed": {
            "invocation": "1" * 32,
            "pid": 42,
            "start_ticks": 123,
            "cgroup": f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/app.slice/{unit}",
            "cgroup_device": 1,
            "cgroup_inode": 2,
            "limits": {
                "cpu.max": "50000 100000",
                "memory.max": "67108864",
                "memory.swap.max": "0",
                "pids.max": "8",
            },
            "runtime_seconds": 8,
            "stop_seconds": 1,
            "quiescence_evidence": evidence,
            "transient_fragment": "retained-after-stop",
        },
    }


def encode(value):
    return (json.dumps(value, separators=(",", ":")) + "\n").encode("ascii")


def write_report(value=None, status=0):
    os.write(1, encode(success() if value is None else value))
    os._exit(status)


class ReaderTests(unittest.TestCase):
    def capture(self, child=write_report, capture=None):
        with (
            patch.object(r, "child_exec", side_effect=lambda _fd: child()),
            patch.object(r, "CAPTURE_SECONDS", 0.16),
            patch.object(r, "TERM_SECONDS", 0.08),
            patch.object(r, "KILL_SECONDS", 0.12),
            patch.object(os, "kill", side_effect=AssertionError("numeric signal")),
            patch.object(os, "killpg", side_effect=AssertionError("group signal")),
            patch.object(
                resource_guard.Host,
                "command",
                side_effect=AssertionError("manager access"),
            ),
        ):
            return (capture or r.Capture()).run(-1)

    def assert_reaped(self, value):
        self.assertTrue(value["parent_reaped"], value)
        self.assertTrue(value["parent_pidfd_dead"], value)
        self.assertEqual(value["channels_eof"], ["stderr", "stdout"], value)
        self.assertEqual(value["kernel_reclamation"], "unverified")

    def test_both_activation_gates_and_artifact_pin_remain_unset(self):
        with (
            patch.object(r, "fixed_artifact") as artifact,
            patch.object(r, "spawn") as spawn,
            patch.object(r, "save_evidence") as evidence,
            patch.object(r.sys, "argv", ["resource_reader.py"]),
            self.assertRaisesRegex(r.Rejected, "reader-activation-review-required"),
        ):
            r.main()
        artifact.assert_not_called()
        spawn.assert_not_called()
        evidence.assert_not_called()
        with self.assertRaisesRegex(
            resource_guard.Rejected, "lifecycle-review-required"
        ):
            resource_guard.lifecycle_review_required()
        self.assertIsNone(r.ARTIFACT)
        self.assertIsNone(r.ARTIFACT_SHA256)
        with self.assertRaisesRegex(r.Rejected, "artifact-pin-required"):
            r.fixed_artifact()

    def test_success_fresh_pipes_pinned_before_release_and_exact_reaping(self):
        events = []
        pidfd_open = os.pidfd_open
        waitid = os.waitid

        def pin(pid, flags):
            events.append("pin")
            return pidfd_open(pid, flags)

        def reap(kind, fd, flags):
            self.assertEqual(kind, os.P_PIDFD)
            self.assertEqual(events[0], "pin")
            events.append("reap")
            return waitid(kind, fd, flags)

        def child():
            self.assertTrue(stat.S_ISFIFO(os.fstat(1).st_mode))
            self.assertTrue(stat.S_ISFIFO(os.fstat(2).st_mode))
            self.assertNotEqual(os.fstat(1).st_ino, os.fstat(2).st_ino)
            write_report()

        with (
            patch.object(os, "pidfd_open", side_effect=pin),
            patch.object(os, "waitid", side_effect=reap),
        ):
            value = self.capture(child)
        self.assertTrue(value["ok"], value)
        self.assert_reaped(value)
        self.assertEqual(events, ["pin", "reap"])
        self.assertEqual(value["signals"], [])
        self.assertFalse(value["possible_unfinished_manager_operation"])
        self.assertEqual(
            json.loads(base64.b64decode(value["stdout_base64"])), success()
        )

    def test_unrelated_child_is_never_signalled_or_reaped(self):
        unrelated = os.fork()
        if unrelated == 0:
            os._exit(23)
        try:
            value = self.capture()
            self.assertTrue(value["ok"], value)
            self.assert_reaped(value)
        finally:
            found, status = os.waitpid(unrelated, 0)
        self.assertEqual(found, unrelated)
        self.assertEqual(os.waitstatus_to_exitcode(status), 23)

    def test_pin_precedes_any_fake_child_progress(self):
        marker_r, marker_w = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
        original_pin = os.pidfd_open
        try:

            def pin(pid, flags):
                with self.assertRaises(BlockingIOError):
                    os.read(marker_r, 1)
                return original_pin(pid, flags)

            def child():
                os.write(marker_w, b"X")
                write_report()

            with patch.object(os, "pidfd_open", side_effect=pin):
                value = self.capture(child)
            self.assertTrue(value["ok"], value)
            self.assertEqual(os.read(marker_r, 1), b"X")
        finally:
            os.close(marker_r)
            os.close(marker_w)

    def test_pidfd_signal_error_does_not_erase_capture_failure(self):
        original_send = signal.pidfd_send_signal

        def send(fd, signum):
            if signum == signal.SIGTERM:
                raise PermissionError(errno.EPERM, "UNTRUSTED")
            return original_send(fd, signum)

        with patch.object(signal, "pidfd_send_signal", side_effect=send):
            value = self.capture(lambda: time.sleep(5))
        self.assertEqual(value["primary_failure"], "capture-deadline")
        self.assertEqual(value["cleanup_failure"], "pidfd-signal-failed")
        self.assert_reaped(value)
        self.assertNotIn("UNTRUSTED", json.dumps(value))

    def test_unsupported_parent_signal_or_thread_state_cannot_fork(self):
        for target, value in ((r.threading, 2), (r.signal, signal.SIG_IGN)):
            name = "active_count" if target is r.threading else "getsignal"
            with (
                patch.object(target, name, return_value=value),
                patch.object(os, "fork") as fork,
            ):
                result = r.Capture().run(-1)
            fork.assert_not_called()
            self.assertFalse(result["ok"])
            self.assertFalse(result["possible_unfinished_manager_operation"])

    def test_closed_reader_standard_fd_refuses_before_fork(self):
        original_stat = os.fstat

        def missing(fd):
            if fd == 1:
                raise OSError(errno.EBADF, "closed fixture")
            return original_stat(fd)

        with (
            patch.object(os, "fstat", side_effect=missing),
            patch.object(os, "fork") as fork,
        ):
            value = r.Capture().run(-1)
        fork.assert_not_called()
        self.assertEqual(value["primary_failure"], "reader-standard-fds-required")
        self.assertFalse(value["possible_unfinished_manager_operation"])

    def test_validated_artifact_is_hashed_but_never_executed_in_tests(self):
        path = (
            "/nix/store/"
            + "a" * 32
            + "-waddle-benign-resource-inspection-1/bin/benign-resource-inspection"
        )
        content = b"explicit non-executable test bytes"
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "artifact"
            fixture.write_bytes(content)
            fixture.chmod(0o444)
            original_open = os.open

            def open_fixture(candidate, flags):
                self.assertEqual(candidate, path)
                self.assertTrue(flags & os.O_NOFOLLOW)
                return original_open(fixture, flags)

            with (
                patch.object(r, "ARTIFACT", path),
                patch.object(r, "ARTIFACT_SHA256", hashlib.sha256(content).hexdigest()),
                patch.object(os, "open", side_effect=open_fixture),
            ):
                fd = r.fixed_artifact()
                self.assertEqual(os.fstat(fd).st_size, len(content))
                os.close(fd)
            with (
                patch.object(r, "ARTIFACT", path),
                patch.object(r, "ARTIFACT_SHA256", "0" * 64),
                patch.object(os, "open", side_effect=open_fixture),
                self.assertRaisesRegex(r.Rejected, "artifact-hash"),
            ):
                r.fixed_artifact()

    def test_timeout_sigterm_then_exact_pidfd_sigkill(self):
        def child():
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            time.sleep(5)

        value = self.capture(child)
        self.assertEqual(value["primary_failure"], "capture-deadline")
        self.assertEqual(value["signals"], ["SIGTERM", "SIGKILL"])
        self.assertEqual(value["exit_status"], -signal.SIGKILL)
        self.assertTrue(value["possible_unfinished_manager_operation"])
        self.assert_reaped(value)
        self.assertLess(value["capture_seconds"], 1)

    def test_overflow_drains_both_channels_without_blocking(self):
        def child():
            for _ in range(32):
                os.write(1, b"x" * 4096)
                os.write(2, b"y" * 4096)
            os._exit(0)

        value = self.capture(child)
        self.assertEqual(value["primary_failure"], "capture-overflow")
        self.assertLessEqual(
            len(base64.b64decode(value["stdout_base64"])), r.CHANNEL_CAP
        )
        self.assertLessEqual(
            len(base64.b64decode(value["stderr_base64"])), r.CHANNEL_CAP
        )
        self.assert_reaped(value)

    def test_both_fresh_pipes_can_fill_before_first_drain(self):
        marker_r, marker_w = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
        original_select = r.selectors.EpollSelector.select
        ready = False
        try:

            def child():
                for fd in (1, 2):
                    os.set_blocking(fd, False)
                    for _ in range(1024):
                        try:
                            os.write(fd, b"x" * 4096)
                        except BlockingIOError:
                            break
                    else:
                        os._exit(76)
                os.write(marker_w, b"F")
                os._exit(0)

            def delay_first_drain(selector, timeout):
                nonlocal ready
                if not ready:
                    deadline = time.monotonic() + 1
                    while not ready and time.monotonic() < deadline:
                        try:
                            ready = os.read(marker_r, 1) == b"F"
                        except BlockingIOError:
                            time.sleep(0.001)
                    self.assertTrue(ready, "fake child never filled both pipes")
                return original_select(selector, timeout)

            with patch.object(r.selectors.EpollSelector, "select", delay_first_drain):
                value = self.capture(child)
            self.assertEqual(value["primary_failure"], "capture-overflow")
            self.assert_reaped(value)
        finally:
            os.close(marker_r)
            os.close(marker_w)

    def test_concurrent_finite_stdout_stderr_never_deadlock(self):
        def child():
            os.write(2, b"x" * 4096)
            write_report()

        value = self.capture(child)
        self.assertEqual(value["primary_failure"], "report-stderr")
        self.assert_reaped(value)

    def test_early_exit_missing_report_and_closed_channels(self):
        for child in (
            lambda: os._exit(0),
            lambda: (os.close(1), os.close(2), os._exit(1)),
        ):
            value = self.capture(child)
            self.assertEqual(value["primary_failure"], "report-missing")
            self.assert_reaped(value)
            self.assertTrue(value["possible_unfinished_manager_operation"])

    def test_channel_close_with_live_child_is_bounded(self):
        def child():
            os.close(1)
            os.close(2)
            time.sleep(5)

        value = self.capture(child)
        self.assertEqual(value["primary_failure"], "capture-deadline")
        self.assert_reaped(value)

    def test_malformed_multiple_truncated_and_status_inconsistent(self):
        examples = [
            (b"{broken}\n", 0, "report-malformed"),
            (encode(success()) * 2, 0, "report-multiple"),
            (encode(success())[:-1], 0, "report-truncated"),
            (encode(success()), 1, "report-exit-status"),
            (
                encode({"ok": False, "check": "reporting-failed"}),
                74,
                "report-exit-status",
            ),
            (b'{"ok":false,"ok":false,"check":"x"}\n', 1, "report-duplicate-key"),
            (b"[]\n", 0, "report-schema"),
            (b"\xff\n", 0, "report-malformed"),
        ]
        for data, status, check in examples:
            with self.subTest(check=check):
                value = self.capture(
                    lambda data=data, status=status: (
                        os.write(1, data),
                        os._exit(status),
                    )
                )
                self.assertEqual(value["primary_failure"], check, value)
                self.assert_reaped(value)

    def test_schema_rejects_bad_types_extra_fields_and_reclamation_claims(self):
        cases = []
        for key, bad in (
            ("ok", 1),
            ("check", []),
            ("unit", "wrong"),
            ("quiescence_evidence", []),
            ("kernel_reclamation", "verified"),
            ("observed", []),
        ):
            value = success()
            value[key] = bad
            cases.append(value)
        for key in success()["observed"]:
            value = success()
            value["observed"][key] = None
            cases.append(value)
        value = success()
        value["extra"] = True
        cases.append(value)
        for value in cases:
            with self.assertRaises(r.Rejected):
                r.report(encode(value), b"", 0)

    def test_failure_preserves_child_primary_and_cleanup(self):
        original = {
            "ok": False,
            "check": "cleanup-unverified",
            "primary_failure": "field-cpuquota",
            "cleanup_failure": "cleanup-ownership-unknown",
            "userspace_quiesced": False,
            "kernel_reclamation": "unverified",
            "unit": success()["unit"],
        }
        value = self.capture(lambda: write_report(original, 1))
        self.assertEqual(value["primary_failure"], "guard-reported-failure")
        self.assertEqual(value["cleanup_failure"], "guard-cleanup-unverified")
        self.assertEqual(value["guard_report"], original)
        self.assertTrue(value["possible_unfinished_manager_operation"])
        self.assert_reaped(value)

    def test_interruption_after_pin_before_release_never_executes(self):
        capture = r.Capture()
        capture.interrupt(signal.SIGTERM, None)
        value = self.capture(capture=capture)
        self.assertEqual(value["primary_failure"], "reader-interrupted")
        self.assertFalse(value["possible_unfinished_manager_operation"])
        self.assertEqual(value["stdout_base64"], "")
        self.assert_reaped(value)

    def test_interruption_while_running_preserves_primary_during_cleanup(self):
        capture = r.Capture()
        original_select = r.selectors.EpollSelector.select
        calls = []

        def interrupt(selector, timeout):
            calls.append(1)
            if len(calls) == 2:
                capture.interrupt(signal.SIGINT, None)
            return original_select(selector, timeout)

        with patch.object(r.selectors.EpollSelector, "select", interrupt):
            value = self.capture(lambda: time.sleep(5), capture)
        self.assertEqual(value["primary_failure"], "reader-interrupted")
        self.assert_reaped(value)

    def test_pump_error_still_terminates_and_reaps_only_pinned_child(self):
        with patch.object(
            r.selectors.EpollSelector,
            "select",
            side_effect=OSError(errno.EBADF, "UNTRUSTED"),
        ):
            value = self.capture(lambda: time.sleep(5))
        self.assertEqual(value["primary_failure"], "reader-failed")
        self.assertTrue(value["parent_reaped"])
        self.assertEqual(value["cleanup_failure"], "reader-cleanup-unverified")
        self.assertNotIn("UNTRUSTED", json.dumps(value))

    def test_pin_failure_never_executes_signals_or_guesses_reaping(self):
        fork = os.fork
        children = []

        def record_fork():
            pid = fork()
            if pid:
                children.append(pid)
            return pid

        with (
            patch.object(os, "fork", side_effect=record_fork),
            patch.object(
                os, "pidfd_open", side_effect=OSError(errno.EMFILE, "UNTRUSTED")
            ),
            patch.object(signal, "pidfd_send_signal") as send,
            patch.object(os, "waitid") as wait,
        ):
            value = self.capture()
        send.assert_not_called()
        wait.assert_not_called()
        self.assertEqual(value["primary_failure"], "child-pin-failed-unreaped")
        self.assertFalse(value["parent_reaped"])
        self.assertFalse(value["possible_unfinished_manager_operation"])
        # Fixture owns this unreaped fake child; it exits on barrier EOF. This
        # explicit test teardown is not the production reader's cleanup path.
        self.assertEqual(len(children), 1)
        self.assertEqual(os.waitpid(children[0], 0)[0], children[0])


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.directory = self.stack.enter_context(tempfile.TemporaryDirectory())
        saved = os.open(".", os.O_RDONLY | os.O_DIRECTORY)
        self.stack.callback(os.close, saved)
        self.stack.callback(os.fchdir, saved)
        os.chdir(self.directory)
        self.root = r.evidence_root()
        self.stack.callback(os.close, self.root)
        self.name = "benign-resource-capture-" + "11" * 16
        self.stack.enter_context(patch.object(os, "urandom", return_value=b"\x11" * 16))

    def test_fresh_owned_private_bounded_evidence(self):
        value = {"ok": False, "stdout_base64": "eA==", "stderr_base64": ""}
        self.assertEqual(r.save_evidence(value, self.root), self.name)
        target = Path(self.name) / "evidence.json"
        self.assertEqual(stat.S_IMODE(Path(self.name).stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        self.assertEqual(target.stat().st_uid, os.getuid())
        self.assertEqual(json.loads(target.read_bytes()), value)
        with self.assertRaises(FileExistsError):
            r.save_evidence(value, self.root)
        self.assertEqual(json.loads(target.read_bytes()), value)

    def test_directory_symlink_or_existing_directory_never_reused(self):
        Path("target").mkdir()
        Path(self.name).symlink_to("target", target_is_directory=True)
        with self.assertRaises(FileExistsError):
            r.save_evidence({}, self.root)
        self.assertEqual(list(Path("target").iterdir()), [])
        Path(self.name).unlink()
        Path(self.name).mkdir()
        with self.assertRaises(FileExistsError):
            r.save_evidence({}, self.root)

    def test_injected_record_symlink_or_existing_file_never_overwritten(self):
        original_open = os.open
        Path("target").write_text("untouched")

        def inject(path, flags, mode=0o777, *, dir_fd=None):
            if path == "evidence.json":
                os.symlink(str(Path("target").absolute()), path, dir_fd=dir_fd)
            return original_open(path, flags, mode, dir_fd=dir_fd)

        with (
            patch.object(os, "open", side_effect=inject),
            self.assertRaises(FileExistsError),
        ):
            r.save_evidence({}, self.root)
        self.assertEqual(Path("target").read_text(), "untouched")

    def test_oversize_and_unsafe_root_rejected_before_creation(self):
        with self.assertRaisesRegex(r.Rejected, "evidence-size"):
            r.save_evidence({"data": "x" * r.EVIDENCE_CAP}, self.root)
        self.assertFalse(Path(self.name).exists())
        os.chmod(".", 0o777)
        with self.assertRaisesRegex(r.Rejected, "evidence-root"):
            r.save_evidence({}, self.root)
        self.assertFalse(Path(self.name).exists())

    def test_persistence_error_never_overwrites_or_deletes_uncertain_files(self):
        with (
            patch.object(os, "write", side_effect=OSError(errno.ENOSPC, "UNTRUSTED")),
            self.assertRaises(OSError),
        ):
            r.save_evidence(
                {
                    "primary_failure": "capture-deadline",
                    "cleanup_failure": "child-exit-unverified",
                },
                self.root,
            )
        target = Path(self.name) / "evidence.json"
        self.assertTrue(target.exists())
        self.assertEqual(target.read_bytes(), b"")
        with self.assertRaises(FileExistsError):
            r.save_evidence({}, self.root)

    def test_directory_replacement_is_detected_without_deleting_anything(self):
        original_fsync = os.fsync
        calls = []

        def replace(fd):
            original_fsync(fd)
            calls.append(fd)
            if len(calls) == 2:
                os.rename(self.name, "retained-original")
                Path(self.name).mkdir()

        with (
            patch.object(os, "fsync", side_effect=replace),
            self.assertRaisesRegex(r.Rejected, "evidence-directory-drift"),
        ):
            r.save_evidence({}, self.root)
        self.assertTrue((Path("retained-original") / "evidence.json").exists())
        self.assertEqual(list(Path(self.name).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
